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
