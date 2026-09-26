// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Raster image extraction from PDF content streams and XObject resources.

use super::*;

pub(super) fn skip_ws(data: &[u8], j: &mut usize) {
    while *j < data.len() && (data[*j].is_ascii_whitespace() || data[*j] == 0) {
        *j += 1;
    }
}

pub(super) fn read_word<'a>(data: &'a [u8], j: &mut usize) -> &'a [u8] {
    let start = *j;
    while *j < data.len()
        && !(data[*j].is_ascii_whitespace()
            || data[*j] == 0
            || b"()<>[]{}/%".contains(&data[*j]))
    {
        *j += 1;
    }
    &data[start..*j]
}

pub(super) fn read_name(data: &[u8], j: &mut usize) -> Vec<u8> {
    skip_ws(data, j);
    if data.get(*j) == Some(&b'/') {
        *j += 1;
    }
    let start = *j;
    while *j < data.len()
        && (data[*j].is_ascii_alphanumeric()
            || data[*j] > 127
            || b"#*-+.".contains(&data[*j]))
    {
        *j += 1;
    }
    data[start..*j].to_vec()
}

/// Maximum nesting depth of an inline-image value (arrays inside arrays).
///
/// Real inline-image dictionaries are 1–2 levels deep; the cap only stops a
/// crafted sample of `[[[[…` from recursing until the process stack overflows.
pub(super) const MAX_INLINE_VALUE_DEPTH: usize = 32;

pub(super) fn read_inline_value(data: &[u8], j: &mut usize) -> Option<Object> {
    read_inline_value_at(data, j, 0)
}

fn read_inline_value_at(data: &[u8], j: &mut usize, depth: usize) -> Option<Object> {
    if depth >= MAX_INLINE_VALUE_DEPTH {
        return None;
    }
    skip_ws(data, j);
    if *j >= data.len() {
        return None;
    }
    match data[*j] {
        b'/' => Some(Object::Name(read_name(data, j))),
        b'0'..=b'9' | b'+' | b'-' | b'.' => {
            let start = *j;
            let mut has_digit = false;
            while *j < data.len()
                && (data[*j].is_ascii_digit()
                    || matches!(data[*j], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                has_digit |= data[*j].is_ascii_digit();
                *j += 1;
            }
            if !has_digit {
                return None;
            }
            let s = std::str::from_utf8(&data[start..*j]).ok()?;
            let f: f64 = s.parse().ok()?;
            if f.fract() == 0.0 && f.abs() < 1e15 {
                Some(Object::Integer(f as i64))
            } else {
                Some(Object::Real(f as f32))
            }
        }
        b'[' => {
            *j += 1;
            let mut arr = Vec::new();
            loop {
                skip_ws(data, j);
                if data.get(*j) == Some(&b']') {
                    *j += 1;
                    break;
                }
                if *j >= data.len() {
                    break;
                }
                if let Some(v) = read_inline_value_at(data, j, depth + 1) {
                    arr.push(v);
                } else {
                    *j += 1;
                }
            }
            Some(Object::Array(arr))
        }
        b't' => {
            *j += 1;
            Some(Object::Boolean(true))
        }
        b'f' => {
            *j += 1;
            Some(Object::Boolean(false))
        }
        _ => {
            *j += 1;
            None
        }
    }
}

/// Compute the byte span (from `j`) of one inline image's samples and push a
/// normalized stream if it is a valid, decodable image. Advances `j` past the
/// `EI` operator.
pub(super) fn parse_inline_image(
    data: &[u8],
    j: &mut usize,
    ctm: Mtx,
    name: Vec<u8>,
) -> Option<Placement> {
    let n = data.len();
    // Parse the inline-image dictionary until the `ID` keyword.
    let mut dict: Vec<(Vec<u8>, Object)> = Vec::new();
    loop {
        skip_ws(data, j);
        if data.get(*j) != Some(&b'/') {
            let w = read_word(data, j);
            if w == b"ID" {
                break;
            }
            if w.is_empty() {
                return None;
            }
            continue;
        }
        let key = read_name(data, j);
        if let Some(v) = read_inline_value(data, j) {
            dict.push((key, v));
        }
    }
    let get = |k: &[u8]| -> Option<&Object> {
        dict.iter().find(|(kw, _)| kw == k).map(|(_, v)| v)
    };
    let width = get(b"W")
        .or_else(|| get(b"Width"))?
        .as_i64()
        .ok()? as u32;
    let height = get(b"H")
        .or_else(|| get(b"Height"))?
        .as_i64()
        .ok()? as u32;
    if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
        return None;
    }
    // Reject an oversized declaration before any inflated sample or RGBA buffer
    // is allocated; the pixel count is the true memory driver, not the box.
    if width as usize * height as usize > crate::media::MAX_IMAGE_PIXELS {
        return None;
    }
    let bpc = get(b"BPC")
        .or_else(|| get(b"BitsPerComponent"))
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(8);
    // Keep the unfiltered sample-span arithmetic below from overflowing on a
    // crafted `BitsPerComponent`; the decoder only ever consumes 1/8-bit.
    if !(1..=16).contains(&bpc) {
        return None;
    }
    let cs = get(b"CS")
        .or_else(|| get(b"ColorSpace"))
        .cloned()
        .unwrap_or_else(|| Object::Name(b"DeviceGray".to_vec()));
    let comps = match &cs {
        Object::Name(nm) => match nm.as_slice() {
            b"G" | b"Gray" | b"DeviceGray" => 1u32,
            b"RGB" | b"DeviceRGB" => 3,
            b"CMYK" | b"DeviceCMYK" => 4,
            b"RGBA" | b"DeviceRGBA" => 4,
            _ => return None,
        },
        _ => return None,
    };
    let filter = get(b"F").or_else(|| get(b"Filter")).cloned();
    let im = get(b"IM").or_else(|| get(b"ImageMask")).cloned();

    skip_ws(data, j);
    let start = *j;
    let data_len = match &filter {
        None => {
            let stride = (width as usize * (comps as usize * bpc as usize)).div_ceil(8);
            height as usize * stride
        }
        Some(_) => {
            // Filtered: samples run to the whitespace-delimited `EI`. Inflate
            // (or decompress) the whole chunk; filters tolerate trailing bytes.
            let mut k = start;
            while k < n {
                if data[k] == b'E' && data.get(k - 1).map_or(false, |&c| c.is_ascii_whitespace()) {
                    if data.get(k..k + 2).map_or(false, |w| w == b"EI")
                        && data.get(k + 2).map_or(false, |&c| c.is_ascii_whitespace())
                    {
                        break;
                    }
                }
                k += 1;
            }
            (k - start).max(1)
        }
    };
    if start + data_len > n {
        return None;
    }
    let mut content = data[start..start + data_len].to_vec();
    if let Some(f) = &filter {
        let fv = f.as_name().ok().map(|n| n.to_vec())?;
        match fv.as_slice() {
            b"FlateDecode" | b"Fl" => content = inflate(&content, crate::media::MAX_IMAGE_SAMPLES)?,
            b"RunLengthDecode" | b"RL" => content = decode_runlength(&content)?,
            b"ASCIIHexDecode" | b"AHx" => content = decode_asciihex(&content)?,
            b"ASCII85Decode" | b"A85" => content = decode_ascii85(&content)?,
            // DCTDecode / JPXDecode are JPEG: keep the samples for passthrough.
            b"DCTDecode" | b"DCT" | b"JPXDecode" | b"J" => {}
            _ => return None,
        }
    }
    *j = start + data_len;
    skip_ws(data, j); // to the `EI` operator
    read_word(data, j); // consume `EI`

    let mut d = lopdf::Dictionary::new();
    d.set(b"Width", width as i64);
    d.set(b"Height", height as i64);
    d.set(b"BitsPerComponent", bpc);
    d.set(b"ColorSpace", expand_color_space(&cs));
    if let Some(f) = &filter {
        if let Ok(fn_) = f.as_name() {
            if matches!(fn_, b"DCTDecode" | b"DCT" | b"JPXDecode" | b"J") {
                d.set(b"Filter", Object::Name(b"DCTDecode".to_vec()));
            }
        }
    }
    if let Some(im) = &im {
        d.set(b"ImageMask", im.clone());
    }
    let (x0, y0, x1, y1) = placement_bbox(&ctm);
    Some(Placement {
        name,
        obj_id: None,
        obj: Object::Stream(lopdf::Stream::new(d, content)),
        x0,
        y0,
        x1,
        y1,
    })
}

/// Re-scan decoded content bytes for `BI ... ID <data> EI` inline images,
/// tracking the graphics-state matrix through `q`/`Q`/`cm` so each computed
/// placement lands at its on-page box.
pub(crate) fn scan_inline_images(data: &[u8], init_ctm: Mtx) -> Vec<Placement> {
    let mut out: Vec<Placement> = Vec::new();
    let mut ctm = init_ctm;
    let mut stack: Vec<Mtx> = Vec::new();
    let mut pending: Vec<f64> = Vec::new();
    let mut i = 0usize;
    let n = data.len();
    while i < n {
        let b = data[i];
        if b.is_ascii_whitespace() || b == 0 {
            i += 1;
        } else if b == b'%' {
            while i < n && data[i] != b'\n' {
                i += 1;
            }
        } else if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' {
            let start = i;
            let mut has_digit = false;
            while i < n
                && (data[i].is_ascii_digit()
                    || matches!(data[i], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                has_digit |= data[i].is_ascii_digit();
                i += 1;
            }
            if has_digit {
                if let Ok(s) = std::str::from_utf8(&data[start..i]) {
                    if let Ok(v) = s.parse::<f64>() {
                        pending.push(v);
                    }
                }
            }
        } else if b == b'/' {
            i += 1;
            while i < n
                && (data[i].is_ascii_alphanumeric()
                    || data[i] > 127
                    || b"#*-+.".contains(&data[i]))
            {
                i += 1;
            }
            pending.clear();
        } else if b == b'(' {
            let mut depth = 1usize;
            i += 1;
            while i < n && depth > 0 {
                if data[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if data[i] == b'(' {
                    depth += 1;
                } else if data[i] == b')' {
                    depth -= 1;
                }
                i += 1;
            }
            pending.clear();
        } else {
            // Operator or array/dict start.
            if b == b'[' {
                let mut depth = 1usize;
                i += 1;
                while i < n && depth > 0 {
                    match data[i] {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                pending.clear();
            } else if data.get(i..i + 2) == Some(b"<<") {
                let mut depth = 1usize;
                i += 2;
                while i < n && depth > 0 {
                    if data.get(i..i + 2) == Some(b"<<") {
                        depth += 1;
                        i += 2;
                        continue;
                    }
                    if data.get(i..i + 2) == Some(b">>") {
                        depth -= 1;
                        i += 2;
                        continue;
                    }
                    i += 1;
                }
                pending.clear();
            } else if b == b'<' {
                while i < n && data[i] != b'>' {
                    i += 1;
                }
                i += 1;
                pending.clear();
            } else {
                let op = read_word(data, &mut i);
                if op.is_empty() {
                    // Stray delimiter: guarantee forward progress.
                    i += 1;
                    pending.clear();
                    continue;
                }
                match op {
                    b"q" => {
                        stack.push(ctm);
                        pending.clear();
                    }
                    b"Q" => {
                        if let Some(m) = stack.pop() {
                            ctm = m;
                        }
                        pending.clear();
                    }
                    b"cm" => {
                        if pending.len() >= 6 {
                            ctm.pre_mul(&Mtx::from_parts(
                                pending[0], pending[1], pending[2], pending[3], pending[4],
                                pending[5],
                            ));
                        }
                        pending.clear();
                    }
                    b"BI" | b"INLINEIMAGE" => {
                        let name = format!("inline_{}", out.len());
                        if let Some(p) = parse_inline_image(data, &mut i, ctm, name.into_bytes()) {
                            out.push(p);
                        }
                        pending.clear();
                    }
                    _ => pending.clear(),
                }
            }
        }
    }
    out
}
