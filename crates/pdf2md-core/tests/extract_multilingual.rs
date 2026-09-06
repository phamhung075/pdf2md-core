//! End-to-end regression tests for multilingual (FR / VI) text extraction.
//!
//! synth_fr.pdf reproduces the EDF / Enedis failure mode: Type1 fonts with an
//! `/Encoding` dictionary whose `/Differences` array contains `/.notdef`
//! entries. lopdf's extractor fell back to its corrupt STANDARD_ENCODING table
//! there, turning `é` into `Ø` and `è` into `Ł`. The fixtures assert the fixed
//! behaviour of our own decoder (see src/text_extract.rs).
//!
//! synth_vi.pdf exercises the Vietnamese shape: a Type0 /Identity-H CID font
//! with a /ToUnicode CMap carrying precomposed Vietnamese characters.

use pdf2md_core::convert_pdf_bytes_to_markdown;
use pdf2md_core::ConversionOptions;

fn convert(fixture: &str) -> String {
    let bytes = std::fs::read(fixture).expect("fixture missing");
    convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
        .expect("conversion should succeed")
        .markdown
}

#[test]
fn french_accents_survive_differences_with_notdef() {
    let md = convert("tests/fixtures/synth_fr.pdf");
    for expected in [
        "Votre consommation en Électricité",
        "Relevé Enedis Relevé Client Relevé estimé",
        "Siège social : 22-30 avenue de Wagram",
        "dès 8h",
        "jusqu'à 20h",
    ] {
        assert!(md.contains(expected), "missing {expected:?} in:\n{md}");
    }
    // The corruption markers of lopdf's old STANDARD fallback must never appear.
    for bad in ['Ø', 'Ł', 'Þ'] {
        assert!(!md.contains(bad), "corruption marker {bad:?} present in:\n{md}");
    }
}

#[test]
fn vietnamese_extracts_via_tounicode_cmap() {
    let md = convert("tests/fixtures/synth_vi.pdf");
    for expected in [
        "Hóa đơn tiền điện tháng 6/2025",
        "tổng 1.234.567 ₫",
    ] {
        assert!(md.contains(expected), "missing {expected:?} in:\n{md}");
    }
}

#[test]
fn type3_glyph_encoded_reports_ocr_required() {
    // Ghostscript/PDFCreator-style output: a Type3 font with numeric glyph
    // names and no ToUnicode. No text is recoverable from the text layer, so
    // the engine must report a clear "needs OCR" error instead of a silent
    // empty markdown / "0 words".
    let bytes = std::fs::read("tests/fixtures/type3_glyph.pdf").expect("fixture missing");
    let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default());
    let err = res.expect_err("glyph-encoded document should not produce markdown");
    assert!(
        err.contains("OCR"),
        "error should mention OCR, got: {err}"
    );
}

#[test]
fn glyph_positioned_page_is_reassembled_in_reading_order() {
    // LibreOffice-form style output: one glyph per `BT … Tm … TD … TJ … ET`
    // block, absolute positions, NO space glyphs (word boundaries are encoded
    // as inter-glyph gaps), and lines emitted out of reading order. The
    // geometry engine must (1) sort lines top-to-bottom and (2) recover the
    // gap-encoded spaces.
    let md = convert("tests/fixtures/synth_fiche_glyphs.pdf");
    // "Vie Privée" (top) must precede "Liens personnels" (bottom) even though
    // the fixture emits "Liens personnels" first.
    let vie = md.find("Vie Privée").expect("top line missing");
    let liens = md.find("Liens personnels").expect("bottom line missing");
    assert!(
        vie < liens,
        "reading order wrong: 'Vie Privée' should come before 'Liens personnels':\n{md}"
    );
    // Gap-encoded spaces must be recovered (no merged words).
    assert!(md.contains("Vie Privée"), "word gap not recovered:\n{md}");
    assert!(md.contains("Liens personnels"), "word gap not recovered:\n{md}");
}

#[test]
fn compressed_text_layer_ticket_is_detected_and_extracted() {
    // Electronic tickets (e.g. "billet électronique") store their content
    // streams FlateDecode-compressed, so the raw bytes contain no "BT"/"Tj"
    // markers. The digital-text-layer detector must still recognise it (by
    // decompressing the stream / checking fonts), and the extractor must turn
    // the French/English text into Markdown.
    // A synthetic ticket whose content stream is FlateDecode-compressed (no
    // personal data). Raw bytes carry no "BT"/"Tj" markers.
    let bytes = std::fs::read("tests/fixtures/synth_ticket_compressed.pdf").expect("fixture missing");
    assert!(!bytes.windows(2).any(|w| w == b"BT"), "fixture should store text compressed");
    assert!(
        pdf2md_core::is_digital_pdf_bytes(&bytes),
        "compressed text-stream PDF should be detected as digital"
    );
    let md = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
        .expect("digital ticket should extract, not be rejected as scanned")
        .markdown;
    for expected in [
        "BILLET ÉLECTRONIQUE",
        "RÉFÉRENCE DE VOTRE RÉSERVATION",
        "pièce d'identité",
        "carte d'embarquement",
    ] {
        assert!(md.contains(expected), "missing {expected:?} in:\n{md}");
    }
}

#[test]
fn aligned_grid_glyph_page_becomes_gfm_table() {
    // A genuine aligned grid drawn the way table producers (LibreOffice,
    // print drivers) draw one: every row starts its cell words at the same
    // absolute column x (per-glyph BT/Tm/TD/TJ blocks). The geometry engine's
    // Stage-3 table recovery must turn it into a GFM pipe table and report a
    // real table count.
    let bytes = std::fs::read("tests/fixtures/synth_grid_table.pdf").expect("fixture missing");
    let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
        .expect("grid fixture should convert");
    assert!(res.tables_detected >= 1, "expected >=1 table, got {}", res.tables_detected);
    let md = res.markdown;
    // GFM header + separator + body cells.
    assert!(md.contains("| Désignation | Quantité | Prix unitaire | Montant |"), "missing header row:\n{md}");
    assert!(md.contains("| --- |"), "missing GFM separator:\n{md}");
    assert!(md.contains("| Abonnement | 1 | 12,50 | 12,50 |"), "missing data row:\n{md}");
    assert!(md.contains("| Consommation | 240 | 0,1726 | 41,42 |"), "missing data row:\n{md}");
    assert!(md.contains("| Réduction | -1 | -3,00 | -3,00 |"), "missing data row:\n{md}");
    // Each cell must keep its whole text (no merged or dropped words).
    assert!(md.contains("Prix unitaire"), "cell words merged:\n{md}");
    assert!(md.contains("Taxes diverses"), "cell words merged:\n{md}");
}

#[test]
fn two_column_glyph_page_reads_column_by_column() {
    // A page with two side-by-side prose columns sharing baselines. Row-major
    // output would interleave FR/EN lines; human reading order must emit the
    // whole left column first, then the right column, with the full-width
    // title before both.
    let bytes = std::fs::read("tests/fixtures/synth_two_column.pdf").expect("fixture missing");
    let res = convert_pdf_bytes_to_markdown(&bytes, &ConversionOptions::default())
        .expect("two-column page should convert");
    let md = res.markdown;
    // Reading order: title -> all 4 French lines -> all 4 English lines.
    let f1 = md.find("Première ligne de la colonne gauche.").expect("left col 1");
    let f2 = md.find("Quatrième ligne de la colonne gauche.").expect("left col 4");
    let e1 = md.find("First line of the right column.").expect("right col 1");
    assert!(
        f1 < e1 && f2 < e1,
        "left column must finish before right column starts:\n{md}"
    );
    // Structural block list is present and ordered: title first, then the
    // left column's lines, then the right column's lines.
    assert!(!res.blocks.is_empty(), "expected blocks");
    let texts: Vec<&str> = res.blocks.iter().map(|b| b.text.as_str()).collect();
    assert!(texts[0].contains("Rapport"), "title block first: {texts:?}");
    let left_idx = texts
        .iter()
        .position(|t| t.contains("Première"))
        .expect("left col present");
    let right_idx = texts
        .iter()
        .position(|t| t.contains("First line"))
        .expect("right col present");
    assert!(
        left_idx < right_idx,
        "left column blocks before right column blocks: {texts:?}"
    );
}
