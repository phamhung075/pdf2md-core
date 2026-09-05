"""Unit tests for Telegram & WhatsApp webhook interfaces via FastAPI gateway TestClient."""
import base64
import os
import unittest
from unittest.mock import patch
from starlette.testclient import TestClient

from src.domain.model import ConversionResult
from src.interfaces.http.gateway import app


class TestWebhooks(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(app)

    def test_telegram_webhook_start_command(self):
        payload = {
            "update_id": 10001,
            "message": {
                "message_id": 1,
                "chat": {"id": 9999},
                "text": "/start",
            },
        }
        resp = self.client.post("/v1/webhooks/telegram", json=payload)
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertEqual(data["status"], "command_handled")
        self.assertIn("Welcome to pdf2md Bot", data["response"])

    @patch("src.interfaces.http.gateway._conversion_service.convert_request")
    def test_telegram_webhook_document_conversion(self, mock_convert):
        mock_convert.return_value = ConversionResult(
            checksum="mock123hash",
            markdown="# Invoice for Services\n\n- Service A: $100\n- Service B: $400",
            text="Invoice for Services Service A: $100 Service B: $400",
            raw_text="Invoice for Services Service A: $100 Service B: $400",
            numpages=1,
            engine="pdf-oxide-fast-path",
            duration_ms=12,
            info={"title": "invoice_sample.pdf"},
        )
        pdf_content = b"%PDF-1.4 mock content"
        payload = {
            "update_id": 10002,
            "message": {
                "message_id": 2,
                "chat": {"id": 8888},
                "document": {
                    "file_id": "mock_file_id",
                    "file_name": "invoice_sample.pdf",
                    "mime_type": "application/pdf",
                },
            },
            "_test_bytes_b64": base64.b64encode(pdf_content).decode("ascii"),
        }
        resp = self.client.post("/v1/webhooks/telegram", json=payload)
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertEqual(data["status"], "success")
        self.assertIn("markdown", data)
        self.assertIn("---", data["markdown"])  # YAML frontmatter present
        self.assertIn("channel: telegram", data["markdown"])

    def test_whatsapp_webhook_verification_success(self):
        os.environ["WHATSAPP_VERIFY_TOKEN"] = "my_custom_token"
        resp = self.client.get(
            "/v1/webhooks/whatsapp",
            params={
                "hub.mode": "subscribe",
                "hub.verify_token": "my_custom_token",
                "hub.challenge": "115599",
            },
        )
        self.assertEqual(resp.status_code, 200)
        self.assertEqual(resp.text, "115599")

    def test_whatsapp_webhook_verification_failure(self):
        os.environ["WHATSAPP_VERIFY_TOKEN"] = "my_custom_token"
        resp = self.client.get(
            "/v1/webhooks/whatsapp",
            params={
                "hub.mode": "subscribe",
                "hub.verify_token": "wrong_token",
                "hub.challenge": "115599",
            },
        )
        self.assertEqual(resp.status_code, 403)

    @patch("src.interfaces.http.gateway._conversion_service.convert_request")
    def test_whatsapp_webhook_document_notification(self, mock_convert):
        mock_convert.return_value = ConversionResult(
            checksum="mock456hash",
            markdown="# Field Operations Report\n\nAll tasks completed.",
            text="Field Operations Report All tasks completed.",
            raw_text="Field Operations Report All tasks completed.",
            numpages=1,
            engine="pdf-oxide-fast-path",
            duration_ms=15,
            info={"title": "operations.pdf"},
        )
        pdf_content = b"%PDF-1.4 mock operations"
        payload = {
            "object": "whatsapp_business_account",
            "entry": [
                {
                    "id": "123456",
                    "changes": [
                        {
                            "value": {
                                "messaging_product": "whatsapp",
                                "metadata": {"display_phone_number": "123456789", "phone_number_id": "987654321"},
                                "messages": [
                                    {
                                        "from": "+33612345678",
                                        "id": "wamid.001",
                                        "timestamp": "1725578400",
                                        "type": "document",
                                        "document": {
                                            "filename": "operations.pdf",
                                            "mime_type": "application/pdf",
                                            "id": "doc_id_999",
                                        },
                                    }
                                ],
                            },
                            "field": "messages",
                        }
                    ],
                }
            ],
            "_test_bytes_b64": base64.b64encode(pdf_content).decode("ascii"),
        }
        resp = self.client.post("/v1/webhooks/whatsapp", json=payload)
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertEqual(data["status"], "success")
        self.assertEqual(data["sender"], "+33612345678")
        self.assertIn("markdown", data)
        self.assertIn("channel: whatsapp", data["markdown"])


if __name__ == "__main__":
    unittest.main()
