"""End-to-end tests for document extraction using real-world flight tickets and receipts.

Tests edge cases against actual documents from Bac Phuc:
- Billet-électronique.pdf (multi-line borderless itinerary table, French/English text)
- Payment-receipt.pdf (multi-coupon baggage payment receipts, sparse fee line items)
"""
import os
import re
from typing import List, Optional, Tuple
import unittest
from unittest.mock import MagicMock, patch

from src.application.conversion_service import ConversionService, UnsupportedExtensionError
from src.domain.model import ConversionResult, ExtractionRequest
from src.domain.quality_gate import (
    check_ragged_tables,
    evaluate_quality_gate,
    get_cells,
    normalize_markdown_tables,
    recover_collapsed_form_lines,
    split_pipe_row,
)
from src.domain.rules import sanitize_filename
from src.infrastructure.converters.fast_path_adapter import FastPathConverterAdapter
from src.infrastructure.converters.vision_gemini_adapter import VisionGeminiAdapter


def find_fixtures_dir() -> Optional[str]:
    """Finds the directory containing test document fixtures."""
    candidates = [
        os.environ.get("TEST_FIXTURES_DIR"),
        os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(__file__))), "tests", "fixtures"),
        os.path.join(os.path.dirname(__file__), "fixtures"),
        "/tmp/fixtures",
    ]
    for c in candidates:
        if c and os.path.isdir(c):
            return c
    return None


def find_companion_md(pdf_path: str) -> Optional[str]:
    """Finds companion *_md file in the SAME folder as the given PDF file.

    Checks:
    - <stem>.vision.md
    - <filename>.vision.md
    - <stem>.md
    - <filename>.md
    - Any <stem>*.md in the same directory
    """
    folder = os.path.dirname(os.path.abspath(pdf_path))
    stem = os.path.splitext(os.path.basename(pdf_path))[0]
    fname = os.path.basename(pdf_path)

    for cand in [
        os.path.join(folder, f"{fname}.vision.md"),
        os.path.join(folder, f"{stem}.vision.md"),
        os.path.join(folder, f"{stem}.md"),
        os.path.join(folder, f"{fname}.md"),
    ]:
        if os.path.isfile(cand):
            return cand

    try:
        for entry in sorted(os.listdir(folder)):
            if entry.startswith(stem) and entry.endswith(".md"):
                candidate = os.path.join(folder, entry)
                if os.path.isfile(candidate):
                    return candidate
    except OSError:
        pass

    return None


def find_pdf_fixture_pairs(fixtures_dir: Optional[str] = None) -> List[Tuple[str, Optional[str]]]:
    """Discovers all PDF documents in fixtures directory and pairs each with its
    companion *_md file located in the SAME folder.
    """
    fdir = fixtures_dir or find_fixtures_dir()
    if not fdir or not os.path.isdir(fdir):
        return []

    pairs = []
    for entry in sorted(os.listdir(fdir)):
        if entry.lower().endswith(".pdf"):
            pdf_path = os.path.join(fdir, entry)
            companion_md = find_companion_md(pdf_path)
            pairs.append((pdf_path, companion_md))
    return pairs


def _find_fixture(name: str) -> str:
    fdir = find_fixtures_dir()
    if fdir:
        p = os.path.join(fdir, name)
        if os.path.isfile(p):
            return p
    return os.path.join(os.path.dirname(__file__), "fixtures", name)


class TestDocumentFixturesExist(unittest.TestCase):
    """Ensures test fixtures and companion *_md files are available when running in private workspace."""

    def test_fixtures_present(self):
        pairs = find_pdf_fixture_pairs()
        if not pairs:
            self.skipTest("Private personal fixtures are safely quarantined in private repository.")
        for pdf_path, companion_md in pairs:
            self.assertTrue(os.path.isfile(pdf_path), f"PDF fixture missing: {pdf_path}")
            if companion_md:
                self.assertTrue(
                    os.path.isfile(companion_md),
                    f"Companion MD missing: {companion_md}",
                )



class TestTableEdgeCases(unittest.TestCase):
    """Tests edge cases in pipe splitting, cell preservation, and sparse table detection."""

    def test_edge_case_leading_pipes_not_stripped(self):
        """Row starting with empty cells must retain all empty columns without shifting."""
        # Col 5 has fee; cols 1-4 are blank
        row = "|||||55.00 Autres taxes / Other taxes|"
        cells = split_pipe_row(row)
        self.assertEqual(len(cells), 5)
        self.assertEqual(cells[0], "")
        self.assertEqual(cells[1], "")
        self.assertEqual(cells[2], "")
        self.assertEqual(cells[3], "")
        self.assertEqual(cells[4], "55.00 Autres taxes / Other taxes")

    def test_edge_case_flight_route_column_positions(self):
        """Row ||MRS|CDG||||||| must keep MRS in col 2 and CDG in col 3."""
        row = "||MRS|CDG|||||||"
        cells = split_pipe_row(row)
        self.assertEqual(len(cells), 9)
        self.assertEqual(cells[0], "")
        self.assertEqual(cells[1], "MRS")
        self.assertEqual(cells[2], "CDG")
        for i in range(3, 9):
            self.assertEqual(cells[i], "")

    def test_edge_case_table_normalization_preserves_tax_column(self):
        """Normalizing a table with leading empty cells must NOT displace values to column 1."""
        raw_table = (
            "| Nom | Billet | Mode | HT | Taxes |\n"
            "| --- | --- | --- | --- | --- |\n"
            "| DUPONT JEAN | 0123456789012 | Card | EUR 575 | 39.15 Taxe |\n"
            "|||||55.00 Autres taxes / Other taxes|\n"
        )
        norm = normalize_markdown_tables(raw_table)
        lines = [l for l in norm.splitlines() if l.strip()]
        last_row = lines[-1]
        cells = split_pipe_row(last_row)
        self.assertEqual(len(cells), 5)
        self.assertIn("55.00 Autres taxes / Other taxes", cells[4])
        self.assertNotEqual(cells[0], "55.00 Autres taxes / Other taxes")

    def test_edge_case_sparse_table_detection_on_billet(self):
        """Quality gate must detect fragmented sparse rows when table columns are scrambled."""
        sparse_table_md = (
            "| Date | Dep | Arr | Flight | Time | Bag | Cabin | Class | Status |\n"
            "| :--- | :-- | :-- | :----- | :--- | :-- | :---- | :---- | :----- |\n"
            "| 28MAR | MRS | CDG | AF7331 | 09:35 | 1x23 | Eco | L | OK |\n"
            "| | MRS | CDG | | | | | | |\n"
            "| | 12:40 | 06:35 | | | | | | |\n"
            "| 28MAR | CDG | SGN | AF0258 | 11:40 | 1x23 | Eco | N | OK |\n"
            "| | 09:10 | 16:40 | | | | | | |\n"
            "| 08DEC | SGN | CDG | AF0253 | 08:10 | 1x23 | Eco | N | OK |\n"
            "| | 21:10 | 22:35 | | | | | | |\n"
        )
        check = check_ragged_tables(sparse_table_md)
        self.assertFalse(check.passed, "check_ragged_tables should have failed on fragmented table")
        self.assertIn("sparse rows", check.detail)

        passed, reasons = evaluate_quality_gate(sparse_table_md)
        self.assertFalse(passed)
        self.assertTrue(any("ragged-tables" in r for r in reasons))


class TestFormRecoveryEdgeCases(unittest.TestCase):
    """Tests multi-coupon and collapsed form table reconstruction."""

    def test_edge_case_coupon_collapse_recovery(self):
        """Collapsed coupon text in payment receipts must be reconstructed into a Markdown table."""
        collapsed_text = (
            "COUPON 1 / COUPON 1 NUMÉRO DE REÇU 012 345 678 901 2 \"O\" 1er bagage supplémentaire/ 1st additional baggage item "
            "1 bagage(s) Départ/ Departure MARSEILLE AÉROPORT PROVENCE Arrivée/ Arrival PARIS AÉROPORT CHARLES DE GAULLE "
            "Remarque/ Remark B1MRS AF X/PAR AF SGN80.00EUR80.00END Numéro de billet associé/ Associated ticket number 0123456789012"
        )
        recovered = recover_collapsed_form_lines(collapsed_text)
        self.assertIn("| **COUPON 1 / COUPON 1** |", recovered)
        self.assertIn("| Départ/ Departure | MARSEILLE AÉROPORT PROVENCE |", recovered)
        self.assertIn("| Numéro de billet associé/ Associated ticket number | 0123456789012 |", recovered)

    def test_edge_case_multiple_coupons_split_into_discrete_tables(self):
        """Multiple consecutive coupons must be separated by blank lines."""
        fused = (
            "| **COUPON 1 / COUPON 1** | NUMÉRO DE REÇU 012 345 678 901 2 |\n"
            "| --- | --- |\n"
            "| Départ/ Departure | MARSEILLE AÉROPORT PROVENCE |\n"
            "| **COUPON 2 / COUPON 2** | NUMÉRO DE REÇU 012 345 678 901 2 |\n"
            "| --- | --- |\n"
            "| Départ/ Departure | PARIS AÉROPORT CHARLES DE GAULLE |\n"
        )
        normalized = normalize_markdown_tables(fused)
        # Verify there is a double newline (blank line) between coupon 1 and coupon 2
        self.assertIn("\n\n| **COUPON 2 / COUPON 2** |", normalized)


class TestConversionRoutingEdgeCases(unittest.TestCase):
    """Tests routing logic and quality gate fallback on actual document bytes."""

    def test_edge_case_document_routes_to_docling_when_fast_path_disabled(self):
        """When fast path is disabled, conversion must route to Docling converter."""
        pairs = find_pdf_fixture_pairs()
        if not pairs:
            self.skipTest("No PDF fixtures found in test directory.")

        pdf_path, _ = pairs[0]
        with open(pdf_path, "rb") as f:
            pdf_bytes = f.read()

        mock_docling = MagicMock()
        mock_docling.convert.return_value = ConversionResult(
            checksum="fake",
            markdown="# Docling Fallback Success\n\nProper content.",
            text="Docling Fallback Success",
            raw_text="Docling Fallback Success",
            numpages=1,
            engine="docling-pdf",
        )

        service = ConversionService(docling_converter=mock_docling)
        req = ExtractionRequest(
            content=pdf_bytes,
            filename=os.path.basename(pdf_path),
            extension=".pdf",
            allow_fast_path=False,
            allow_vision_fallback=False,
        )

        res = service.convert_request(req)
        mock_docling.convert.assert_called_once()
        self.assertEqual(res.engine, "docling-pdf")

    @patch.object(FastPathConverterAdapter, "is_enabled", return_value=True)
    def test_fast_path_succeeds_and_matches_companion_md(self, _mock_fast_enabled):
        """Normalized fast path output must pass quality gate and match companion *_md in same folder."""
        pairs = find_pdf_fixture_pairs()
        if not pairs:
            self.skipTest("No PDF fixtures found in test directory.")

        service = ConversionService()
        for pdf_path, companion_md in pairs:
            with self.subTest(document=os.path.basename(pdf_path)):
                with open(pdf_path, "rb") as f:
                    pdf_bytes = f.read()

                req = ExtractionRequest(
                    content=pdf_bytes,
                    filename=os.path.basename(pdf_path),
                    extension=".pdf",
                    allow_fast_path=True,
                    allow_vision_fallback=False,
                )
                res = service.convert_request(req)
                self.assertIn(res.engine, ("pdf-oxide-fast-path", "pypdf-fast-path", "docling-pdf"))
                self.assertGreater(len(res.markdown.strip()), 20)

                # Output should pass quality gate evaluation
                passed, reasons = evaluate_quality_gate(res.markdown, pdf_path=pdf_path)
                self.assertTrue(passed, f"Quality gate failed for {pdf_path}: {reasons}")

                # If a companion *_md exists in the SAME folder of the PDF, validate against it
                if companion_md and os.path.isfile(companion_md):
                    with open(companion_md, "r", encoding="utf-8", errors="ignore") as f:
                        expected_md = f.read()

                    # Check that tables from companion md are preserved
                    if "|" in expected_md:
                        self.assertIn("|", res.markdown, f"Expected table syntax '|' in {pdf_path}")

                    # Check that prominent structural headings from companion md are captured
                    expected_headings = [
                        line.strip().lstrip("#").strip()
                        for line in expected_md.splitlines()
                        if line.startswith("#") and len(line.strip().lstrip("#").strip()) > 3
                    ]
                    for heading in expected_headings:
                        words = re.findall(r"\b\w{4,}\b", heading)
                        if words:
                            matched = any(w.lower() in res.markdown.lower() for w in words)
                            self.assertTrue(
                                matched,
                                f"Expected at least one heading keyword from '{heading}' in output of {pdf_path}",
                            )

    def test_edge_case_force_vision_bypasses_fast_path_and_docling(self):
        """Setting force_vision=True must immediately route to Vision rescue."""
        mock_docling = MagicMock()
        mock_fast = MagicMock()
        mock_vision = MagicMock()
        mock_vision.is_enabled.return_value = True
        mock_vision.rescue.return_value = ConversionResult(
            checksum="fake",
            markdown="# Rescued Ticket\n\n| Flight | Status |\n| AF7331 | OK |",
            text="Rescued Ticket",
            raw_text="Rescued Ticket",
            numpages=3,
            engine="vision:gemini-3.8-flash",
        )

        service = ConversionService(
            docling_converter=mock_docling,
            fast_path_converter=mock_fast,
            vision_rescue=mock_vision,
        )

        req = ExtractionRequest(
            content=b"%PDF-1.4 dummy",
            filename="ticket.pdf",
            extension=".pdf",
            force_vision=True,
        )

        res = service.convert_request(req)
        mock_vision.rescue.assert_called_once()
        mock_docling.convert.assert_not_called()
        mock_fast.convert.assert_not_called()
        self.assertEqual(res.engine, "vision:gemini-3.8-flash")

    def test_edge_case_filename_sanitization_with_accents(self):
        """Filename with French accents must sanitize safely."""
        name = "Billet-électronique.pdf"
        sanitized = sanitize_filename(name)
        self.assertTrue(sanitized.endswith(".pdf"))
        self.assertNotIn("\x00", sanitized)

    def test_edge_case_unsupported_file_extension(self):
        """Unsupported extensions must raise UnsupportedExtensionError."""
        service = ConversionService()
        req = ExtractionRequest(
            content=b"malicious executable",
            filename="malware.exe",
            extension=".exe",
        )
        with self.assertRaises(UnsupportedExtensionError):
            service.convert_request(req)

    def test_image_extensions_classified_correctly(self):
        """Image extensions must be recognized as supported and classified as IMAGE."""
        from src.domain.rules import classify_format, is_supported_extension, IMAGE_EXTENSIONS
        from src.domain.model import DocumentFormat

        for ext in IMAGE_EXTENSIONS:
            self.assertTrue(is_supported_extension(ext), f"{ext} should be supported")
            self.assertEqual(classify_format(ext), DocumentFormat.IMAGE, f"{ext} should be DocumentFormat.IMAGE")

    def test_image_conversion_routing_to_docling_image(self):
        """Image requests must route through docling converter with docling-image engine."""
        mock_docling = MagicMock()
        mock_docling.convert.return_value = ConversionResult(
            checksum="",
            markdown="## Extracted Image Text\n\nSome photo content.",
            text="Extracted Image Text",
            raw_text="Extracted Image Text",
            numpages=1,
            engine="docling-image",
        )

        service = ConversionService(
            docling_converter=mock_docling,
            vision_rescue=MagicMock(),
        )

        req = ExtractionRequest(
            content=b"\xff\xd8\xff\xe0 dummy jpeg bytes",
            filename="84 cau truc tieng anh thong dung (10).jpg",
            extension=".jpg",
            allow_fast_path=False,
        )
        res = service.convert_request(req)
        mock_docling.convert.assert_called_once()
        self.assertEqual(res.engine, "docling-image")
        self.assertIn("Extracted Image Text", res.markdown)

    def test_image_auto_routes_to_vision_when_enabled(self):
        """Auto mode should route image to Vision LLM when Vision is enabled."""
        mock_vision = MagicMock()
        mock_vision.is_enabled.return_value = True
        mock_vision.rescue.return_value = ConversionResult(
            checksum="",
            markdown="# High Quality Vision Result\n\nCấu trúc tiếng Anh thông dụng.",
            text="High Quality Vision Result",
            raw_text="High Quality Vision Result",
            numpages=1,
            engine="vision:gemini-3.8-flash",
        )

        service = ConversionService(
            docling_converter=MagicMock(),
            vision_rescue=mock_vision,
        )

        req = ExtractionRequest(
            content=b"\xff\xd8\xff\xe0 dummy jpeg bytes",
            filename="84 cau truc tieng anh thong dung (2).jpg",
            extension=".jpg",
            allow_fast_path=True,
        )
        res = service.convert_request(req)
        mock_vision.rescue.assert_called_once()
        self.assertEqual(res.engine, "vision:gemini-3.8-flash")

    def test_image_force_vision_rescue(self):
        """Forcing vision on an image must route directly to Vision rescue."""
        mock_vision = MagicMock()
        mock_vision.is_enabled.return_value = True
        mock_vision.rescue.return_value = ConversionResult(
            checksum="",
            markdown="# Vision Rescued Image\n\nAccurate table content.",
            text="Vision Rescued Image",
            raw_text="Vision Rescued Image",
            numpages=1,
            engine="vision:gemini-3.8-flash",
        )

        service = ConversionService(
            docling_converter=MagicMock(),
            vision_rescue=mock_vision,
        )

        req = ExtractionRequest(
            content=b"\x89PNG dummy png bytes",
            filename="infographic.png",
            extension=".png",
            force_vision=True,
        )
        res = service.convert_request(req)
        mock_vision.rescue.assert_called_once()
        self.assertEqual(res.engine, "vision:gemini-3.8-flash")


if __name__ == "__main__":
    unittest.main()
