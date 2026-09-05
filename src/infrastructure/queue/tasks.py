"""Celery tasks for asynchronous background document conversions with multi-queue priority routing."""
import base64
from typing import Any, Dict, Optional

from src.application.conversion_service import ConversionService
from src.domain.model import ExtractionRequest
from src.infrastructure.logging.stream_logger import logger
from src.infrastructure.queue.celery_app import celery_app


def _execute_conversion_pipeline(task_id: str, file_b64: str, filename: str, options: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
    """Core execution pipeline shared across interactive and batch tasks."""
    options = options or {}
    logger.info("[Celery Worker] Starting task %s for file: %s", task_id, filename)

    try:
        raw_bytes = base64.b64decode(file_b64)
        request = ExtractionRequest(
            filename=filename,
            content=raw_bytes,
            embed_images=options.get("embed_images"),
            allow_vision_fallback=options.get("allow_vision_fallback", True),
            force_vision=options.get("force_vision", False),
            allow_fast_path=options.get("allow_fast_path", True),
        )

        service = ConversionService()
        result = service.convert_request(request)

        logger.info(
            "[Celery Worker] Successfully processed %s (engine: %s, duration: %d ms)",
            filename,
            result.engine,
            result.duration_ms,
        )

        return {
            "status": "completed",
            "job_id": task_id,
            "filename": filename,
            "engine": result.engine,
            "duration_ms": result.duration_ms,
            "markdown": result.markdown,
            "info": result.info,
            "checksum": result.checksum,
        }
    except Exception as exc:
        logger.error("[Celery Worker] Task %s failed for %s: %s", task_id, filename, exc, exc_info=True)
        # Check if error indicates a poisoned or corrupted document
        is_corrupted = any(kw in str(exc).lower() for kw in ["corrupt", "damaged", "poison", "eof", "syntaxerror", "invalid pdf"])
        if is_corrupted:
            try:
                dead_letter_task.apply_async(
                    args=[task_id, filename, str(exc), file_b64[:256]],
                    queue="queue:dead_letter",
                )
            except Exception as dl_err:
                logger.warning("[Celery Worker] Failed to route to dead letter queue: %s", dl_err)

        return {
            "status": "failed",
            "job_id": task_id,
            "filename": filename,
            "error": str(exc),
        }


@celery_app.task(bind=True, name="tasks.convert_document")
def convert_document_task(
    self,
    file_b64: str,
    filename: str,
    options: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    """Default conversion task."""
    return _execute_conversion_pipeline(self.request.id, file_b64, filename, options)


@celery_app.task(bind=True, name="tasks.convert_document_interactive", queue="queue:interactive")
def convert_document_interactive(
    self,
    file_b64: str,
    filename: str,
    options: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    """High-priority interactive task for paid subscribers and live PKM imports."""
    return _execute_conversion_pipeline(self.request.id, file_b64, filename, options)


@celery_app.task(bind=True, name="tasks.convert_document_batch", queue="queue:batch")
def convert_document_batch(
    self,
    file_b64: str,
    filename: str,
    options: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    """Standard-priority batch task for bulk ingestion."""
    return _execute_conversion_pipeline(self.request.id, file_b64, filename, options)


@celery_app.task(bind=True, name="tasks.dead_letter", queue="queue:dead_letter")
def dead_letter_task(
    self,
    failed_job_id: str,
    filename: str,
    error_reason: str,
    payload_preview: str,
) -> Dict[str, Any]:
    """Dead letter isolation queue for corrupted/poisoned documents."""
    logger.warning("[Dead Letter] Captured failed task %s for file '%s': %s", failed_job_id, filename, error_reason)
    return {
        "status": "dead_lettered",
        "job_id": failed_job_id,
        "filename": filename,
        "error": error_reason,
        "payload_preview": payload_preview,
    }
