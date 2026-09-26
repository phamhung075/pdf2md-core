// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Raster image extraction from PDF content streams and XObject resources.

mod shrink;
pub(crate) use shrink::*;
mod inline_image;
pub(crate) use inline_image::*;
mod decode;
pub use decode::*;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::HashMap;

use crate::layout::Mtx;
use crate::media::codecs::{
    b64encode, decode_ascii85, decode_asciihex, decode_png_rgba, decode_runlength,
    encode_png_rgba, inflate,
};
use crate::media::{MediaItem, MediaKind};

pub(crate) const BG_AREA_FRAC: f64 = 0.82;

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
///
/// `max_image_dimension` caps the pixel width/height a raw/PNG-reconstructed
/// raster is downscaled to before base64 encoding (see
/// `ConversionOptions::max_image_dimension`); pass `u32::MAX` to disable.
pub fn extract_page_media(
    doc: &Document,
    page_id: ObjectId,
    page_num: usize,
    page_bbox: Option<(f64, f64, f64, f64)>,
    max_image_dimension: u32,
) -> Vec<MediaItem> {
    let Some(xobjects) = page_xobjects(doc, page_id) else {
        return Vec::new();
    };
    let Ok(content) = crate::text_extract::decode_page_content(doc, page_id) else {
        return Vec::new();
    };
    let mut placements: Vec<Placement> = Vec::new();
    let (init_ctm, _) = crate::layout::glyph_stream::page_initial_transform(doc, page_id);
    collect_page_ops(doc, &xobjects, &content, &mut placements, 0, &init_ctm);

    // Recover inline (`BI ... EI`) images that lopdf dropped from the parsed
    // operation stream (abbreviated color spaces / filtered samples). Scan the
    // raw decoded content with the same graphics-state matrix so each inline
    // placement lands at its on-page box.
    let raw = doc.get_page_content_with_limit(page_id, 64 << 20).unwrap_or_default();
    if !raw.is_empty() {
        placements.extend(scan_inline_images(&raw, init_ctm));
    }

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
    let mut cache: HashMap<Key, Option<(Vec<u8>, u32, u32)>> = HashMap::new();
    for want in wants {
        let key = want
            .p
            .obj_id
            .map(Key::Id)
            .unwrap_or_else(|| Key::Name(want.p.name.clone()));
        let bytes = match cache.get(&key) {
            Some(d) => d.clone(),
            None => {
                let d = decode_xobject_bytes(doc, &want.p.obj, max_image_dimension);
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
            Some((data, enc_w, enc_h)) => {
                out.push(MediaItem {
                    page: page_num,
                    x0: want.p.x0,
                    y0: want.p.y0,
                    x1: want.p.x1,
                    y1: want.p.y1,
                    // Report the actually-encoded pixel dimensions (post
                    // downscale for raw rasters; unchanged for JPEG
                    // passthrough) rather than the pre-decode probe, so
                    // callers never see a `width`/`height` that disagrees
                    // with the bytes they got.
                    width: enc_w,
                    height: enc_h,
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
            // Inline image: lopdf drops `BI ... ID <data> EI` inline images
            // whose color space is abbreviated (`/CS /G`) or whose data is
            // filtered, so a placed inline barcode is silently lost. We recover
            // them from the raw content stream instead (see
            // `scan_inline_images`), which also tracks the correct graphics
            // state; this lopdf-op path is therefore not used.
            "BI" => {}
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
    // Reject an oversized declaration before any inflated sample or RGBA buffer
    // is allocated; the pixel count is the true memory driver, not the box.
    // 64-bit product so the guard cannot wrap on 32-bit wasm32.
    if (width as u64) * (height as u64) > super::MAX_IMAGE_PIXELS as u64 {
        return None;
    }
    let filters = stream_filters(s);
    if filters.iter().any(|f| f == b"DCTDecode" || f == b"JPXDecode") {
        Some((width, height, "image/jpeg".into()))
    } else {
        Some((width, height, "image/png".into()))
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
    // Standalone icons, bullets, and badge glyphs under ~42pt (1.5cm) in both
    // dimensions are decorative UI furniture, not document figures.
    if w <= 42.0 && h <= 42.0 {
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

    // A barcode is a small, narrow scan. A wide, full-width raster (an attention
    // diagram spanning the text column) is not a barcode regardless of aspect.
    if (aspect > 5.0 || aspect < 0.2) && area < 30_000.0 && frac_w < 0.4 {
        return (MediaKind::Barcode, false);
    }
    // A raster much wider (or taller) than it is deep that occupies a
    // substantial share of the page width is a diagram, not a photo/chart.
    if (aspect > 2.6 || aspect < 0.6) && frac_w >= 0.35 {
        return (MediaKind::Diagram, false);
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
mod downscale_tests;


#[cfg(test)]
mod classification_tests;
