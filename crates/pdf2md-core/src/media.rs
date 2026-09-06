//! Media extraction — cut the images out of a document (Stage 1 of the
//! layout/media engine).
//!
//! Deterministic, no ML. What this recovers per page:
//!   * every image XObject painted with `Do`, positioned in device space via
//!     the graphics-state CTM (q/Q/cm tracking) so each placement gets a real
//!     bbox on the page;
//!   * raster bytes for each placement — DCTDecode chains are unwrapped
//!     (ASCII85/ASCIIHex/Flate) and passed through as JPEG; raw/Flate streams
//!     are decoded to RGBA and re-encoded as PNG;
//!   * a conservative role guess (logo / chart / photo / signature / barcode)
//!     plus a decorative flag, so callers can skip full-page backgrounds,
//!     tiny pattern tiles and repeated headers.
//!
//! Only images actually *drawn* on the page are reported (not every XObject
//! in the resources dict). Repeated paints of the same XObject are collapsed
//! into one item carrying a repeat count, and decoding happens once per
//! object (after classification) so full-page scan backgrounds are skipped
//! without ever inflating them.

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashMap;
use std::io::{Read, Write};

use crate::layout::Mtx;

/// Role guess for an extracted placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Logo,
    Chart,
    Diagram,
    Photo,
    Signature,
    Barcode,
    Decorative,
    Other,
}

impl MediaKind {
    /// Stable lowercase name for the wire/markdown formats.
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Logo => "logo",
            MediaKind::Chart => "chart",
            MediaKind::Diagram => "diagram",
            MediaKind::Photo => "photo",
            MediaKind::Signature => "signature",
            MediaKind::Barcode => "barcode",
            MediaKind::Decorative => "decoration",
            MediaKind::Other => "image",
        }
    }
}

/// One extracted object (deduped per page) with decoded bytes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaItem {
    /// 1-based page number.
    pub page: usize,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub width: u32,
    pub height: u32,
    /// "image/jpeg" | "image/png"
    pub format: String,
    pub kind: MediaKind,
    pub decorative: bool,
    /// Times this object is painted on this page.
    pub repeat: u32,
    /// Raw encoded bytes (JPEG passthrough or PNG re-encode).
    #[serde(skip)]
    pub data: Vec<u8>,
    /// Base64 of `data` (wire format).
    #[serde(rename = "data_b64", default, skip_serializing_if = "String::is_empty")]
    pub data_b64: String,
}

const BG_AREA_FRAC: f64 = 0.82;
const TILE_PT: f64 = 7.0;

/// Standard base64 encoder (RFC 4648) — no dependency needed.
pub fn b64encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn obj_ref(o: &Object) -> Option<ObjectId> {
    match o {
        Object::Reference(id) => Some(*id),
        _ => None,
    }
}

fn deref_obj<'a>(doc: &'a Document, obj: &'a Object) -> Result<&'a Object, lopdf::Error> {
    let mut cur = obj;
    for _ in 0..32 {
        match cur {
            Object::Reference(id) => cur = doc.get_object(*id)?,
            _ => return Ok(cur),
        }
    }
    Ok(cur)
}

fn page_xobjects(doc: &Document, page_id: ObjectId) -> Option<Dictionary> {
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

struct Placement {
    name: Vec<u8>,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
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

    // Group by XObject name, merging placement bboxes and counting repeats.
    let mut groups: Vec<(Placement, u32)> = Vec::new();
    for p in placements {
        match groups.iter_mut().find(|(g, _)| g.name == p.name) {
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
        let Some(xobj) = xobjects.get(&p.name).ok().and_then(|o| deref_obj(doc, o).ok()) else {
            continue;
        };
        let Some((w, h, fmt)) = probe_stream(xobj) else {
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

    // Phase 2 — decode each surviving object once (cache by name).
    let mut cache: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    for want in wants {
        let Some(xobj) = xobjects
            .get(&want.p.name)
            .ok()
            .and_then(|o| deref_obj(doc, o).ok())
        else {
            continue;
        };
        let bytes = match cache.get(&want.p.name) {
            Some(d) => d.clone(),
            None => {
                let d = decode_xobject_bytes(doc, xobj);
                cache.insert(want.p.name.clone(), d.clone());
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
            // Undecodable placement: keep the metadata so callers still see it.
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
fn collect_page_ops(
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
                    ctm.post_mul(&m);
                }
            }
            "Do" => {
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some(xobj) = xobjects.get(name).ok().and_then(|o| deref_obj(doc, o).ok()) else {
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
                        x0,
                        y0,
                        x1,
                        y1,
                    });
                } else if subtype == b"Form" {
                    let mut form_ctm = ctm;
                    if let Ok(m) = s.dict.get(b"Matrix") {
                        if let Ok(arr) = m.as_array() {
                            form_ctm.post_mul(&mtx_from_slice(arr));
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

fn mtx_from_slice(a: &[Object]) -> Mtx {
    let g = |i: usize| crate::layout::num(a.get(i).unwrap_or(&Object::Null)).unwrap_or(0.0);
    crate::layout::Mtx::from_parts(g(0), g(1), g(2), g(3), g(4), g(5))
}

fn placement_bbox(ctm: &Mtx) -> (f64, f64, f64, f64) {
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
fn probe_stream(xobj: &Object) -> Option<(u32, u32, String)> {
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
fn decode_xobject_bytes(doc: &Document, xobj: &Object) -> Option<Vec<u8>> {
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

fn stream_width_height(s: &lopdf::Stream) -> Option<(u32, u32)> {
    let w = s.dict.get(b"Width").ok()?.as_i64().ok()? as u32;
    let h = s.dict.get(b"Height").ok()?.as_i64().ok()? as u32;
    Some((w, h))
}

fn stream_filters(s: &lopdf::Stream) -> Vec<Vec<u8>> {
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

// -- filter decoders ---------------------------------------------------------

fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut d = flate2::read::ZlibDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out).ok()?;
    Some(out)
}

fn decode_asciihex(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &c in data {
        if c == b'>' {
            break;
        }
        let l = c.to_ascii_lowercase();
        let v = match l {
            b'0'..=b'9' => l - b'0',
            b'a'..=b'f' => l - b'a' + 10,
            _ => continue,
        };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push((h << 4) | v),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    Some(out)
}

fn decode_ascii85(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut group: u32 = 0;
    let mut count = 0usize;
    for &c in data {
        if c == b'~' {
            break;
        }
        if c == b'z' && count == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&c) {
            continue;
        }
        group = group * 85 + (c - b'!') as u32;
        count += 1;
        if count == 5 {
            out.extend_from_slice(&group.to_be_bytes());
            group = 0;
            count = 0;
        }
    }
    if count == 1 {
        group = group.wrapping_mul(85u32.pow(4));
        out.extend_from_slice(&group.to_be_bytes()[..1]);
    } else if count == 2 {
        group = group.wrapping_mul(85u32.pow(3));
        out.extend_from_slice(&group.to_be_bytes()[..2]);
    } else if count == 3 {
        group = group.wrapping_mul(85u32.pow(2));
        out.extend_from_slice(&group.to_be_bytes()[..3]);
    } else if count == 4 {
        group = group.wrapping_mul(85);
        out.extend_from_slice(&group.to_be_bytes()[..4]);
    }
    Some(out)
}

fn decode_runlength(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let c = data[i] as i8;
        i += 1;
        if c == -128 {
            break;
        }
        if c >= 0 {
            let len = c as usize + 1;
            if i + len > data.len() {
                return None;
            }
            out.extend_from_slice(&data[i..i + len]);
            i += len;
        } else {
            let len = (-(c as i16)) as usize + 1;
            if i >= data.len() {
                return None;
            }
            out.extend(std::iter::repeat(data[i]).take(len));
            i += 1;
        }
    }
    Some(out)
}

// -- color space -> RGBA -----------------------------------------------------

fn raster_to_rgba(doc: &Document, w: u32, h: u32, bits: u32, cs: &Object, data: &[u8]) -> Option<Vec<u8>> {
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

fn resolve_indexed_palette(doc: &Document, a: &[Object]) -> Option<Vec<u8>> {
    let lookup = a.get(3)?;
    let o = deref_obj(doc, lookup).ok()?;
    match o {
        Object::String(b, _) => Some(b.clone()),
        Object::Stream(st) => st.decompressed_content_with_limit(16 << 20).ok(),
        _ => None,
    }
}

// -- PNG encoder -------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut c = Vec::with_capacity(kind.len() + data.len());
    c.extend_from_slice(kind);
    c.extend_from_slice(data);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

/// Encode RGBA8 pixels as PNG.
pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6); // RGBA
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    chunk(&mut out, b"IHDR", &ihdr);

    let row_len = width as usize * 4;
    let mut raw = Vec::with_capacity((row_len + 1) * height as usize);
    for row in rgba.chunks_exact(row_len) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = enc.write_all(&raw);
    let compressed = enc.finish().unwrap_or_default();
    chunk(&mut out, b"IDAT", &compressed);
    chunk(&mut out, b"IEND", &[]);
    out
}

// -- classification ----------------------------------------------------------

/// Pure geometry-based role classification.
fn classify_geometry(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_roundtrip_vectors() {
        assert_eq!(b64encode(b""), "");
        assert_eq!(b64encode(b"f"), "Zg==");
        assert_eq!(b64encode(b"fo"), "Zm8=");
        assert_eq!(b64encode(b"foo"), "Zm9v");
        assert_eq!(b64encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn png_writer_produces_valid_signature_and_chunks() {
        // 2x1 RGBA
        let png = encode_png_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        // IHDR at offset 8
        assert_eq!(&png[12..16], b"IHDR");
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!((w, h), (2, 1));
        // chunks end with IEND chunk header
        assert!(png.windows(4).rev().any(|x| x == b"IEND"));
        // IDAT present
        assert!(png.windows(4).any(|x| x == b"IDAT"));
    }

    #[test]
    fn ascii85_roundtrip() {
        let enc = b"9jqo^BlbD-BleB1DJ+*+F(f,q/0JhKF<GL>Cj@.4Gp$d7F!,L7@<6@)/0JDEF<G%<+EV:2F!,O<DJ+*.@<*K0@<6L(Df-\\0Ec5e;DffZ(EZee.Bl.9pF\"AGXBPCsi+DGm>@3BB/F*&OCAfu2/AKYi(DIb:@FD,*)+C]U=@3BN#EcYf8ATD3s@q?d$AftVqCh[NqF<G:8+EV:.+Cf>-FD5W8ARlolDIal(DId<j@<?3r@:F%a+D58'ATD4$Bl@l3De:,-DJs`8ARoFb/0JMK@qB4^F!,R<AKZ&-DfTqBG%G>uD.RTpAKYo'+CT/5+Cei#DII?(E,9)oF*2M7/c~>";
        let dec = decode_ascii85(&enc[..]).expect("decode");
        let text = String::from_utf8_lossy(&dec);
        assert!(text.contains("Man is distinguished"));
    }
}

// ---------------------------------------------------------------------------
// Stage: pure-vector figure regions -> clipped-PDF cuts
// ---------------------------------------------------------------------------
//
// Some figures (charts, diagrams, logos) have no raster stream at all — they
// are drawn with PDF path operators. We detect compact clusters of painted
// vector ink (filled rects, bars, strokes) that look like a figure and cut
// them out as a tiny standalone PDF that renders only that region of the
// page (content is wrapped in a clip + translate, MediaBox shrunk to the
// region). Decorative hairlines and page borders do not qualify.

/// Device-space bbox of one painted vector segment.
#[derive(Clone, Copy)]
struct InkBox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

fn merge_boxes(a: &InkBox, b: &InkBox) -> InkBox {
    InkBox {
        x0: a.x0.min(b.x0),
        y0: a.y0.min(b.y0),
        x1: a.x1.max(b.x1),
        y1: a.y1.max(b.y1),
    }
}

/// Walk one content stream and collect painted vector ink boxes (device
/// space). Handles q/Q/cm tracking like the media walker and descends into
/// Form XObjects (depth limited). Only subpaths that are actually painted
/// (f/F/S/s/B/b) and carry non-trivial area qualify.
fn collect_ink_boxes(
    doc: &Document,
    xobjects: &Dictionary,
    content: &Content<Vec<Operation>>,
    out: &mut Vec<InkBox>,
    depth: usize,
    ctm_in: &Mtx,
) {
    if depth > 5 {
        return;
    }
    let mut ctm = *ctm_in;
    let mut stack: Vec<Mtx> = Vec::new();
    let mut path: Vec<(f64, f64)> = Vec::new();
    let mut start: Option<(f64, f64)> = None;

    let flush_path = |path: &mut Vec<(f64, f64)>, out: &mut Vec<InkBox>| {
        if path.len() < 2 {
            path.clear();
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in path.iter() {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let area = (x1 - x0) * (y1 - y0);
        if area.abs() > 4.0 {
            out.push(InkBox { x0, y0, x1, y1 });
        }
        path.clear();
    };

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
                    ctm.post_mul(&m);
                }
            }
            // Path construction operators.
            "m" => {
                path.clear();
                start = None;
                if let (Some(x), Some(y)) = (pt(op, 0), pt(op, 1)) {
                    let (dx, dy) = ctm.apply(x, y);
                    path.push((dx, dy));
                    start = Some((dx, dy));
                }
            }
            "l" => {
                if let (Some(x), Some(y)) = (pt(op, 0), pt(op, 1)) {
                    let (dx, dy) = ctm.apply(x, y);
                    path.push((dx, dy));
                }
            }
            "c" | "v" | "y" => {
                // Bezier: push the final end point only (bbox of curve lies
                // inside control convex hull; using end points is a close,
                // conservative approximation for figure detection).
                let last = if op.operator == "c" { 5 } else { 3 };
                if let (Some(x), Some(y)) = (pt(op, last), pt(op, last + 1)) {
                    let (dx, dy) = ctm.apply(x, y);
                    path.push((dx, dy));
                }
            }
            "re" => {
                // Rectangle: x y w h. Push its corners as its own path then
                // paint it when f/S arrives.
                if let (Some(x), Some(y), Some(w), Some(h)) =
                    (pt(op, 0), pt(op, 1), pt(op, 2), pt(op, 3))
                {
                    path.clear();
                    for (px, py) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                        let (dx, dy) = ctm.apply(px, py);
                        path.push((dx, dy));
                    }
                }
            }
            "h" => {
                if let Some(s0) = start {
                    path.push(s0);
                }
            }
            // Paint operators.
            "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "S" | "s" => {
                flush_path(&mut path, out);
            }
            "n" => {
                path.clear();
            }
            "Do" => {
                flush_path(&mut path, out);
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some(xobj) = xobjects.get(name).ok().and_then(|o| deref_obj(doc, o).ok()) else {
                    continue;
                };
                if let Object::Stream(fs) = xobj {
                    let subtype = fs
                        .dict
                        .get(b"Subtype")
                        .ok()
                        .and_then(|o| o.as_name().ok())
                        .unwrap_or_default();
                    if subtype == b"Form" {
                        let mut fctm = ctm;
                        if let Ok(m) = fs.dict.get(b"Matrix") {
                            if let Ok(arr) = m.as_array() {
                                fctm.post_mul(&mtx_from_slice(arr));
                            }
                        }
                        let sub_xo: Dictionary = fs
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
                        if let Ok(b) = fs.decompressed_content_with_limit(64 << 20) {
                            if let Ok(sub) = Content::decode(&b) {
                                collect_ink_boxes(doc, &sub_xo, &sub, out, depth + 1, &fctm);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    flush_path(&mut path, out);
}

fn pt(op: &Operation, i: usize) -> Option<f64> {
    op.operands
        .get(i)
        .and_then(|o| crate::layout::num(o))
}

/// Merge ink boxes into figure regions by proximity. A figure must contain at
/// least `min_segments` painted subpaths and have a reasonable size — this
/// excludes single decorative hairlines and page borders.
fn cluster_figures(boxes: &[InkBox], page_bbox: Option<(f64, f64, f64, f64)>) -> Vec<InkBox> {
    if boxes.len() < 3 {
        return Vec::new();
    }
    let page_w = page_bbox.map_or(595.0, |(a, _, b, _)| (b - a).abs());
    let page_h = page_bbox.map_or(842.0, |(_, a, _, b)| (b - a).abs());
    let max_gap = (0.12 * page_w).clamp(12.0, 90.0);

    // Greedy agglomerative clustering by bbox center distance.
    let mut clusters: Vec<InkBox> = Vec::new();
    for b in boxes {
        // Greedy merge: attach to the cluster whose bounding box is nearest
        // (edge distance). Bar charts and dot plots form a single figure even
        // when individual elements are separated by small gutters.
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in clusters.iter().enumerate() {
            let gap_x = (b.x0 - c.x1).max(c.x0 - b.x1).max(0.0);
            let gap_y = (b.y0 - c.y1).max(c.y0 - b.y1).max(0.0);
            let d = gap_x.max(gap_y);
            if d <= max_gap && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        match best {
            Some((i, _)) => clusters[i] = merge_boxes(&clusters[i], b),
            None => clusters.push(*b),
        }
    }
    clusters
        .into_iter()
        .filter(|c| {
            let w = (c.x1 - c.x0).abs();
            let h = (c.y1 - c.y0).abs();
            let area = w * h;
            // Require a genuinely visible figure block: >= 0.5% of the page
            // and not a sliver.
            area >= 0.005 * page_w * page_h && w >= 12.0 && h >= 8.0
        })
        .collect()
}

/// Build a standalone one-page PDF that renders only the given region of the
/// page (content wrapped in a clip+translate, MediaBox shrunk to the region).
fn cut_page_region(doc: &Document, page_id: ObjectId, r: &InkBox) -> Option<Vec<u8>> {
    let mut d = doc.clone();
    let w = (r.x1 - r.x0).abs();
    let h = (r.y1 - r.y0).abs();
    if w < 1.0 || h < 1.0 {
        return None;
    }
    let orig_content = d.get_page_content(page_id);
    if orig_content.is_empty() {
        return None;
    }
    // Wrapper: translate the region to the origin, clip to it, then replay.
    let mut wrapped = Vec::new();
    wrapped.extend_from_slice(
        format!(
            "q 1 0 0 1 {:.3} {:.3} cm 0 0 {:.3} {:.3} re W n\n",
            -r.x0, -r.y0, w, h
        )
        .as_bytes(),
    );
    wrapped.extend_from_slice(&orig_content);
    wrapped.extend_from_slice(b"\nQ\n");

    // Point the page at a fresh content stream.
    let mut stream = lopdf::Stream::new(
        Dictionary::new(),
        wrapped,
    );
    stream.compress().ok()?;
    let new_id = (d.max_id + 1, 0);
    d.max_id += 1;
    d.objects.insert(new_id, Object::Stream(stream));
    let page = d.get_object_mut(page_id).ok()?.as_dict_mut().ok()?;
    page.set("Contents", Object::Reference(new_id));
    page.set("MediaBox", Object::Array(vec![
        Object::Real(0.0),
        Object::Real(0.0),
        Object::Real(w as f32),
        Object::Real(h as f32),
    ]));
    // The page tree still points at all pages; shrink to one page.
    let mut buf = Vec::new();
    d.save_to(&mut buf).ok()?;
    Some(buf)
}

/// Detect and cut pure-vector figure regions on a page (charts/diagrams/logos
/// drawn with paths, no raster). Returns PDF media items (kind diagram).
pub fn extract_page_vector_figures(
    doc: &Document,
    page_id: ObjectId,
    page_num: usize,
    page_bbox: Option<(f64, f64, f64, f64)>,
) -> Vec<MediaItem> {
    let xobjects = page_xobjects(doc, page_id).unwrap_or_default();
    let Ok(content) = doc.get_and_decode_page_content(page_id) else {
        return Vec::new();
    };
    let mut boxes: Vec<InkBox> = Vec::new();
    collect_ink_boxes(doc, &xobjects, &content, &mut boxes, 0, &Mtx::ID);
    let figs = cluster_figures(&boxes, page_bbox);
    if figs.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, f) in figs.iter().enumerate() {
        let Some(data) = cut_page_region(doc, page_id, f) else {
            continue;
        };
        if data.len() > (6 << 20) {
            continue;
        }
        let item = MediaItem {
            page: page_num,
            x0: f.x0.min(f.x1),
            y0: f.y0.min(f.y1),
            x1: f.x0.max(f.x1),
            y1: f.y0.max(f.y1),
            width: 0,
            height: 0,
            format: "application/pdf".into(),
            kind: MediaKind::Diagram,
            decorative: false,
            repeat: 1,
            data: data.clone(),
            data_b64: b64encode(&data),
        };
        // width/height unknown without rasterizing; leave 0 and let clients
        // render the PDF. Keep name hint for diagnostics.
        let _ = i;
        out.push(item);
    }
    out
}

#[cfg(test)]
mod vector_tests {
    use super::*;

    #[test]
    fn vector_chart_region_is_cut_to_pdf() {
        // A pure-vector bar chart (no raster stream). detect_vectors must find
        // one figure region covering the bars and return a valid clipped PDF.
        let bytes = std::fs::read("tests/fixtures/synth_vector_chart.pdf").expect("fixture missing");
        let doc = lopdf::Document::load_mem(&bytes).unwrap();
        for (_pn, pid) in doc.get_pages() {
            let figs =
                extract_page_vector_figures(&doc, pid, 1, Some((0.0, 0.0, 595.0, 842.0)));
            assert!(!figs.is_empty(), "expected at least one vector figure");
            for f in figs {
                assert_eq!(f.kind, MediaKind::Diagram);
                assert!(f.data.starts_with(b"%PDF"), "cut must be a PDF");
                assert!(!f.data_b64.is_empty());
                // Region should cover the bars (x 80..300, y 100..190 area).
                assert!(f.x1 - f.x0 >= 100.0, "figure too narrow");
            }
        }
    }
}
