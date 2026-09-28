// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Robust multilingual (FR / VI / EN) PDF text extraction for the fast path.
//!
//! This module replaces `lopdf::Document::extract_text` for the digital-PDF
//! fast path. `lopdf`'s extractor has two defects that corrupt accented Latin
//! text (French, Vietnamese, ...):
//!
//! 1. A `/Encoding` dictionary whose `/Differences` array contains `/.notdef`
//!    (extremely common in real-world generators, e.g. EDF / Enedis bills) is
//!    treated as an *error*, so the whole font silently falls back to lopdf's
//!    `STANDARD_ENCODING` table.
//! 2. That fallback table is corrupt for bytes >= 0xC0: it maps byte `0xE9`
//!    (`é`) to `Ø` and byte `0xE8` (`è`) to `Ł`, among others.
//!
//! We therefore resolve font encodings ourselves with correct data tables and
//! decode each text run accordingly:
//!   * `/ToUnicode` CMaps (bfchar / bfrange) — authoritative when present;
//!   * `/Encoding` by name (`WinAnsiEncoding`, `MacRomanEncoding`, ...);
//!   * `/Encoding` dictionaries with `/Differences` (`.notdef` and unknown
//!     glyph names are tolerated, AGL `uniXXXX` names supported);
//!   * a WinAnsi heuristic for non-symbolic simple fonts with no encoding
//!     information (how the overwhelming majority of producers write accented
//!     Latin text).
//!
//! Page content / font-structure parsing still comes from lopdf (public API
//! only). If content parsing fails, the caller falls back to lopdf's own
//! extractor.

use super::*;

/// True for the handful of faces whose byte codes index a built-in glyph set
/// with no Latin semantics (dingbats/dingbat-like fonts). A font merely flagged
/// Symbolic but named as a text face is *not* one of these.
pub(crate) fn is_dingbat_face(font: &Dictionary) -> bool {
    if let Some(bf) = get_name(font, b"BaseFont") {
        let upper = bf.to_ascii_uppercase();
        for marker in [
            b"SYMBOL".as_slice(),
            b"ZAPFDINGBATS",
            b"WINGDINGS",
            b"PICTS",
        ] {
            if upper.windows(marker.len()).any(|w| w == marker) {
                return true;
            }
        }
    }
    false
}

pub(super) fn is_symbolic(doc: &Document, font: &Dictionary) -> bool {
    if let Some(desc) = font.get(b"FontDescriptor").ok().and_then(|o| deref(doc, o)) {
        if let Object::Dictionary(d) = desc {
            if let Ok(flags) = d.get(b"Flags").and_then(|f| f.as_i64()) {
                if flags & 0x4 != 0 {
                    return true;
                }
            }
        }
    }
    is_dingbat_face(font)
}

/// Extract the `/ToUnicode` CMap of a font (handles indirect references).
pub(super) fn font_to_unicode(doc: &Document, font: &Dictionary) -> Option<CMapCodec> {
    let obj = font.get(b"ToUnicode").ok()?;
    let obj = deref(doc, obj)?;
    let stream = obj.as_stream().ok()?;
    let data = stream.get_plain_content_with_limit(16 << 20).ok()?;
    parse_cmap(&data)
}

pub(crate) fn resolve_codec(doc: &Document, font: &Dictionary) -> Option<Codec> {
    let subtype = get_name(font, b"Subtype").unwrap_or(b"");

    // PUA code points mean different glyphs in the Symbol and Wingdings
    // charsets, so carry the family (from `BaseFont`, or the descriptor's
    // `FontName`) into the codec and map them at decode time.
    let base_font = get_name(font, b"BaseFont")
        .or_else(|| {
            font.get(b"FontDescriptor")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| get_name(d, b"FontName"))
        })
        .unwrap_or(b"");
    let pua = pua_family_for_base_font(base_font);

    if subtype == b"Type0" {
        // CID-keyed font: decode exclusively through its ToUnicode CMap.
        return font_to_unicode(doc, font).map(|cm| Codec::CMap(cm, None, pua));
    }

    // Simple fonts: resolve the 8-bit /Encoding first (it also serves as the
    // fallback for a partial /ToUnicode CMap).
    let enc = font.get(b"Encoding").ok().and_then(|o| deref(doc, o));

    let table = match enc {
        Some(Object::Name(n)) => base_table(n),
        Some(Object::Dictionary(d)) => {
            let mut t = match d.get(b"BaseEncoding").ok().and_then(|o| deref(doc, o)) {
                Some(Object::Name(n)) => base_table(n),
                _ => ascii_table(),
            };
            if let Ok(diffs) = d.get(b"Differences") {
                apply_differences(&mut t, doc, diffs);
            }
            t
        }
        _ => {
            if is_symbolic(doc, font) {
                // Symbolic fonts (dingbats etc.) carry no recoverable textual
                // semantics from a standard byte encoding: their codes index
                // the font's built-in glyph set. An explicit /ToUnicode CMap,
                // however, *is* authoritative semantics — and TeX's CMR/CMMI
                // faces are routinely flagged Symbolic with no /Encoding while
                // shipping a full /ToUnicode. Decode through that rather than
                // dropping every glyph drawn in the font.
                if let Some(cm) = font_to_unicode(doc, font) {
                    return Some(Codec::CMap(cm, None, pua));
                }
                // A Latin text face merely flagged Symbolic (very common on
                // subset CFF/Type1 fonts, e.g. Bouygues bills) still draws
                // StandardEncoding-compatible bytes; decode those through the
                // Latin table instead of dropping the whole document. Only the
                // genuine dingbat faces are left unmapped so they escalate.
                if is_dingbat_face(font) {
                    return None;
                }
            }
            // Non-symbolic simple fonts with no declared encoding almost
            // universally use WinAnsi byte values for accented Latin text.
            ByteTable(WIN_ANSI)
        }
    };

    // Some producers point a code's `/ToUnicode` at a C0 control instead of
    // U+0020 for the glyph the font itself defines as a space. The raw control
    // is unusable text and is stripped downstream, fusing the words on either
    // side. Where the font's own encoding (or a space-sized `/Widths` entry)
    // identifies the code as a space, that meaning wins over the broken map.
    let mut cm = font_to_unicode(doc, font);
    if let Some(cm) = cm.as_mut() {
        fix_control_space_codes(doc, font, &table, cm);
    }

    if let Some(cm) = cm {
        return Some(Codec::CMap(cm, Some(table), pua));
    }
    Some(Codec::Byte8(table, pua))
}

/// `/Widths` fraction of an em below/above which a code cannot be the space
/// glyph. Mainstream text faces set a space at roughly 0.2-0.33 em; the band is
/// widened a little so condensed and wide faces still qualify. Anything
/// narrower is a mark and anything wider is an ordinary letter.
const SPACE_WIDTH_EM_MIN: f64 = 0.15;
const SPACE_WIDTH_EM_MAX: f64 = 0.45;

/// The C0 controls a `/ToUnicode` map may use to name a *space*. U+0000 is
/// excluded: producers use NULL as the "no character / unmapped glyph" target
/// (a symbol or `.notdef` glyph drawn from its own outline), so treating it as a
/// space would inject a break inside a word. The observed space mis-mapping is
/// the non-NULL U+0001.
const SPACE_CONTROL_MIN: u16 = 0x01;
const SPACE_CONTROL_MAX: u16 = 0x1F;

/// Rewrite `/ToUnicode` entries that resolve a code to a C0 control when the
/// font itself says the code is a space, either because its `/Encoding` (or
/// base encoding) decodes to U+0020 or because it is an unnamed subset glyph
/// whose `/Widths` advance is a space's typical width. Any other control target
/// is left exactly as today, so a symbol glyph is never turned into a space.
fn fix_control_space_codes(doc: &Document, font: &Dictionary, table: &ByteTable, cm: &mut CMapCodec) {
    let control: Vec<usize> = (0..256).filter(|&c| cm.space_control_target(c as u32)).collect();
    if control.is_empty() {
        return;
    }
    // The font's own encoding already names the code a space (a `/Differences`
    // `space`/`uni0020` entry or a base-encoding space at 0x20).
    let mut fixes: Vec<usize> = control.iter().copied().filter(|&c| table.0[c] == 0x20).collect();
    // `0` means the code has no usable glyph name (an unnamed subset glyph such
    // as `g1`, or `.notdef`). Fall back to its advance width.
    let unnamed: Vec<usize> = control
        .iter()
        .copied()
        .filter(|&c| table.0[c] == 0 && !fixes.contains(&c))
        .collect();
    if !unnamed.is_empty() {
        let widths = resolve_widths(doc, font);
        let em = widths.em_scale().max(f64::MIN_POSITIVE);
        for c in unnamed {
            let Some(w) = widths.width(&[c as u8]) else {
                continue;
            };
            let frac = w / 1000.0 / em;
            if (SPACE_WIDTH_EM_MIN..=SPACE_WIDTH_EM_MAX).contains(&frac) {
                fixes.push(c);
            }
        }
    }
    for c in fixes {
        cm.exact.insert((1, c as u32), vec![0x20]);
    }
}

impl CMapCodec {
    /// Whether the 1-byte source `code` maps to a C0 control character in the
    /// non-NULL space range ([`SPACE_CONTROL_MIN`], [`SPACE_CONTROL_MAX`]).
    fn space_control_target(&self, code: u32) -> bool {
        let target = self
            .exact
            .get(&(1, code))
            .and_then(|units| units.first())
            .copied()
            .or_else(|| {
                self.ranges
                    .iter()
                    .find(|(len, lo, hi, _)| *len == 1 && code >= *lo && code <= *hi)
                    .map(|(_, lo, _, dst)| (*dst + (code - lo)) as u16)
            });
        matches!(target, Some(t) if (SPACE_CONTROL_MIN..=SPACE_CONTROL_MAX).contains(&t))
    }
}

// ---------------------------------------------------------------------------
// Page text walker
// ---------------------------------------------------------------------------
