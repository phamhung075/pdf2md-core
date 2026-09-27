//! pdf2md-core — High-performance native Rust core engine for sub-millisecond
//! PDF-to-Markdown extraction and 2D spatial canvas table reconstruction.
//!
//! Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
//! SPDX-License-Identifier: BSL-1.1
//! Licensed under the Business Source License 1.1 (BSL-1.1).

//! In-code builders for the robustness repros that used to ship as binary PDF
//! fixtures under `tests/fixtures/` (binary PDFs are not kept in this repository):
//!
//! - the 542-byte O1 xref-stream repro (`/W [1 4294967296 1]`);
//! - the M1/M2 glyph-amplification repros (one `Tj` per glyph).
//!
//! Test-only: wired from `lib.rs` behind `#[cfg(test)]`.

/// Classic-PDF header emitted by the corpus generators.
const PDF_HEADER: &[u8] = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n";

/// One glyph-showing run, `(word) Tj 0 -3 Td\n` — 18 bytes. Repeated to inflate
/// the decoded content stream without inflating the file.
const GLYPH_OP: &[u8] = b"(word) Tj 0 -3 Td\n";

/// Opens the text block and selects Helvetica 8pt at (20, 760).
const GLYPH_CONTENT_PREFIX: &[u8] = b"BT /F1 8 Tf 20 760 Td\n";

/// Closes the text block.
const GLYPH_CONTENT_SUFFIX: &[u8] = b"ET";

/// Glyph ops for the M1 repro: 800 000 ops -> a 14.4 MB decoded stream behind a
/// ~35 KB file (the original fixture measured 1.87 GB RSS).
pub(crate) const GLYPH_AMPLIFICATION_OPS: usize = 800_000;

/// Glyph ops for the M2 repro: 300 000 ops -> a 5.4 MB decoded stream behind a
/// ~13 KB file (the original aborted under the harness's 1.6 GB address-space
/// limit).
pub(crate) const GLYPH_ABORT_OPS: usize = 300_000;

/// Width declared for the middle field of the O1 xref stream's `/W`: 2^32,
/// enough to make lopdf allocate 4 GiB.
const XREF_W_4GIB_FIELD_WIDTH: u64 = 4_294_967_296;

/// Bytes of the xref-stream payload in the O1 repro (one degenerate entry).
const XREF_W_4GIB_STREAM_LEN: usize = 10;

/// Assemble a classic-xref PDF (trailer `startxref` points at the xref table)
/// from `(object number, body)` pairs, mirroring the generators' `build_pdf`.
/// `objects` must be sorted by number.
fn classic_pdf(objects: &[(u32, Vec<u8>)], root: u32) -> Vec<u8> {
    let mut buf = PDF_HEADER.to_vec();
    let mut offsets: Vec<(u32, usize)> = Vec::with_capacity(objects.len());
    for (num, body) in objects {
        offsets.push((*num, buf.len()));
        buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        buf.extend_from_slice(body);
        buf.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = buf.len();
    let maxnum = objects.iter().map(|(num, _)| *num).max().unwrap_or(0) + 1;
    buf.extend_from_slice(format!("xref\n0 {maxnum}\n").as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \n");
    for num in 1..maxnum {
        match offsets.iter().find(|(n, _)| *n == num) {
            Some((_, off)) => buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => buf.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    buf.extend_from_slice(
        format!("trailer\n<< /Size {maxnum} /Root {root} 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n")
            .as_bytes(),
    );
    buf
}

/// zlib-compress `data`, the codec lopdf's `/FlateDecode` reads.
fn deflate(data: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).expect("flate compress");
    enc.finish().expect("flate finish")
}

/// One-page Helvetica PDF whose content stream is the Flate-compressed `content`.
fn flate_page_pdf(content: &[u8]) -> Vec<u8> {
    let compressed = deflate(content);
    let mut stream_obj = format!(
        "<< /Length {} /Filter /FlateDecode >>\nstream\n",
        compressed.len()
    )
    .into_bytes();
    stream_obj.extend_from_slice(&compressed);
    stream_obj.extend_from_slice(b"\nendstream");
    classic_pdf(
        &[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                   /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj),
            (
                5,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            ),
        ],
        1,
    )
}

/// O1: a 542-byte PDF whose trailer points `startxref` at an xref stream that
/// declares `/W [1 4294967296 1]` (hand-assembled because the cross-reference
/// is a stream, not a classic table).
pub(crate) fn oom_xref_w_4gib_pdf() -> Vec<u8> {
    let bodies: [(u32, &[u8]); 5] = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
               /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        ),
        (
            4,
            b"<< /Length 50 >>\nstream\nBT /F1 12 Tf 72 720 Td (hello) Tj ET\nendstream",
        ),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ];
    let mut buf = b"%PDF-1.5\n".to_vec();
    for (num, body) in bodies {
        buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        buf.extend_from_slice(body);
        buf.extend_from_slice(b"\nendobj\n");
    }
    let xref_off = buf.len();
    let xref_stream = [0u8; XREF_W_4GIB_STREAM_LEN];
    buf.extend_from_slice(b"6 0 obj\n");
    buf.extend_from_slice(
        format!(
            "<< /Type /XRef /Size 7 /W [1 {XREF_W_4GIB_FIELD_WIDTH} 1] \
             /Index [0 1] /Root 1 0 R /Length {} >>\nstream\n",
            xref_stream.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(&xref_stream);
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    buf.extend_from_slice(format!("startxref\n{xref_off}\n%%EOF\n").as_bytes());
    buf
}

/// M1/M2: a one-page PDF whose decoded content stream holds `glyph_ops`
/// `(word) Tj 0 -3 Td` runs, Flate-compressed so the file stays small. Above
/// `MAX_PAGE_GLYPHS` the pre-decode cap must truncate it and set
/// `budget_exhausted`; the decoded stream is what used to be materialised whole.
pub(crate) fn many_glyph_page_pdf(glyph_ops: usize) -> Vec<u8> {
    let mut content = Vec::with_capacity(
        GLYPH_CONTENT_PREFIX.len() + glyph_ops * GLYPH_OP.len() + GLYPH_CONTENT_SUFFIX.len(),
    );
    content.extend_from_slice(GLYPH_CONTENT_PREFIX);
    for _ in 0..glyph_ops {
        content.extend_from_slice(GLYPH_OP);
    }
    content.extend_from_slice(GLYPH_CONTENT_SUFFIX);
    flate_page_pdf(&content)
}
