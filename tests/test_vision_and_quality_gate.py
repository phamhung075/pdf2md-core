"""Unit tests for Quality Gate and Vision LLM Rescue fallback."""
import json
import os
import unittest
from unittest.mock import MagicMock, patch

from src.domain.model import ConversionResult, ExtractionRequest
from src.domain.quality_gate import (
    check_mojibake,
    check_no_text_or_image_placeholder,
    check_ragged_tables,
    evaluate_quality_gate,
)
from src.application.conversion_service import ConversionService
from src.infrastructure.converters.vision_gemini_adapter import VisionGeminiAdapter


class TestQualityGate(unittest.TestCase):
    """Tests for the domain quality gate heuristics."""

    def test_empty_or_image_placeholder_fails(self):
        # Case 1: Docling classified whole page as an image placeholder
        bad_md = "<!-- image -->\n<!-- image -->"
        res = check_no_text_or_image_placeholder(bad_md)
        self.assertFalse(res.passed)

        # Case 2: Almost no visible characters
        res2 = check_no_text_or_image_placeholder("abc   \n")
        self.assertFalse(res2.passed)

        # Case 3: Sufficient text
        good_md = "# Title\nThis is a legitimate document with sufficient content."
        res3 = check_no_text_or_image_placeholder(good_md)
        self.assertTrue(res3.passed)

    def test_vietnamese_text_passes_mojibake_check(self):
        # Document containing Vietnamese diacritics
        vn_text = (
            "Cộng hòa Xã hội Chủ nghĩa Việt Nam. Độc lập - Tự do - Hạnh phúc. "
            "Hóa đơn giá trị gia tăng số 12345. Tiền thuế GTGT: 100.000 VNĐ. "
            "Người mua hàng: Nguyễn Văn A. Địa chỉ: Hà Nội, Việt Nam. "
            "Các mặt hàng: đường, sữa, cà phê, nước giải khát, bánh mì. "
            "Xin cảm ơn quý khách đã sử dụng dịch vụ của chúng tôi!"
        )
        res = check_mojibake(vn_text)
        self.assertTrue(res.passed)

    def test_mojibake_broken_decode_fails(self):
        # CMap decoding failure (e.g. Hangul syllables or garbage)
        mojibake_text = " ".join(["가나다라마바사아자차카타파하"] * 55)
        res = check_mojibake(mojibake_text)
        self.assertFalse(res.passed)

    def test_ragged_tables_detection(self):
        # Clean table
        clean_table = """
| Name | Age | City |
| :--- | :-- | :--- |
| Alice | 25 | Hanoi |
| Bob | 30 | Paris |
| Charlie | 35 | London |
| David | 28 | Tokyo |
| Eve | 22 | New York |
| Frank | 40 | Berlin |
| Grace | 33 | Rome |
"""
        res_clean = check_ragged_tables(clean_table)
        self.assertTrue(res_clean.passed)

        # Broken/ragged table (inconsistent columns in majority of rows)
        ragged_table = """
| Col1 | Col2 | Col3 | Col4 |
| :--- | :--- | :--- | :--- |
| A | B |
| C | D | E |
| F |
| G | H |
| I | J | K |
| L | M |
| N |
"""
        res_ragged = check_ragged_tables(ragged_table)
        self.assertFalse(res_ragged.passed)

    def test_normalize_markdown_tables(self):
        from src.domain.quality_gate import normalize_markdown_tables
        raw_table = """
| Item | Qty | Price |
| :--- | :-- | :---- |
| Coffee | 1 | 5.00 |
| Subtotal: 5.00 |
| Tax: 0.50 |
| Total Paid: 5.50 |
| Thank you! |
| Payment Method: Card |
"""
        # Before normalization, ragged rows fail the check
        res_before = check_ragged_tables(raw_table)
        self.assertFalse(res_before.passed)

        # After normalization, missing columns are padded with empty cells
        normalized = normalize_markdown_tables(raw_table)
        res_after = check_ragged_tables(normalized)
        self.assertTrue(res_after.passed)
        self.assertIn("| Total Paid: 5.50 |  |  |", normalized)

    def test_normalize_markdown_tables_splits_fused_tables(self):
        from src.domain.quality_gate import normalize_markdown_tables
        fused_table = """
|**COUPON  1 / COUPON  1**|NUMÉRO DE REÇU057 151 132 262 3|
|---|---|
|"O" 1er bagage supplémentaire/ 1st additional baggage item|1 bagage(s)|
|Départ/ Departure|MARSEILLE AÉROPORT PROVENCE|
|Arrivée/ Arrival|PARIS AÉROPORT CHARLES DE GAULLE|
|**COUPON  2 / COUPON  2**|NUMÉRO DE REÇU057 151 132 262 3|
|"O" 1er bagage supplémentaire/ 1st additional baggage item|1 bagage(s)|
|Départ/ Departure|PARIS AÉROPORT CHARLES DE GAULLE|
|Arrivée/ Arrival|HO CHI MINH VILLE TAN SON NHAT AIRPORT|
|**COUPON  1 / COUPON  1**|NUMÉRO DE REÇU057 151 132 262 4|
|"O" 1er bagage supplémentaire/ 1st additional baggage item|1 bagage(s)|
|Départ/ Departure|HO CHI MINH VILLE TAN SON NHAT AIRPORT|
|Arrivée/ Arrival|PARIS AÉROPORT CHARLES DE GAULLE|
"""
        normalized = normalize_markdown_tables(fused_table)
        # Verify that the tables are separated by blank lines and each has its own separator row
        self.assertIn("| **COUPON  1 / COUPON  1** | NUMÉRO DE REÇU057 151 132 262 3 |", normalized)
        self.assertIn("| **COUPON  2 / COUPON  2** | NUMÉRO DE REÇU057 151 132 262 3 |", normalized)
        self.assertIn("| **COUPON  1 / COUPON  1** | NUMÉRO DE REÇU057 151 132 262 4 |", normalized)
        # Should contain at least 3 separator rows (| --- | --- |)
        self.assertEqual(normalized.count("| --- | --- |"), 3)
        # Should have blank line separators between tables
        table_blocks = [b for b in normalized.strip().split("\n\n") if b.strip().startswith("|")]
        self.assertEqual(len(table_blocks), 3)

    def test_recover_collapsed_form_lines(self):
        from src.domain.quality_gate import normalize_markdown_tables
        raw_text = (
            'PASSAGER(S) TRAN MINH PHUC MR\n\n'
            '**COUPON  2 / COUPON  2** NUMÉRO DE REÇU 057 151 132 262 4 '
            '"O" 1er bagage supplémentaire / 1st additional baggage item 1 bagage(s) '
            '<mark>Départ / Departure</mark> PARIS AÉROPORT CHARLES DE GAULLE '
            '<mark>Arrivée / Arrival</mark> MARSEILLE AÉROPORT PROVENCE '
            '<mark>Remarque / Remark</mark> AF7342 08DEC23 CDG MRSSGN AF X/PAR AF MRS80.00EUR80.00END '
            '<mark>Numéro de billet associé / Associated ticket number</mark> 0571485844905\n\n'
            '### REÇU DE PAIEMENT'
        )
        normalized = normalize_markdown_tables(raw_text)
        self.assertIn("| **COUPON  2 / COUPON  2** | NUMÉRO DE REÇU 057 151 132 262 4 |", normalized)
        self.assertIn("| Départ / Departure | PARIS AÉROPORT CHARLES DE GAULLE |", normalized)
        self.assertIn("| Arrivée / Arrival | MARSEILLE AÉROPORT PROVENCE |", normalized)
        self.assertIn("| Numéro de billet associé / Associated ticket number | 0571485844905 |", normalized)
        self.assertIn("| --- | --- |", normalized)

    def test_evaluate_quality_gate(self):
        passed, reasons = evaluate_quality_gate("# Title\nValid content with tables and text.")
        self.assertTrue(passed)
        self.assertEqual(len(reasons), 0)

        passed, reasons = evaluate_quality_gate("<!-- image -->")
        self.assertFalse(passed)
        self.assertTrue(any("no-text-or-image-placeholder" in r for r in reasons))


class TestConversionServiceRescue(unittest.TestCase):
    """Tests for ConversionService orchestration with Vision fallback."""

    def setUp(self):
        self.mock_docling = MagicMock()
        self.mock_fast_path = MagicMock()
        self.mock_fast_path.is_enabled.return_value = False
        self.mock_vision = MagicMock()

        self.service = ConversionService(
            docling_converter=self.mock_docling,
            fast_path_converter=self.mock_fast_path,
            vision_rescue=self.mock_vision,
        )

    def test_quality_pass_does_not_invoke_vision_rescue(self):
        self.mock_docling.convert.return_value = ConversionResult(
            checksum="",
            markdown="# Valid Invoice\nAmount: 1,500 USD with all details clearly readable.",
            text="Valid Invoice Amount: 1,500 USD",
            raw_text="Valid Invoice Amount: 1,500 USD",
            numpages=1,
            engine="docling-pdf",
        )
        self.mock_vision.is_enabled.return_value = True

        req = ExtractionRequest(
            content=b"%PDF-1.4 dummy",
            filename="invoice.pdf",
            extension=".pdf",
        )
        result = self.service.convert_request(req)

        self.assertEqual(result.engine, "docling-pdf")
        self.mock_vision.rescue.assert_not_called()

    def test_fast_path_quality_fail_falls_through_to_docling(self):
        self.mock_fast_path.is_enabled.return_value = True
        self.mock_fast_path.is_digital.return_value = True
        # Fast path returned broken output that fails quality gate
        self.mock_fast_path.convert.return_value = ConversionResult(
            checksum="",
            markdown="<!-- image -->",
            text="",
            raw_text="",
            numpages=1,
            engine="pypdf-fast-path",
        )
        # Docling succeeds and produces valid output
        self.mock_docling.convert.return_value = ConversionResult(
            checksum="",
            markdown="# Docling Clean Table\n\n| A | B |\n|---|---|\n| 1 | 2 |",
            text="Docling Clean Table",
            raw_text="Docling Clean Table",
            numpages=1,
            engine="docling-pdf",
        )
        self.mock_vision.is_enabled.return_value = True

        req = ExtractionRequest(
            content=b"%PDF-1.4 dummy",
            filename="receipt.pdf",
            extension=".pdf",
        )
        result = self.service.convert_request(req)

        self.assertEqual(result.engine, "docling-pdf")
        self.assertIn("Docling Clean Table", result.markdown)
        self.mock_docling.convert.assert_called_once()
        self.mock_vision.rescue.assert_not_called()

    def test_quality_fail_triggers_vision_rescue(self):
        # Docling dropped text and returned only an image placeholder
        self.mock_docling.convert.return_value = ConversionResult(
            checksum="",
            markdown="<!-- image -->",
            text="",
            raw_text="",
            numpages=1,
            engine="docling-pdf",
        )
        self.mock_vision.is_enabled.return_value = True
        self.mock_vision.rescue.return_value = ConversionResult(
            checksum="",
            markdown="# Rescued Invoice\nScanned content successfully recovered by Vision LLM.",
            text="Rescued Invoice",
            raw_text="Rescued Invoice",
            numpages=1,
            engine="vision:gemini-flash-latest",
            info={"rescued_by_vision": True},
        )

        req = ExtractionRequest(
            content=b"%PDF-1.4 dummy",
            filename="scan_bad.pdf",
            extension=".pdf",
        )
        result = self.service.convert_request(req)

        self.mock_vision.rescue.assert_called_once()
        self.assertEqual(result.engine, "vision:gemini-flash-latest")
        self.assertIn("original_engine", result.info)
        self.assertEqual(result.info["original_engine"], "docling-pdf")
        self.assertIn("quality_gate_reasons", result.info)

    def test_force_vision_bypasses_docling(self):
        self.mock_vision.is_enabled.return_value = True
        self.mock_vision.rescue.return_value = ConversionResult(
            checksum="",
            markdown="# Force Vision\nTranscribed directly.",
            text="Force Vision",
            raw_text="Force Vision",
            numpages=1,
            engine="vision:gemini-flash-latest",
        )

        req = ExtractionRequest(
            content=b"%PDF-1.4 dummy",
            filename="scan.pdf",
            extension=".pdf",
            force_vision=True,
        )
        result = self.service.convert_request(req)

        self.assertEqual(result.engine, "vision:gemini-flash-latest")
        self.mock_docling.convert.assert_not_called()
        self.mock_vision.rescue.assert_called_once()

    def test_vision_failure_falls_back_to_docling(self):
        self.mock_docling.convert.return_value = ConversionResult(
            checksum="",
            markdown="<!-- image -->",
            text="",
            raw_text="",
            numpages=1,
            engine="docling-pdf",
        )
        self.mock_vision.is_enabled.return_value = True
        self.mock_vision.rescue.side_effect = RuntimeError("Network timeout or API rate limit")

        req = ExtractionRequest(
            content=b"%PDF-1.4 dummy",
            filename="scan_bad.pdf",
            extension=".pdf",
        )
        # Should gracefully return original docling output instead of crashing
        result = self.service.convert_request(req)

        self.assertEqual(result.engine, "docling-pdf")
        self.assertEqual(result.markdown, "<!-- image -->")


class TestVisionGeminiAdapter(unittest.TestCase):
    """Tests for VisionGeminiAdapter API request formation and response parsing."""

    def test_gemini_api_call_success(self):
        adapter = VisionGeminiAdapter()
        mock_response = {
            "candidates": [
                {
                    "content": {
                        "parts": [
                            {"text": "# Hóa đơn giá trị gia tăng\n\n| Hàng hóa | Giá |\n| :--- | :--- |\n| Cà phê | 50.000 |"}
                        ]
                    }
                }
            ]
        }
        with patch("urllib.request.urlopen") as mock_urlopen:
            mock_resp = MagicMock()
            mock_resp.read.return_value = json.dumps(mock_response).encode("utf-8")
            mock_urlopen.return_value.__enter__.return_value = mock_resp

            result = adapter._call_gemini_api("dummy_base64_image")
            self.assertIn("# Hóa đơn giá trị gia tăng", result)
            self.assertIn("Cà phê", result)

    def test_markdown_fence_cleaning(self):
        adapter = VisionGeminiAdapter()
        with patch.object(adapter, "_call_gemini_api", return_value="```markdown\n# Clean Title\nContent\n```"):
            clean = adapter._transcribe_image("dummy_b64")
            self.assertEqual(clean, "# Clean Title\nContent")

    def test_retry_on_503_success(self):
        adapter = VisionGeminiAdapter()
        mock_response = {
            "candidates": [
                {"content": {"parts": [{"text": "# Rescued After 503"}]}}
            ]
        }
        mock_resp_success = MagicMock()
        mock_resp_success.read.return_value = json.dumps(mock_response).encode("utf-8")

        import urllib.error
        import io
        err_503 = urllib.error.HTTPError(
            url="http://test",
            code=503,
            msg="Service Unavailable",
            hdrs={},
            fp=io.BytesIO(b"Model overloaded"),
        )

        with patch("time.sleep") as mock_sleep, patch("urllib.request.urlopen") as mock_urlopen:
            mock_urlopen.side_effect = [
                err_503,
                MagicMock(__enter__=MagicMock(return_value=mock_resp_success)),
            ]
            result = adapter._call_gemini_api("dummy_b64", page_num=1)
            self.assertEqual(result, "# Rescued After 503")
            mock_sleep.assert_called_once()

    def test_retry_exhausted_raises(self):
        adapter = VisionGeminiAdapter()
        import urllib.error
        import io
        err_503 = urllib.error.HTTPError(
            url="http://test",
            code=503,
            msg="Service Unavailable",
            hdrs={},
            fp=io.BytesIO(b"Model overloaded"),
        )

        with patch("time.sleep"), patch("urllib.request.urlopen") as mock_urlopen:
            mock_urlopen.side_effect = err_503
            with self.assertRaises(urllib.error.HTTPError):
                adapter._call_gemini_api("dummy_b64", page_num=1)

    def test_partial_page_fallback(self):
        adapter = VisionGeminiAdapter()
        mock_pdfium = MagicMock()
        mock_doc = MagicMock()
        mock_doc.__len__.return_value = 2

        page0 = MagicMock()
        page1 = MagicMock()
        page1.get_textpage.return_value.get_text_range.return_value = "Extracted plain text for page 2"
        mock_doc.__getitem__.side_effect = [page0, page1]
        mock_pdfium.PdfDocument.return_value = mock_doc

        def fake_render(item):
            p_num, _ = item
            if p_num == 0:
                return 0, "# Page 1 Markdown"
            raise RuntimeError("API rate limit exhausted for page 2")

        import src.infrastructure.converters.vision_gemini_adapter as vga
        with patch.object(vga, "_HAVE_PDFIUM", True), \
             patch.object(vga, "pypdfium2", mock_pdfium), \
             patch.object(adapter, "_render_and_transcribe_page", side_effect=fake_render):
            result = adapter.rescue("dummy.pdf", "dummy.pdf")
            self.assertIn("# Page 1 Markdown", result.markdown)
            self.assertEqual(result.info["failed_pages"], [2])


class TestTableNormalizationAndSparseGate(unittest.TestCase):
    """Tests for pipe splitting, column preservation, and sparse table detection."""

    def test_split_pipe_row_preserves_empty_columns(self):
        from src.domain.quality_gate import split_pipe_row, normalize_markdown_tables
        # Ensure leading and trailing empty columns are preserved
        row = "|||||55.00 Autres taxes / Other taxes|"
        cells = split_pipe_row(row)
        self.assertEqual(len(cells), 5)
        self.assertEqual(cells[4], "55.00 Autres taxes / Other taxes")
        self.assertEqual(cells[0], "")

        # Test normalization preserves column alignment instead of shifting left
        table = (
            "| Col1 | Col2 | Col3 | Col4 | Col5 |\n"
            "| --- | --- | --- | --- | --- |\n"
            "|||||55.00 Autres taxes / Other taxes|\n"
        )
        norm = normalize_markdown_tables(table)
        norm_lines = [l for l in norm.splitlines() if l.strip()]
        self.assertIn("|  |  |  |  | 55.00 Autres taxes / Other taxes |", norm_lines[2])

    def test_sparse_tables_detection(self):
        # Table with excessive empty/ghost rows from fragmented cell extraction
        sparse_table = (
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
        res = check_ragged_tables(sparse_table)
        self.assertFalse(res.passed)
        self.assertIn("sparse rows", res.detail)

    def test_canvas_table_grid_detection(self):
        from src.domain.quality_gate import detect_canvas_table_grids

        # Helper to find private fixtures if present
        def _find_fixture(name: str):
            candidates = [
                os.environ.get("TEST_FIXTURES_DIR"),
                os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(__file__))), "tests", "fixtures"),
                os.path.join(os.path.dirname(__file__), "fixtures"),
                "/tmp/fixtures",
            ]
            for c in candidates:
                if c and os.path.isdir(c):
                    p = os.path.join(c, name)
                    if os.path.isfile(p):
                        return p
            return None

        # Test on billet_electronique.pdf
        billet = _find_fixture("billet_electronique.pdf")
        if not billet:
            self.skipTest("Private personal fixture not present (quarantined to private repository).")

        billet_grids = detect_canvas_table_grids(billet)
        self.assertGreaterEqual(len(billet_grids), 2)
        # Verify receipt table detected on page 2
        p2_grids = [g for g in billet_grids if g.page_number == 2]
        self.assertGreaterEqual(len(p2_grids), 1)
        self.assertIn("nom", p2_grids[0].words)
        self.assertIn("billet", p2_grids[0].words)

        # Test on payment_receipt.pdf
        receipt = _find_fixture("payment_receipt.pdf")
        if receipt:
            receipt_grids = detect_canvas_table_grids(receipt)
            self.assertGreaterEqual(len(receipt_grids), 1)
            self.assertIn("nom", receipt_grids[0].words)
            self.assertIn("reçu", receipt_grids[0].words)

    def test_lost_table_capture_canvas_detection(self):
        from src.domain.quality_gate import check_lost_table_capture

        def _find_fixture(name: str):
            candidates = [
                os.environ.get("TEST_FIXTURES_DIR"),
                os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(__file__))), "tests", "fixtures"),
                os.path.join(os.path.dirname(__file__), "fixtures"),
                "/tmp/fixtures",
            ]
            for c in candidates:
                if c and os.path.isdir(c):
                    p = os.path.join(c, name)
                    if os.path.isfile(p):
                        return p
            return None

        billet = _find_fixture("billet_electronique.pdf")
        if not billet:
            self.skipTest("Private personal fixture not present (quarantined to private repository).")

        # Missing table in markdown should fail canvas quality check
        missing_table_md = "# Electronic Ticket\n\nSome plain text without pipe tables."
        res_missing = check_lost_table_capture(missing_table_md, pdf_path=billet)
        self.assertFalse(res_missing.passed)
        self.assertIn("Lost table capture: 2D canvas table", res_missing.detail)

        # Properly captured markdown tables should pass canvas check
        captured_table_md = (
            "| AVANT VOTRE DÉPART | Site internet Air France, rubrique Vos réservations | Air France website, “Your Reservations” section |\n"
            "| --- | --- | --- |\n"
            "| BEFORE YOUR FLIGHT | Par téléphone au +33 (0)9 69 39 36 54 | Pour consulter, compléter ou modifier votre réservation (si |\n\n"
            "| Date | Départ | Arrivée | Vol | Fin enregistrement | Total bagages | Cabine |\n"
            "| --- | --- | --- | --- | --- | --- | --- |\n"
            "| 28MAR | 10:05 Marseille MRS Aéroport Provence 1B | 11:30 Paris CDG Aéroport Charles de Gaulle 2F | AF7335 | 09:45 | 2PC | ECONOMY |\n"
            "| 28MAR | 12:40 Paris CDG Aéroport Charles de Gaulle 2E | 06:35 Ho Chi Minh Ville SGN Tan Son Nhat Airport 2 | AF0258 | 11:40 | 2PC | ECONOMY |\n"
            "| 08DEC | 09:10 Ho Chi Minh Ville SGN Tan Son Nhat Airport 2 | 16:40 Paris CDG Aéroport Charles de Gaulle 2E | AF0253 | 08:10 | 2PC | ECONOMY |\n"
            "| 08DEC | 21:10 Paris CDG Aéroport Charles de Gaulle 2F | 22:35 Marseille MRS Aéroport Provence 1B | AF7342 | 20:25 | 2PC | ECONOMY |\n\n"
            "| Nom | Numéro de billet | Mode de paiement | Tarif HT | Taxes, surcharge transporteur | Montant total |\n"
            "| --- | --- | --- | --- | --- | --- |\n"
            "| TRAN MINH PHUC MR | 057 148 584 490 5 | Carte Master/Eurocard | EUR 575.00 | EUR 315.15 | EUR 890.15 |\n"
        )
        res_captured = check_lost_table_capture(captured_table_md, pdf_path=billet)
        self.assertTrue(res_captured.passed)

    def test_recover_lost_canvas_tables(self):
        from src.domain.quality_gate import recover_lost_canvas_tables, evaluate_quality_gate

        def _find_fixture(name: str):
            candidates = [
                os.environ.get("TEST_FIXTURES_DIR"),
                os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(__file__))), "tests", "fixtures"),
                os.path.join(os.path.dirname(__file__), "fixtures"),
                "/tmp/fixtures",
            ]
            for c in candidates:
                if c and os.path.isdir(c):
                    p = os.path.join(c, name)
                    if os.path.isfile(p):
                        return p
            return None

        billet = _find_fixture("billet_electronique.pdf")
        if not billet:
            self.skipTest("Private personal fixture not present (quarantined to private repository).")

        # Markdown where page 1 flight table is captured, but page 2 receipt table was dumped as scrambled text
        partial_md = (
            "# ELECTRONIC TICKET\n\n"
            "| AVANT VOTRE DÉPART | Site internet Air France, rubrique Vos réservations | Air France website, “Your Reservations” section |\n"
            "| --- | --- | --- |\n"
            "| BEFORE YOUR FLIGHT | Par téléphone au +33 (0)9 69 39 36 54 | Pour consulter, compléter ou modifier votre réservation (si |\n\n"
            "| Date | Départ | Arrivée | Vol | Fin enregistrement | Total bagages | Cabine |\n"
            "| --- | --- | --- | --- | --- | --- | --- |\n"
            "| 28MAR | MRS | CDG | AF7331 | 09:35 | 1x23 Kg | Economy |\n"
            "| 28MAR | CDG | SGN | AF0258 | 11:40 | 1x23 Kg | Economy |\n"
            "| 08DEC | SGN | CDG | AF0253 | 08:10 | 1x23 Kg | Economy |\n"
            "| 08DEC | CDG | MRS | AF7342 | 20:30 | 1x23 Kg | Economy |\n\n"
            "## DÉTAIL DU PRIX\n\n"
            "Nom Numéro de billet Mode de paiement\n"
            "TRAN MINH PHUC MR 057 148 584 490 5 Carte Master/Eurocard\n"
            "Tarif HT Taxes Montant total\n"
            "EUR 575.00 EUR 315.15 EUR 890.15\n"
        )
        # Without recovery, lost-table-capture fails because page 2 table is not in pipe markdown
        passed_before, reasons_before = evaluate_quality_gate(partial_md, pdf_path=billet)
        self.assertFalse(passed_before)
        self.assertTrue(any("lost-table-capture" in r for r in reasons_before))

        # After recovery, the canvas table is reconstructed
        recovered_md = recover_lost_canvas_tables(partial_md, pdf_path=billet)
        self.assertIn("|", recovered_md)
        self.assertIn("TRAN MINH PHUC", recovered_md)

        # Quality gate should now pass
        passed_after, reasons_after = evaluate_quality_gate(recovered_md, pdf_path=billet)
        self.assertTrue(passed_after, f"Expected quality gate to pass, got reasons: {reasons_after}")
        self.assertTrue(passed_after, f"Expected quality gate to pass, got reasons: {reasons_after}")

    def test_degraded_vietnamese_ocr_quality_gate(self):
        """Quality gate must detect degraded mixed English/Vietnamese OCR output."""
        from src.domain.quality_gate import check_degraded_ocr, evaluate_quality_gate

        degraded_md = (
            "cau trúc tiếng Anh thong dung\n"
            "Cau Tréc 02\n"
            "S+V + so tadj/ adv + that + S+V\n\n"
            "(Quá... đến nỗi mas.)\n\n"
            "VD: This box is so heavy that I cannot take it"
        )
        res = check_degraded_ocr(degraded_md)
        self.assertFalse(res.passed)
        self.assertIn("degraded-ocr", res.check_id)

        clean_md = (
            "# Cấu trúc tiếng Anh thông dụng\n\n"
            "## Cấu Trúc 02\n\n"
            "S + V + so + adj/adv + that + S + V\n\n"
            "(Quá... đến nỗi mà...)\n\n"
            "VD: This box is so heavy that I cannot take it"
        )
        res_clean = check_degraded_ocr(clean_md)
        self.assertTrue(res_clean.passed)


if __name__ == "__main__":
    unittest.main()
