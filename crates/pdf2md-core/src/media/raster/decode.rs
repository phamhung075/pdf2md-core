// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Raster image extraction from PDF content streams and XObject resources.

use super::*;

/// Decode an XObject stream into encoded bytes (JPEG passthrough or PNG),
/// plus the actually-encoded pixel width/height. Non-JPEG rasters are
/// downscaled to fit `max_image_dimension` (pass `u32::MAX` to disable)
/// before PNG encoding; JPEG streams pass through unchanged since re-encoding
/// them needs a JPEG codec this crate does not link outside the `vision`
/// feature.
pub fn decode_xobject_bytes(
    doc: &Document,
    xobj: &Object,
    max_image_dimension: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let Object::Stream(s) = xobj else {
        return None;
    };
    let (width, height) = stream_width_height(s)?;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return None;
    }
    // Reject an oversized declaration before any inflated sample or RGBA buffer
    // is allocated; the pixel count is the true memory driver, not the box.
    // 64-bit product so the guard cannot wrap on 32-bit wasm32.
    if (width as u64) * (height as u64) > crate::media::MAX_IMAGE_PIXELS as u64 {
        return None;
    }
    let filters = stream_filters(s);
    let mut data = s.content.clone();
    let mut is_jpeg = false;
    for f in &filters {
        match f.as_slice() {
            b"ASCIIHexDecode" => data = decode_asciihex(&data)?,
            b"ASCII85Decode" => data = decode_ascii85(&data)?,
            b"FlateDecode" => data = inflate(&data, crate::media::MAX_IMAGE_SAMPLES)?,
            b"RunLengthDecode" => data = decode_runlength(&data)?,
            b"DCTDecode" | b"JPXDecode" => {
                is_jpeg = true;
                break;
            }
            _ => return None,
        }
    }
    if is_jpeg {
        return Some((data, width, height));
    }
    // PNG-style predictors (DecodeParms Predictor 10..=15) are common on
    // Flate image streams: undo the per-row filtering before interpreting
    // samples. lopdf's own png module does exactly the PDF predictor pass.
    let data = match s.dict.get(b"DecodeParms").ok() {
        Some(Object::Dictionary(dp)) => {
            let predictor = dp
                .get(b"Predictor")
                .ok()
                .and_then(|o| o.as_i64().ok())
                .unwrap_or(1);
            if (10..=15).contains(&predictor) {
                let columns = dp
                    .get(b"Columns")
                    .ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(width as i64) as usize;
                let colors = dp
                    .get(b"Colors")
                    .ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(1) as usize;
                let bpc = dp
                    .get(b"BitsPerComponent")
                    .ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(8) as usize;
                let bpp = (colors * bpc).div_ceil(8).max(1);
                lopdf::filters::png::decode_frame(&data, bpp, columns).ok()?
            } else {
                data
            }
        }
        _ => data,
    };
    let bits = s
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(8);
    let mask = s
        .dict
        .get(b"ImageMask")
        .ok()
        .and_then(|o| o.as_bool().ok())
        .unwrap_or(false);
    if mask || bits == 0 {
        return None;
    }
    let cs = match s.dict.get(b"ColorSpace").ok() {
        Some(o) => deref_obj(doc, o).ok().cloned().unwrap_or(o.clone()),
        None => Object::Name(b"DeviceRGB".to_vec()),
    };
    let rgba = raster_to_rgba(doc, width, height, bits as u32, &cs, &data)?;
    let (rgba, out_w, out_h) = downscale_rgba(&rgba, width, height, max_image_dimension);
    Some((encode_png_rgba(out_w, out_h, &rgba), out_w, out_h))
}

pub fn stream_width_height(s: &lopdf::Stream) -> Option<(u32, u32)> {
    let w = s.dict.get(b"Width").ok()?.as_i64().ok()? as u32;
    let h = s.dict.get(b"Height").ok()?.as_i64().ok()? as u32;
    Some((w, h))
}

pub fn stream_filters(s: &lopdf::Stream) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Ok(f) = s.dict.get(b"Filter") else {
        return out;
    };
    match f {
        Object::Name(n) => out.push(n.clone()),
        Object::Array(a) => {
            for o in a {
                if let Ok(n) = o.as_name() {
                    out.push(n.to_vec());
                }
            }
        }
        _ => {}
    }
    out
}

/// Expand abbreviated PDF color-space names (`/CS /RGB` style, common on inline
/// images) to the full names the decoders in this module recognize.
pub(crate) fn expand_color_space(cs: &Object) -> Object {
    let expand_name = |n: &[u8]| -> Vec<u8> {
        match n {
            b"G" | b"Gray" => b"DeviceGray".to_vec(),
            b"RGB" => b"DeviceRGB".to_vec(),
            b"CMYK" => b"DeviceCMYK".to_vec(),
            other => other.to_vec(),
        }
    };
    match cs {
        Object::Name(n) => Object::Name(expand_name(n)),
        Object::Array(a) => {
            let mut out = a.clone();
            if let Some(first) = out.first_mut() {
                if let Object::Name(n) = first {
                    *first = Object::Name(expand_name(n));
                }
            }
            Object::Array(out)
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Inline image recovery from the raw content stream.
//
// lopdf drops `BI ... ID <data> EI` inline images whose color space is
// abbreviated (/CS /G, /CS /RGB) or whose samples are filtered, so placed
// inline barcodes / stamps are silently lost. We re-scan the decoded content
// bytes, tracking the graphics-state matrix through `q`/`Q`/`cm`, and surface
// each rendered inline image as a normal `Placement` so it flows through the
// same probe / classify / decode pipeline.
// ---------------------------------------------------------------------------

pub fn raster_to_rgba(doc: &Document, w: u32, h: u32, bits: u32, cs: &Object, data: &[u8]) -> Option<Vec<u8>> {
    // Allocation guard for direct callers too: `pixel × 4` RGBA is the big
    // buffer, so refuse an oversized raster before computing `n`. The product is
    // 64-bit so the guard cannot wrap on 32-bit wasm32.
    if (w as u64) * (h as u64) > crate::media::MAX_IMAGE_PIXELS as u64 {
        return None;
    }
    let n = (w as u64 * h as u64) as usize;
    if bits == 1 {
        let row_bytes = (w as usize + 7) / 8;
        let need = row_bytes.checked_mul(h as usize)?;
        let src = data.get(..need)?;
        let rgba_cap = n.checked_mul(4)?;
        let mut rgba = Vec::with_capacity(rgba_cap);
        for r in 0..h as usize {
            for c in 0..w as usize {
                let byte = src[r * row_bytes + c / 8];
                let bit = (byte >> (7 - (c % 8))) & 1;
                let v = if bit == 1 { 0u8 } else { 255u8 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        return Some(rgba);
    }
    if bits != 8 {
        return None;
    }
    let comps = match cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" => 1u32,
            b"DeviceRGB" | b"CalRGB" => 3,
            b"DeviceCMYK" => 4,
            _ => return None,
        },
        Object::Array(a) => {
            let Some(first) = a.first().and_then(|o| o.as_name().ok()) else {
                return None;
            };
            match first {
                b"DeviceGray" | b"CalGray" => 1,
                b"DeviceRGB" | b"CalRGB" => 3,
                b"Indexed" => 1,
                b"ICCBased" => 3,
                _ => return None,
            }
        }
        _ => return None,
    };

    if let Object::Array(a) = cs {
        if a.first().and_then(|o| o.as_name().ok()) == Some(b"Indexed") {
            let pal = resolve_indexed_palette(doc, a)?;
            let src = data.get(..n)?;
            let mut rgba = Vec::with_capacity(n * 4);
            for &idx in src {
                let i = idx as usize * 3;
                if i + 3 <= pal.len() {
                    rgba.extend_from_slice(&[pal[i], pal[i + 1], pal[i + 2], 255]);
                } else {
                    rgba.extend_from_slice(&[255, 255, 255, 255]);
                }
            }
            return Some(rgba);
        }
    }

    let need = n * comps as usize;
    let src = data.get(..need)?;
    let mut rgba = Vec::with_capacity(n * 4);
    match comps {
        1 => {
            for &g in src {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
        3 => {
            for p in src.chunks_exact(3) {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        4 => {
            for p in src.chunks_exact(4) {
                let k = p[3] as f32 / 255.0;
                let conv = |v: u8| {
                    let x = (255.0 * (1.0 - v as f32 / 255.0) * (1.0 - k))
                        .round()
                        .clamp(0.0, 255.0);
                    x as u8
                };
                rgba.extend_from_slice(&[conv(p[0]), conv(p[1]), conv(p[2]), 255]);
            }
        }
        _ => return None,
    }
    Some(rgba)
}

pub fn resolve_indexed_palette(doc: &Document, a: &[Object]) -> Option<Vec<u8>> {
    let lookup = a.get(3)?;
    let o = deref_obj(doc, lookup).ok()?;
    match o {
        Object::String(b, _) => Some(b.clone()),
        Object::Stream(st) => st.decompressed_content_with_limit(16 << 20).ok(),
        _ => None,
    }
}
