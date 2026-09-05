"""Unit and integration tests for FastAPI Web Gateway and Celery Queue."""
import io
import os
import unittest
from unittest.mock import MagicMock, patch

from starlette.testclient import TestClient

from src.domain.model import ConversionResult
from src.interfaces.http.gateway import app


class TestFastApiGateway(unittest.TestCase):
    """Test suite for FastAPI Web Gateway endpoints and security rules."""

    @classmethod
    def setUpClass(cls):
        cls.client = TestClient(app)

    def test_health_endpoint_contract(self):
        """GET /health must return 200 with engine metadata and security headers."""
        resp = self.client.get("/health")
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertEqual(data["status"], "ok")
        self.assertIn("service", data)
        self.assertIn("visionFallback", data)
        self.assertIn("uptimeSec", data)

        # Standard Security Headers check
        self.assertEqual(resp.headers.get("x-content-type-options"), "nosniff")
        self.assertEqual(resp.headers.get("x-frame-options"), "SAMEORIGIN")
        self.assertEqual(resp.headers.get("referrer-policy"), "no-referrer")

    def test_root_metadata_contract(self):
        """GET / must list active API endpoints and configuration."""
        resp = self.client.get("/")
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertEqual(data["status"], "ok")
        self.assertIn("endpoints", data)
        self.assertIn("extract", data["endpoints"])
        self.assertIn("sync_v1", data["endpoints"])

    def test_extract_rejects_empty_payload(self):
        """POST /extract with empty content must return 400 Bad Request."""
        resp = self.client.post("/extract", content=b"", headers={"X-File-Name": "doc.pdf"})
        self.assertEqual(resp.status_code, 400)

    def test_extract_rejects_unsupported_media_type(self):
        """POST /extract with unsupported extension must return 415."""
        resp = self.client.post("/extract", content=b"dummy executable", headers={"X-File-Name": "malicious.exe"})
        self.assertEqual(resp.status_code, 415)

    @patch("src.interfaces.http.gateway._conversion_service.convert_request")
    def test_extract_binary_payload_success(self, mock_convert):
        """POST /extract accepts raw PDF binary stream and returns Markdown."""
        mock_convert.return_value = ConversionResult(
            checksum="abc123hash",
            markdown="# Document Title\n\nExtracted content.",
            text="Document Title Extracted content.",
            raw_text="Document Title Extracted content.",
            numpages=1,
            engine="pdf-oxide-fast-path",
            duration_ms=42,
            info={"title": "test.pdf"},
        )

        dummy_pdf = b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>%%EOF"
        resp = self.client.post(
            "/extract",
            content=dummy_pdf,
            headers={"X-File-Name": "test.pdf", "Content-Type": "application/pdf"},
        )
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertIn("# Document Title", data["markdown"])
        self.assertEqual(data["engine"], "pdf-oxide-fast-path")
        self.assertEqual(data["duration_ms"], 42)

    @patch("src.interfaces.http.gateway._conversion_service.convert_request")
    def test_extract_multipart_payload_success(self, mock_convert):
        """POST /extract accepts multipart/form-data upload."""
        mock_convert.return_value = ConversionResult(
            checksum="multi123hash",
            markdown="# Multipart Document",
            text="Multipart Document",
            raw_text="Multipart Document",
            numpages=2,
            engine="docling-pdf",
            duration_ms=120,
            info={"title": "upload.pdf"},
        )

        file_payload = io.BytesIO(b"%PDF-1.4 fake bytes")
        resp = self.client.post(
            "/extract",
            files={"file": ("upload.pdf", file_payload, "application/pdf")},
        )
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertIn("# Multipart Document", data["markdown"])

    @patch("src.interfaces.http.gateway._conversion_service.convert_request")
    def test_v1_convert_endpoint_success(self, mock_convert):
        """POST /v1/convert executes synchronous conversion."""
        mock_convert.return_value = ConversionResult(
            checksum="table123hash",
            markdown="| Col 1 | Col 2 |\n| --- | --- |\n| A | B |",
            text="Col 1 Col 2 A B",
            raw_text="Col 1 Col 2 A B",
            numpages=1,
            engine="pdf-oxide-fast-path",
            duration_ms=18,
            info={"title": "table.pdf"},
        )

        dummy_pdf = b"%PDF-1.4 table test"
        resp = self.client.post(
            "/v1/convert",
            content=dummy_pdf,
            headers={"X-File-Name": "table.pdf"},
        )
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertTrue(data["success"])
        self.assertIn("| Col 1 | Col 2 |", data["markdown"])

    @patch("src.interfaces.http.gateway._conversion_service.convert_request")
    def test_v1_jobs_creation_and_sync_fallback(self, mock_convert):
        """POST /v1/jobs submits document and returns job identifier."""
        mock_convert.return_value = ConversionResult(
            checksum="async123hash",
            markdown="# Background Result",
            text="Background Result",
            raw_text="Background Result",
            numpages=1,
            engine="pdf-oxide-fast-path",
            duration_ms=15,
        )

        dummy_pdf = b"%PDF-1.4 async test"
        resp = self.client.post(
            "/v1/jobs",
            content=dummy_pdf,
            headers={"X-File-Name": "async.pdf"},
        )
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertIn("job_id", data)
        self.assertIn(data["status"], ("PENDING", "COMPLETED"))

    def test_keycloak_jwt_validation_when_enabled(self):
        """When KEYCLOAK_ENABLED=1, requests without valid token must be rejected (401)."""
        with patch("src.interfaces.http.gateway.KEYCLOAK_ENABLED", True):
            resp = self.client.post("/v1/convert", content=b"%PDF-1.4 dummy", headers={"X-File-Name": "doc.pdf"})
            self.assertEqual(resp.status_code, 401)


if __name__ == "__main__":
    unittest.main()
