"""Comprehensive security tests for markdown-extract-service hardening."""
import io
import json
import os
import unittest
from unittest.mock import MagicMock, patch

from src.application.conversion_service import conversion_service
from src.domain.rules import extension_of, sanitize_filename
from src.infrastructure.config import ServiceConfig
from src.infrastructure.converters.vision_gemini_adapter import VisionGeminiAdapter
from src.infrastructure.dev_assets import get_dev_asset
from src.interfaces.http.handler import ExtractServiceHandler


class TestFilenameSanitization(unittest.TestCase):
    """Verifies that filename sanitization prevents path traversal and log injection."""

    def test_path_traversal_stripped(self):
        self.assertEqual(sanitize_filename("../../etc/passwd"), "passwd")
        self.assertEqual(sanitize_filename("..\\..\\windows\\system32\\calc.exe"), "calc.exe")
        self.assertEqual(sanitize_filename("/absolute/path/doc.pdf"), "doc.pdf")
        self.assertEqual(sanitize_filename("C:\\Users\\Admin\\secret.docx"), "secret.docx")

    def test_control_characters_and_crlf_stripped(self):
        # Prevents Log Injection (CWE-117) and HTTP header splitting
        dirty = "report\r\nInjected-Header: evil\x00\t.pdf"
        clean = sanitize_filename(dirty)
        self.assertNotIn("\r", clean)
        self.assertNotIn("\n", clean)
        self.assertNotIn("\x00", clean)
        self.assertNotIn("\t", clean)
        self.assertEqual(clean, "reportInjected-Header: evil.pdf")

    def test_empty_or_invalid_names_fallback_to_default(self):
        self.assertEqual(sanitize_filename(""), "upload.bin")
        self.assertEqual(sanitize_filename("   "), "upload.bin")
        self.assertEqual(sanitize_filename("."), "upload.bin")
        self.assertEqual(sanitize_filename(".."), "upload.bin")
        self.assertEqual(sanitize_filename("..."), "upload.bin")
        self.assertEqual(sanitize_filename(None), "upload.bin")  # type: ignore

    def test_extension_preserved_on_truncation(self):
        long_name = "a" * 300 + ".pdf"
        sanitized = sanitize_filename(long_name)
        self.assertLessEqual(len(sanitized), 255)
        self.assertTrue(sanitized.endswith(".pdf"))

    def test_extension_of_uses_sanitization(self):
        self.assertEqual(extension_of("../../evil.PDF"), ".pdf")
        self.assertEqual(extension_of("report\r\n.DOCX"), ".docx")


class TestServiceSecurityConfig(unittest.TestCase):
    """Verifies security-related configuration properties and defaults."""

    def test_max_upload_size_bytes_calculation(self):
        cfg = ServiceConfig(max_upload_size_mb=50)
        self.assertEqual(cfg.max_upload_size_bytes, 50 * 1024 * 1024)

    def test_env_example_contains_no_real_secrets(self):
        env_example_path = os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            ".env.example",
        )
        if os.path.isfile(env_example_path):
            with open(env_example_path, "r", encoding="utf-8") as f:
                content = f.read()
            # Ensure real API keys are never present in .env.example
            self.assertNotIn("AQ" + "." + "Ab8RN6Jsxdv", content)
            self.assertIn("your_gemini_api_key_here", content)


class DummyHandler(ExtractServiceHandler):
    """Subclass of ExtractServiceHandler allowing test harness to simulate HTTP requests."""

    def __init__(self, method: str, path: str, headers: dict, body: bytes = b""):
        self.command = method
        self.path = path
        self.headers = headers
        self.rfile = io.BytesIO(body)
        self.wfile = io.BytesIO()
        self._headers_buffer = []

    def send_response(self, code, message=None):
        self.response_code = code

    def send_header(self, keyword, value):
        self._headers_buffer.append((keyword.lower(), value))

    def end_headers(self):
        pass

    def get_header(self, name: str):
        for k, v in self._headers_buffer:
            if k == name.lower():
                return v
        return None

    def get_json_response(self):
        self.wfile.seek(0)
        content = self.wfile.read()
        return json.loads(content.decode("utf-8")) if content else {}


class TestHttpSecurityHeadersAndCors(unittest.TestCase):
    """Verifies HTTP security headers and CORS preflight handling."""

    def test_security_headers_in_get_root(self):
        handler = DummyHandler("GET", "/", {})
        handler.do_GET()
        self.assertEqual(handler.response_code, 200)
        self.assertEqual(handler.get_header("x-content-type-options"), "nosniff")
        self.assertEqual(handler.get_header("x-frame-options"), "SAMEORIGIN")
        self.assertEqual(handler.get_header("referrer-policy"), "no-referrer")
        self.assertEqual(handler.get_header("access-control-allow-origin"), "*")

    def test_health_does_not_leak_pid(self):
        handler = DummyHandler("GET", "/health", {})
        handler.do_GET()
        self.assertEqual(handler.response_code, 200)
        data = handler.get_json_response()
        self.assertNotIn("pid", data)
        self.assertIn("status", data)
        self.assertIn("maxUploadSizeMb", data)

    def test_options_cors_preflight(self):
        handler = DummyHandler("OPTIONS", "/extract", {"Origin": "http://localhost:3000"})
        handler.do_OPTIONS()
        self.assertEqual(handler.response_code, 204)
        self.assertEqual(handler.get_header("access-control-allow-methods"), "GET, POST, OPTIONS, HEAD")
        self.assertIn("Content-Type", handler.get_header("access-control-allow-headers"))
        self.assertEqual(handler.get_header("x-content-type-options"), "nosniff")


class TestHttpPayloadLimits(unittest.TestCase):
    """Verifies Denial of Service protection against oversized or malformed payloads."""

    def test_missing_content_length_rejected(self):
        handler = DummyHandler("POST", "/extract", {})
        handler.do_POST()
        self.assertEqual(handler.response_code, 411)
        resp = handler.get_json_response()
        self.assertIn("Length Required", resp.get("error", ""))

    def test_invalid_content_length_rejected(self):
        handler = DummyHandler("POST", "/extract", {"content-length": "not-a-number"})
        handler.do_POST()
        self.assertEqual(handler.response_code, 400)
        resp = handler.get_json_response()
        self.assertIn("Invalid Content-Length", resp.get("error", ""))

    def test_negative_content_length_rejected(self):
        handler = DummyHandler("POST", "/extract", {"content-length": "-50"})
        handler.do_POST()
        self.assertEqual(handler.response_code, 400)

    def test_empty_body_rejected(self):
        handler = DummyHandler("POST", "/extract", {"content-length": "0"}, b"")
        handler.do_POST()
        self.assertEqual(handler.response_code, 400)
        resp = handler.get_json_response()
        self.assertIn("empty body", resp.get("error", ""))

    def test_oversized_payload_rejected_with_413(self):
        oversized = 101 * 1024 * 1024  # 101 MB exceeds 100 MB default
        handler = DummyHandler("POST", "/extract", {"content-length": str(oversized)}, b"")
        handler.do_POST()
        self.assertEqual(handler.response_code, 413)
        resp = handler.get_json_response()
        self.assertIn("Payload Too Large", resp.get("error", ""))

    def test_incomplete_body_rejected(self):
        handler = DummyHandler("POST", "/extract", {"content-length": "100"}, b"short-body")
        handler.do_POST()
        self.assertEqual(handler.response_code, 400)
        resp = handler.get_json_response()
        self.assertIn("Incomplete body", resp.get("error", ""))


class TestDevAssetsSecurity(unittest.TestCase):
    """Verifies static asset traversal defenses in dev_assets.py."""

    def test_null_bytes_rejected(self):
        self.assertIsNone(get_dev_asset("/test/test\x00.html"))
        self.assertIsNone(get_dev_asset("/test/\x00"))

    def test_path_traversal_rejected(self):
        self.assertIsNone(get_dev_asset("/test/../../../../etc/passwd"))
        self.assertIsNone(get_dev_asset("/test/..%2f..%2fmain.py"))


class TestVisionAdapterSsrfProtection(unittest.TestCase):
    """Verifies that VisionGeminiAdapter blocks SSRF and non-HTTP URL schemes."""

    def test_file_scheme_rejected(self):
        adapter = VisionGeminiAdapter()
        with patch("src.infrastructure.converters.vision_gemini_adapter.config") as mock_cfg:
            mock_cfg.vision_base_url = "file:///etc/passwd"
            mock_cfg.vision_model = "test-model"
            with self.assertRaises(ValueError) as ctx:
                adapter._call_openai_compatible_api("dummy_b64")
            self.assertIn("Invalid VISION_BASE_URL scheme", str(ctx.exception))

    def test_ftp_scheme_rejected(self):
        adapter = VisionGeminiAdapter()
        with patch("src.infrastructure.converters.vision_gemini_adapter.config") as mock_cfg:
            mock_cfg.vision_base_url = "ftp://internal-service.local/data"
            mock_cfg.vision_model = "test-model"
            with self.assertRaises(ValueError) as ctx:
                adapter._call_openai_compatible_api("dummy_b64")
            self.assertIn("Invalid VISION_BASE_URL scheme", str(ctx.exception))


class TestConversionServiceSecurity(unittest.TestCase):
    """Verifies security controls on local disk conversions."""

    def test_convert_file_on_disk_rejects_null_bytes(self):
        with self.assertRaises(ValueError) as ctx:
            conversion_service.convert_file_on_disk("test\x00.pdf")
        self.assertIn("Invalid file path", str(ctx.exception))

    def test_convert_file_on_disk_rejects_oversized_file(self):
        with patch("os.path.isfile", return_value=True), \
             patch("os.path.realpath", return_value="/tmp/huge.pdf"), \
             patch("os.path.getsize", return_value=200 * 1024 * 1024):
            with self.assertRaises(ValueError) as ctx:
                conversion_service.convert_file_on_disk("/tmp/huge.pdf")
            self.assertIn("exceeds maximum allowed limit", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
