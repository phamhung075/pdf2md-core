// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! PDF content-stream glyph extraction, font metric resolution, and 2D span aggregation.

use super::*;

impl Widths {
    /// Advance width for a char code, in 1/1000 em units.
    pub(crate) fn width(&self, bytes: &[u8]) -> Option<f64> {
        match self {
            Widths::Byte(t) => {
                if bytes.is_empty() {
                    return None;
                }
                // Sum *every* glyph's advance. A `Tj`/`TJ` operand carries a
                // whole word or phrase, and every consumer uses
                // `span.x + span.advance` as the run's right edge. Returning
                // only the first glyph's width under-measures multi-character
                // runs, so a run that ends where an absolute `Tm` boundary
                // begins looks like it stops far short of the next span — a
                // phantom gap wide enough for the column/reading-order
                // heuristics to split single-column prose into fake columns.
                let mut total = 0.0;
                let mut any = false;
                for &b in bytes {
                    let w = t[b as usize];
                    if w > 0.0 {
                        any = true;
                        total += w;
                    }
                }
                if any {
                    Some(total)
                } else {
                    None
                }
            }
            Widths::Cid {
                map,
                default,
                encoding,
            } => {
                if bytes.is_empty() {
                    return None;
                }
                // Sum every CID's advance, exactly as the `Widths::Byte` branch
                // above does. Callers use `span.x + span.advance` as the run's
                // right edge *and* advance the text matrix by this value, so
                // folding a multi-glyph run into one merged CID (which misses
                // /W, falls back to /DW, and mis-measures every multi-glyph
                // run) drifts all later x positions and destroys residual-gap
                // space detection — fusing words in Identity-H/CID fonts.
                let mut total = 0.0;
                let mut any = false;
                match encoding {
                    Some(cm) => {
                        // Custom code->CID CMap: consume the code lengths the
                        // CMap knows, exactly as `decode_cmap` does.
                        let mut i = 0usize;
                        while i < bytes.len() {
                            let mut matched = false;
                            for len in 1u8..=4u8 {
                                let n = len as usize;
                                if i + n > bytes.len() {
                                    continue;
                                }
                                if let Some(cid) = cm.lookup(&bytes[i..i + n]) {
                                    let w = map.get(&cid).copied().unwrap_or(*default);
                                    if w > 0.0 {
                                        any = true;
                                        total += w;
                                    }
                                    i += n;
                                    matched = true;
                                    break;
                                }
                            }
                            if !matched {
                                i += 1;
                            }
                        }
                    }
                    None => {
                        // Identity-H/V: the code bytes *are* the CID, two
                        // bytes each.
                        for chunk in bytes.chunks(2) {
                            let mut cid = 0u32;
                            for &b in chunk {
                                cid = (cid << 8) | b as u32;
                            }
                            let w = map.get(&cid).copied().unwrap_or(*default);
                            if w > 0.0 {
                                any = true;
                                total += w;
                            }
                        }
                    }
                }
                if any {
                    Some(total)
                } else {
                    None
                }
            }
            Widths::None => None,
        }
    }
}

pub(crate) fn resolve_widths(doc: &Document, font: &Dictionary) -> Widths {
    let subtype = get_name(font, b"Subtype").unwrap_or(b"");

    if subtype == b"Type0" {
        let desc = font
            .get(b"DescendantFonts")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_array().ok())
            .and_then(|a| a.first())
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok());
        let Some(desc) = desc else {
            return Widths::None;
        };
        let default = desc
            .get(b"DW")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(num)
            .unwrap_or(1000.0);
        let Some(w) = desc.get(b"W").ok().and_then(|o| deref(doc, o)) else {
            return Widths::None;
        };
        let Ok(arr) = w.as_array() else {
            return Widths::None;
        };
        let mut map = HashMap::new();
        // `/W` elements are routinely *indirect* objects — a single shared
        // width object referenced by every CID (and arrays referenced by the
        // `/W` entry itself). Every element must be resolved through `deref`
        // before it is read as a number or an array: otherwise `num()` sees an
        // `Object::Reference`, the whole `/W` table is silently dropped, and
        // every glyph falls back to `/DW` (1 em), inflating run advances and
        // fusing words (e.g. `| Gesamtbetrag der Zuschläge0,00 |` on the
        // intarsys EN16931 fixtures, whose `/W` is `[3 3 20 0 R 8 8 20 0 R …]`
        // with object 20 = 600 while `/DW` defaults to 1000).
        let numd = |o: &Object| -> Option<f64> { deref(doc, o).and_then(num) };
        let mut i = 0usize;
        while i < arr.len() {
            let Some(c0) = numd(&arr[i]).map(|v| v as u32) else {
                i += 1;
                continue;
            };
            i += 1;
            if i >= arr.len() {
                break;
            }
            if let Some(vals) = deref(doc, &arr[i]).and_then(|o| o.as_array().ok()) {
                for (k, it) in vals.iter().enumerate() {
                    if let Some(v) = numd(it) {
                        map.insert(c0 + k as u32, v);
                    }
                }
                i += 1;
            } else if i + 1 < arr.len() {
                if let (Some(c1), Some(v)) = (numd(&arr[i]).map(|x| x as u32), numd(&arr[i + 1])) {
                    for c in c0..=c1 {
                        map.insert(c, v);
                    }
                }
                i += 2;
            } else {
                break;
            }
        }
        // A non-identity `/Encoding` CMap stream maps code -> CID. Identity-H/V
        // are predefined names with no stream: the code bytes are the CID.
        let encoding = font
            .get(b"Encoding")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_stream().ok())
            .and_then(|s| s.get_plain_content_with_limit(16 << 20).ok())
            .and_then(|d| parse_cmap(&d));
        Widths::Cid {
            map,
            default,
            encoding,
        }
    } else {
        let first = font
            .get(b"FirstChar")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(num)
            .unwrap_or(0.0) as i32;
        let Some(w) = font.get(b"Widths").ok().and_then(|o| deref(doc, o)) else {
            if let Some(base_font) = font
                .get(b"BaseFont")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_name().ok())
            {
                let name = String::from_utf8_lossy(base_font).to_lowercase();
                if name.contains("courier") {
                    return Widths::Byte([crate::glyph_data::COURIER_WIDTH; 256]);
                } else if name.contains("helvetica-bold")
                    || (name.contains("helvetica") && name.contains("bold"))
                    || (name.contains("arial") && name.contains("bold"))
                {
                    return Widths::Byte(crate::glyph_data::HELVETICA_BOLD_WIDTHS);
                } else if name.contains("helvetica") || name.contains("arial") {
                    return Widths::Byte(crate::glyph_data::HELVETICA_WIDTHS);
                } else if name.contains("times") {
                    return Widths::Byte(crate::glyph_data::TIMES_ROMAN_WIDTHS);
                }
            }
            return Widths::None;
        };
        let Ok(arr) = w.as_array() else {
            return Widths::None;
        };
        let mut t = [0.0f64; 256];
        for (i, item) in arr.iter().enumerate() {
            let code = first + i as i32;
            if (0..=255).contains(&code) {
                t[code as usize] = num(item).unwrap_or(0.0);
            }
        }
        Widths::Byte(t)
    }
}

// ---------------------------------------------------------------------------
// Glyph spans + clustering
// ---------------------------------------------------------------------------
