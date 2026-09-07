// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Raster image extraction from PDF content streams and XObject resources.

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashMap;

use crate::layout::Mtx;
use crate::media::codecs::{
    b64encode, decode_ascii85, decode_asciihex, decode_runlength, encode_png_rgba, inflate,
};
use crate::media::{MediaItem, MediaKind};

pub(crate) const BG_AREA_FRAC: f64 = 0.82;
pub(crate) const TILE_PT: f64 = 7.0;

pub(crate) fn obj_ref(o: &Object) -> Option<ObjectId> {
    match o {
        Object::Reference(id) => Some(*id),
        _ => None,
    }
}

pub(crate) fn deref_obj<'a>(doc: &'a Document, obj: &'a Object) -> Result<&'a Object, lopdf::Error> {
    let mut cur = obj;
    for _ in 0..32 {
        match cur {
            Object::Reference(id) => cur = doc.get_object(*id)?,
            _ => return Ok(cur),
        }
    }
    Ok(cur)
}

pub(crate) fn page_xobjects(doc: &Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut cur_id = Some(page_id);
    let mut guard = 0;
    while let Some(pid) = cur_id {
        guard += 1;
        if guard > 64 {
            return None;
        }
        let Ok(page) = doc.get_dictionary(pid) else {
            return None;
        };
        if let Some(xo) = page
            .get(b"Resources")
            .ok()
            .and_then(|o| deref_obj(doc, o).ok())
            .and_then(|r| r.as_dict().ok())
            .and_then(|d| d.get(b"XObject").ok())
            .and_then(|o| deref_obj(doc, o).ok())
            .and_then(|o| o.as_dict().ok())
        {
            return Some(xo.clone());
        }
        cur_id = page.get(b"Parent").ok().and_then(|o| obj_ref(o));
    }
    None
}

pub(crate) struct Placement {
    pub name: Vec<u8>,
    pub obj_id: Option<ObjectId>,
    pub obj: Object,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// Extract every image placement on one page.
pub fn extract_page_media(
    doc: &Document,
    page_id: ObjectId,
    page_num: usize,
    page_bbox: Option<(f64, f64, f64, f64)>,
) -> Vec<MediaItem> {
    let Some(xobjects) = page_xobjects(doc, page_id) else {
        return Vec::new();
    };
    let Ok(content) = doc.get_and_decode_page_content(page_id) else {
        return Vec::new();
    };
    let mut placements: Vec<Placement> = Vec::new();
    collect_page_ops(doc, &xobjects, &content, &mut placements, 0, &Mtx::ID);

    // Group by XObject (by obj_id if present, else name), merging placement bboxes and counting repeats.
    let mut groups: Vec<(Placement, u32)> = Vec::new();
    for p in placements {
        match groups.iter_mut().find(|(g, _)| {
            if let (Some(gid), Some(pid)) = (g.obj_id, p.obj_id) {
                gid == pid
            } else {
                g.name == p.name
            }
        }) {
            Some((g, cnt)) => {
                *cnt += 1;
                g.x0 = g.x0.min(p.x0);
                g.y0 = g.y0.min(p.y0);
                g.x1 = g.x1.max(p.x1);
                g.y1 = g.y1.max(p.y1);
            }
            None => groups.push((p, 1)),
        }
    }

    let mut out: Vec<MediaItem> = Vec::new();

    // Phase 1 — classify from geometry + declared pixel dims only (cheap),
    // so full-page scan backgrounds and tiny tiles never get inflated.
    struct Want {
        p: Placement,
        cnt: u32,
        w: u32,
        h: u32,
        fmt: String,
    }
    let mut wants: Vec<Want> = Vec::new();
    for (p, cnt) in groups {
        let Some((w, h, fmt)) = probe_stream(&p.obj) else {
            continue;
        };
        let (kind, decorative) = classify_geometry(p.x0, p.y0, p.x1, p.y1, page_bbox);
        // Pixel-tiny sources (2x2 pattern tiles) are decoration regardless of
        // how they are scaled on the page.
        let decorative = decorative || (w * h) <= 64;
        if decorative || kind == MediaKind::Decorative {
            // Report the placement but without bytes.
            out.push(MediaItem {
                page: page_num,
                x0: p.x0,
                y0: p.y0,
                x1: p.x1,
                y1: p.y1,
                width: w,
                height: h,
                format: fmt.clone(),
                kind,
                decorative: true,
                repeat: cnt,
                data: Vec::new(),
                data_b64: String::new(),
            });
        } else {
            wants.push(Want { p, cnt, w, h, fmt });
        }
    }

    // Phase 2 — decode each surviving object once (cache by obj_id or name).
    #[derive(Hash, PartialEq, Eq, Clone)]
    enum Key {
        Id(ObjectId),
        Name(Vec<u8>),
    }
    let mut cache: HashMap<Key, Option<Vec<u8>>> = HashMap::new();
    for want in wants {
        let key = want
            .p
            .obj_id
            .map(Key::Id)
            .unwrap_or_else(|| Key::Name(want.p.name.clone()));
        let bytes = match cache.get(&key) {
            Some(d) => d.clone(),
            None => {
                let d = decode_xobject_bytes(doc, &want.p.obj);
                cache.insert(key, d.clone());
                d
            }
        };
        let (kind, decorative) = classify_geometry(
            want.p.x0, want.p.y0, want.p.x1, want.p.y1, page_bbox,
        );
        if decorative {
            continue;
        }
        match bytes {
            Some(data) => {
                out.push(MediaItem {
                    page: page_num,
                    x0: want.p.x0,
                    y0: want.p.y0,
                    x1: want.p.x1,
                    y1: want.p.y1,
                    width: want.w,
                    height: want.h,
                    format: want.fmt.clone(),
                    kind,
                    decorative: false,
                    repeat: want.cnt,
                    data: data.clone(),
                    data_b64: b64encode(&data),
                });
            }
            None => {
                out.push(MediaItem {
                    page: page_num,
                    x0: want.p.x0,
                    y0: want.p.y0,
                    x1: want.p.x1,
                    y1: want.p.y1,
                    width: want.w,
                    height: want.h,
                    format: want.fmt.clone(),
                    kind,
                    decorative: false,
                    repeat: want.cnt,
                    data: Vec::new(),
                    data_b64: String::new(),
                });
            }
        }
    }
    out
}

/// Walk one content stream tracking the CTM; record image `Do` placements and
/// descend into Form XObjects (depth-limited).
pub(crate) fn collect_page_ops(
    doc: &Document,
    xobjects: &Dictionary,
    content: &Content<Vec<Operation>>,
    out: &mut Vec<Placement>,
    depth: usize,
    ctm_in: &Mtx,
) {
    if depth > 5 {
        return;
    }
    let mut ctm = *ctm_in;
    let mut stack: Vec<Mtx> = Vec::new();
    for op in &content.operations {
        match op.operator.as_str() {
            "q" => stack.push(ctm),
            "Q" => {
                if let Some(m) = stack.pop() {
                    ctm = m;
                }
            }
            "cm" => {
                if let Some(m) = crate::layout::mtx_from(op) {
                    ctm.pre_mul(&m);
                }
            }
            "Do" => {
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some(raw_obj) = xobjects.get(name).ok() else {
                    continue;
                };
                let obj_id = match raw_obj {
                    Object::Reference(id) => Some(*id),
                    _ => None,
                };
                let Some(xobj) = deref_obj(doc, raw_obj).ok() else {
                    continue;
                };
                let Object::Stream(s) = xobj else {
                    continue;
                };
                let subtype = s
                    .dict
                    .get(b"Subtype")
                    .ok()
                    .and_then(|o| o.as_name().ok())
                    .unwrap_or_default();
                if subtype == b"Image" {
                    let (x0, y0, x1, y1) = placement_bbox(&ctm);
                    out.push(Placement {
                        name: name.to_vec(),
                        obj_id,
                        obj: xobj.clone(),
                        x0,
                        y0,
                        x1,
                        y1,
                    });
                } else if subtype == b"Form" {
                    let mut form_ctm = ctm;
                    if let Ok(m) = s.dict.get(b"Matrix") {
                        if let Ok(arr) = m.as_array() {
                            form_ctm.pre_mul(&mtx_from_slice(arr));
                        }
                    }
                    let sub_xo: Dictionary = s
                        .dict
                        .get(b"Resources")
                        .ok()
                        .and_then(|o| deref_obj(doc, o).ok())
                        .and_then(|o| o.as_dict().ok())
                        .and_then(|d| d.get(b"XObject").ok())
                        .and_then(|o| deref_obj(doc, o).ok())
                        .and_then(|o| o.as_dict().ok())
                        .cloned()
                        .unwrap_or_else(|| xobjects.clone());
                    if let Ok(b) = s.decompressed_content_with_limit(64 << 20) {
                        if let Ok(ops) = Content::<Vec<Operation>>::decode(&b) {
                            collect_page_ops(doc, &sub_xo, &ops, out, depth + 1, &form_ctm);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn mtx_from_slice(a: &[Object]) -> Mtx {
    let g = |i: usize| crate::layout::num(a.get(i).unwrap_or(&Object::Null)).unwrap_or(0.0);
    crate::layout::Mtx::from_parts(g(0), g(1), g(2), g(3), g(4), g(5))
}

pub(crate) fn placement_bbox(ctm: &Mtx) -> (f64, f64, f64, f64) {
    let corners = [
        ctm.apply(0.0, 0.0),
        ctm.apply(1.0, 0.0),
        ctm.apply(0.0, 1.0),
        ctm.apply(1.0, 1.0),
    ];
    let mut x0 = f64::INFINITY;
    let mut y0 = f64::INFINITY;
    let mut x1 = f64::NEG_INFINITY;
    let mut y1 = f64::NEG_INFINITY;
    for (x, y) in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    (x0, y0, x1, y1)
}

/// Cheap probe: dimensions + format, without decoding the payload.
pub fn probe_stream(xobj: &Object) -> Option<(u32, u32, String)> {
    let Object::Stream(s) = xobj else {
        return None;
    };
    let (width, height) = stream_width_height(s)?;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return None;
    }
    let filters = stream_filters(s);
    if filters.iter().any(|f| f == b"DCTDecode" || f == b"JPXDecode") {
        Some((width, height, "image/jpeg".into()))
    } else {
        Some((width, height, "image/png".into()))
    }
}

/// Decode an XObject stream into encoded bytes (JPEG passthrough or PNG).
pub fn decode_xobject_bytes(doc: &Document, xobj: &Object) -> Option<Vec<u8>> {
    let Object::Stream(s) = xobj else {
        return None;
    };
    let (width, height) = stream_width_height(s)?;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return None;
    }
    let filters = stream_filters(s);
    let mut data = s.content.clone();
    let mut is_jpeg = false;
    for f in &filters {
        match f.as_slice() {
            b"ASCIIHexDecode" => data = decode_asciihex(&data)?,
            b"ASCII85Decode" => data = decode_ascii85(&data)?,
            b"FlateDecode" => data = inflate(&data)?,
            b"RunLengthDecode" => data = decode_runlength(&data)?,
            b"DCTDecode" | b"JPXDecode" => {
                is_jpeg = true;
                break;
            }
            _ => return None,
        }
    }
    if is_jpeg {
        return Some(data);
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
    Some(encode_png_rgba(width, height, &rgba))
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

pub fn raster_to_rgba(doc: &Document, w: u32, h: u32, bits: u32, cs: &Object, data: &[u8]) -> Option<Vec<u8>> {
    let n = w as usize * h as usize;
    if bits == 1 {
        let row_bytes = (w as usize + 7) / 8;
        let need = row_bytes * h as usize;
        let src = data.get(..need)?;
        let mut rgba = Vec::with_capacity(n * 4);
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

/// Pure geometry-based role classification.
pub fn classify_geometry(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    page_bbox: Option<(f64, f64, f64, f64)>,
) -> (MediaKind, bool) {
    let w = (x1 - x0).abs().max(1e-3);
    let h = (y1 - y0).abs().max(1e-3);
    let area = w * h;

    if let Some((px0, py0, px1, py1)) = page_bbox {
        let parea = ((px1 - px0) * (py1 - py0)).max(1.0);
        let near_full = area >= parea * BG_AREA_FRAC
            && (x0 - px0).abs() < 30.0
            && (px1 - x1).abs() < 30.0
            && (y0 - py0).abs() < 30.0
            && (py1 - y1).abs() < 30.0;
        if near_full {
            return (MediaKind::Decorative, true);
        }
    }
    if w < TILE_PT && h < TILE_PT {
        return (MediaKind::Decorative, true);
    }

    let aspect = w / h;
    let frac_w = page_bbox.map_or(1.0, |(px0, _, px1, _)| w / (px1 - px0).max(1.0));
    // PDF device y grows upward: the visual bottom of the page is the LOW y
    // edge. A signature is a small ink-dense scan near that bottom edge.
    let bottom_frac = page_bbox.map_or(0.0, |(_, py0, _, py1)| {
        let low = py0.min(py1);
        let high = py0.max(py1);
        ((y0.min(y1)) - low) / (high - low).max(1.0)
    });

    if aspect > 5.0 || aspect < 0.2 {
        return (MediaKind::Barcode, false);
    }
    if bottom_frac < 0.22 && area < 25_000.0 && aspect >= 0.3 && aspect <= 4.0 && frac_w < 0.35 {
        return (MediaKind::Signature, false);
    }
    if area < 20_000.0 && aspect >= 1.1 && h < 180.0 {
        return (MediaKind::Logo, false);
    }
    if aspect >= 0.6 && aspect <= 2.6 && frac_w >= 0.2 && area < 900_000.0 {
        return (MediaKind::Chart, false);
    }
    (MediaKind::Photo, false)
}
