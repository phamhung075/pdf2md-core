"""Unit tests for Celery fleet multi-queue priority routing and dead-letter isolation."""
import unittest
from unittest.mock import MagicMock, patch
from starlette.testclient import TestClient

from src.infrastructure.queue.celery_app import celery_app
from src.infrastructure.queue.tasks import (
    convert_document_batch,
    convert_document_interactive,
    dead_letter_task,
)
from src.interfaces.http.gateway import app


class TestCeleryFleet(unittest.TestCase):
    def test_celery_multi_queue_topology(self):
        """Celery application must declare interactive, batch, and dead_letter queues."""
        queue_names = [q.name for q in celery_app.conf.task_queues]
        self.assertIn("queue:interactive", queue_names)
        self.assertIn("queue:batch", queue_names)
        self.assertIn("queue:dead_letter", queue_names)
        self.assertEqual(celery_app.conf.task_default_queue, "queue:batch")

    def test_task_queue_routing_bindings(self):
        """Tasks must bind to their designated priority queues."""
        self.assertEqual(convert_document_interactive.queue, "queue:interactive")
        self.assertEqual(convert_document_batch.queue, "queue:batch")
        self.assertEqual(dead_letter_task.queue, "queue:dead_letter")

    def test_dead_letter_task_execution(self):
        """Dead letter task produces structured isolation record."""
        record = dead_letter_task.run(
            failed_job_id="job-999",
            filename="corrupt_document.pdf",
            error_reason="Unexpected EOF in xref table",
            payload_preview="%PDF-1.4 [corrupted]",
        )
        self.assertEqual(record["status"], "dead_lettered")
        self.assertEqual(record["job_id"], "job-999")
        self.assertEqual(record["filename"], "corrupt_document.pdf")
        self.assertIn("EOF", record["error"])

    @patch("src.infrastructure.queue.tasks.convert_document_interactive.delay")
    def test_gateway_routes_high_priority_to_interactive_queue(self, mock_delay):
        """Requests with X-Priority: high must be routed to queue:interactive."""
        mock_task = MagicMock()
        mock_task.id = "job-interactive-001"
        mock_delay.return_value = mock_task

        client = TestClient(app)
        dummy_pdf = b"%PDF-1.4 fake bytes for queue routing"
        resp = client.post(
            "/v1/jobs",
            content=dummy_pdf,
            headers={"X-File-Name": "live_note.pdf", "X-Priority": "high", "X-Bypass-RateLimit": "1"},
        )
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertEqual(data["queue"], "queue:interactive")
        self.assertEqual(data["job_id"], "job-interactive-001")
        mock_delay.assert_called_once()

    @patch("src.infrastructure.queue.tasks.convert_document_batch.delay")
    def test_gateway_routes_default_to_batch_queue(self, mock_delay):
        """Standard requests without priority headers must be routed to queue:batch."""
        mock_task = MagicMock()
        mock_task.id = "job-batch-002"
        mock_delay.return_value = mock_task

        client = TestClient(app)
        dummy_pdf = b"%PDF-1.4 fake bytes for queue routing"
        resp = client.post(
            "/v1/jobs",
            content=dummy_pdf,
            headers={"X-File-Name": "bulk_scan.pdf", "X-Bypass-RateLimit": "1"},
        )
        self.assertEqual(resp.status_code, 200)
        data = resp.json()
        self.assertEqual(data["queue"], "queue:batch")
        self.assertEqual(data["job_id"], "job-batch-002")
        mock_delay.assert_called_once()


if __name__ == "__main__":
    unittest.main()
