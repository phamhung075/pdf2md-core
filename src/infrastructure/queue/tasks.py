"""Celery tasks for asynchronous background document conversions."""
import base64
from typing import Any, Dict, Optional

from src.application.conversion_service import ConversionService
from src.domain.model import ExtractionRequest
from src.infrastructure.logging.stream_logger import logger
from src.infrastructure.queue.celery_app import celery_app


@celery_app.task(bind=True, name="tasks.convert_document")
def convert_document_task(
    self,
    file_b64: str,
    filename: str,
    options: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    """Asynchronous conversion task executing the tri-tier document pipeline."""
    options = options or {}
    logger.info("[Celery Worker] Starting task %s for file: %s", self.request.id, filename)

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
            "job_id": self.request.id,
            "filename": filename,
            "engine": result.engine,
            "duration_ms": result.duration_ms,
            "markdown": result.markdown,
            "info": result.info,
            "checksum": result.checksum,
        }
    except Exception as exc:
        logger.error("[Celery Worker] Task %s failed for %s: %s", self.request.id, filename, exc, exc_info=True)
        return {
            "status": "failed",
            "job_id": self.request.id,
            "filename": filename,
            "error": str(exc),
        }
