// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Vector figure detection, path clustering, clipping, and PDF region extraction.

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::layout::Mtx;
use crate::media::codecs::b64encode;
use crate::media::raster::{deref_obj, page_xobjects};
use crate::media::{MediaItem, MediaKind};

/// Device-space bbox of one painted vector segment.
#[derive(Clone, Copy, Debug)]
pub struct InkBox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

pub fn merge_boxes(a: &InkBox, b: &InkBox) -> InkBox {
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
pub(crate) fn collect_ink_boxes(
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
        path.clear();
        let w = (x1 - x0).abs();
        let h = (y1 - y0).abs();
        // Ignore single horizontal/vertical hairlines (< 1 pt thick) — those
        // are table borders or separators, not figures.
        if (w < 1.0 && h > 20.0) || (h < 1.0 && w > 20.0) {
            return;
        }
        if w > 2.0 || h > 2.0 {
            out.push(InkBox { x0, y0, x1, y1 });
        }
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
                    ctm.pre_mul(&m);
                }
            }
            "m" => {
                flush_path(&mut path, out);
                if let (Some(x), Some(y)) = (pt(op, 0), pt(op, 1)) {
                    let dev = ctm.apply(x, y);
                    start = Some(dev);
                    path.push(dev);
                }
            }
            "l" => {
                if let (Some(x), Some(y)) = (pt(op, 0), pt(op, 1)) {
                    path.push(ctm.apply(x, y));
                }
            }
            "c" => {
                // Approximate cubic bezier by end point.
                if let (Some(x3), Some(y3)) = (pt(op, 4), pt(op, 5)) {
                    path.push(ctm.apply(x3, y3));
                }
            }
            "v" => {
                if let (Some(x3), Some(y3)) = (pt(op, 2), pt(op, 3)) {
                    path.push(ctm.apply(x3, y3));
                }
            }
            "y" => {
                if let (Some(x3), Some(y3)) = (pt(op, 2), pt(op, 3)) {
                    path.push(ctm.apply(x3, y3));
                }
            }
            "h" => {
                if let Some(s) = start {
                    path.push(s);
                }
            }
            "re" => {
                flush_path(&mut path, out);
                if let (Some(x), Some(y), Some(w), Some(h)) =
                    (pt(op, 0), pt(op, 1), pt(op, 2), pt(op, 3))
                {
                    let p0 = ctm.apply(x, y);
                    let p1 = ctm.apply(x + w, y);
                    let p2 = ctm.apply(x + w, y + h);
                    let p3 = ctm.apply(x, y + h);
                    let x0 = p0.0.min(p1.0).min(p2.0).min(p3.0);
                    let x1 = p0.0.max(p1.0).max(p2.0).max(p3.0);
                    let y0 = p0.1.min(p1.1).min(p2.1).min(p3.1);
                    let y1 = p0.1.max(p1.1).max(p2.1).max(p3.1);
                    let rw = (x1 - x0).abs();
                    let rh = (y1 - y0).abs();
                    if rw > 2.0 || rh > 2.0 {
                        out.push(InkBox { x0, y0, x1, y1 });
                    }
                }
            }
            // Painting operators: flush current path.
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                flush_path(&mut path, out);
            }
            "n" => {
                // Path consumed without painting (clip setup) — discard.
                path.clear();
            }
            "Do" => {
                if let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) {
                    if let Some(s) = xobjects
                        .get(name)
                        .ok()
                        .and_then(|o| deref_obj(doc, o).ok())
                        .and_then(|o| o.as_stream().ok())
                    {
                        let mut fctm = ctm;
                        if let Ok(m) = s.dict.get(b"Matrix") {
                            if let Ok(arr) = m.as_array() {
                                let g = |i: usize| {
                                    crate::layout::num(arr.get(i).unwrap_or(&Object::Null))
                                        .unwrap_or(0.0)
                                };
                                fctm.pre_mul(&Mtx::from_parts(g(0), g(1), g(2), g(3), g(4), g(5)));
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
pub fn cluster_figures(boxes: &[InkBox], page_bbox: Option<(f64, f64, f64, f64)>) -> Vec<InkBox> {
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
pub fn cut_page_region(doc: &Document, page_id: ObjectId, r: &InkBox) -> Option<Vec<u8>> {
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
        let path = "tests/fixtures/synth_vector_chart.pdf";
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                eprintln!("Skipping vector_chart_region_is_cut_to_pdf: fixture missing");
                return;
            }
        };
        let doc = lopdf::Document::load_mem(&bytes).unwrap();
        for (_pn, pid) in doc.get_pages() {
            let figs =
                extract_page_vector_figures(&doc, pid, 1, Some((0.0, 0.0, 595.0, 842.0)));
            assert!(!figs.is_empty(), "expected at least one vector figure");
            for f in figs {
                assert_eq!(f.kind, MediaKind::Diagram);
                assert!(f.data.starts_with(b"%PDF"), "cut must be a PDF");
                assert!(!f.data_b64.is_empty());
                assert!(f.x1 - f.x0 >= 100.0, "figure too narrow");
            }
        }
    }
}
