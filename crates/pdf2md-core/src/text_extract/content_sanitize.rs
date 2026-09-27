// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Page content-stream sanitisation for a lopdf parser gap.
//!
//! `lopdf`'s content parser (`parser::content`) re-enters a comment run at the
//! start of each operation but never skips whitespace before that run, so a
//! **blank line after a comment block** makes the next operation fail and the
//! whole remainder of the stream is silently dropped (`Content::decode`
//! returns `Ok` with only the operations parsed so far). Real producers hit
//! this: the PReS/PrintSoft statement generator writes a `%` metadata header
//! followed by a blank line, so every text operator after it disappears. A
//! minimal repro is `cm\n% c\n\nBT ET` -> `["cm"]`.
//!
//! [`decode_with_comment_fallback`] detects that truncation with the strict
//! parser and retries on a copy with top-level comments removed — comments are
//! inert per the PDF spec — adopting the retry only when it recovers strictly
//! more operations. A well-formed stream pays one extra parse only when it
//! contains a `%`; a page whose stream has no comment is untouched.

use lopdf::content::{Content, Operation};

/// Remove top-level `%` comments from a PDF content stream, keeping the bytes
/// inside literal `(...)` and hex `<...>` strings (a `%` there is data, not a
/// comment). Returns `None` when the stream has no top-level comment, so the
/// caller never allocates a second buffer for an ordinary page.
pub(crate) fn strip_content_comments(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    let mut changed = false;
    // Literal-string nesting; > 0 means the current byte is string data.
    let mut string_depth = 0usize;
    let mut escaped = false;
    // Inside a `<...>` hex string.
    let mut hex = false;
    while i < data.len() {
        let b = data[i];
        if hex {
            out.push(b);
            if b == b'>' {
                hex = false;
            }
            i += 1;
            continue;
        }
        if string_depth > 0 {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'(' {
                string_depth += 1;
            } else if b == b')' {
                string_depth -= 1;
            }
            i += 1;
            continue;
        }
        match b {
            b'(' => {
                string_depth = 1;
                out.push(b);
                i += 1;
            }
            b'<' => {
                if data.get(i + 1) == Some(&b'<') {
                    // `<<` opens a dictionary, not a hex string.
                    out.push(b);
                    out.push(b'<');
                    i += 2;
                } else {
                    hex = true;
                    out.push(b);
                    i += 1;
                }
            }
            b'%' => {
                changed = true;
                // Drop the comment text but keep the terminating EOL byte.
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

/// Decode a page content stream, recovering from lopdf's silent truncation at
/// a blank line that follows a comment run (see the module docs).
pub(crate) fn decode_with_comment_fallback(
    data: &[u8],
) -> lopdf::Result<Content<Vec<Operation>>> {
    let parsed = Content::decode(data)?;
    // No comment can be the trigger; skip the extra strict parse.
    if !data.contains(&b'%') {
        return Ok(parsed);
    }
    // A fully-parsed stream needs no recovery.
    if Content::decode_strict(data).is_ok() {
        return Ok(parsed);
    }
    if let Some(stripped) = strip_content_comments(data) {
        if let Ok(recovered) = Content::decode(&stripped) {
            if recovered.operations.len() > parsed.operations.len() {
                return Ok(recovered);
            }
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_line_after_a_comment_truncates_lopdf() {
        // Documented library behaviour this module exists to work around.
        let data = b"1 0 0 1 0 0 cm\n% c\n\nBT ET";
        assert_eq!(
            Content::<Vec<Operation>>::decode(data)
                .map(|c| c.operations.len())
                .unwrap_or(0),
            1
        );
        let recovered = decode_with_comment_fallback(data).expect("decode");
        let ops: Vec<&str> = recovered
            .operations
            .iter()
            .map(|o| o.operator.as_str())
            .collect();
        assert_eq!(ops, ["cm", "BT", "ET"]);
    }

    #[test]
    fn comments_inside_a_literal_string_are_data() {
        let data = br#"BT /F1 12 Tf (50% off) Tj ET"#;
        assert!(strip_content_comments(data).is_none());
        let c = decode_with_comment_fallback(data).expect("decode");
        let s = c
            .operations
            .iter()
            .find(|o| o.operator == "Tj")
            .and_then(|o| o.operands.first())
            .and_then(|o| o.as_str().ok())
            .unwrap_or(b"");
        assert_eq!(s, b"50% off");
    }

    #[test]
    fn comments_inside_a_hex_string_are_preserved() {
        let data = b"BT <25> Tj % tail\nET";
        let stripped = strip_content_comments(data).expect("a comment exists");
        assert!(stripped.windows(4).any(|w| w == b"<25>"));
        assert!(!stripped.windows(4).any(|w| w == b"tail"));
    }

    #[test]
    fn nested_parens_and_escapes_do_not_end_a_string() {
        let data = br#"BT (a \(b\) c% d) Tj % real comment
ET"#;
        let stripped = strip_content_comments(data).expect("a comment exists");
        assert!(stripped.windows(8).any(|w| w == b"c% d) Tj"));
        assert!(!stripped.windows(4).any(|w| w == b"real"));
    }

    #[test]
    fn a_stream_without_comments_is_returned_unchanged() {
        assert!(strip_content_comments(b"BT (x) Tj ET").is_none());
    }
}
