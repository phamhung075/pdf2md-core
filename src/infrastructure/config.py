"""Service configuration and environment variable parsing."""
import os
import time
from dataclasses import dataclass


@dataclass(frozen=True)
class ServiceConfig:
    host: str = os.environ.get("DOCLING_SERVICE_HOST", "127.0.0.1")
    port: int = int(os.environ.get("DOCLING_SERVICE_PORT", "3984"))
    service_name: str = "markdown-extract"
    dev_ui: bool = os.environ.get("DOCLING_DEV_UI", "").strip().lower() in ("1", "true", "yes", "on")
    dev_ui_dir: str = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "dev_ui")
    log_level: str = os.environ.get("LOG_LEVEL", "INFO").upper()
    embed_images: bool = os.environ.get("DOCLING_EMBED_IMAGES", "1").strip().lower() in ("1", "true", "yes", "on")
    pdf_fast_path: bool = os.environ.get("DOCLING_PDF_FAST_PATH", "").strip().lower() in ("1", "true", "yes", "on")
    gemini_api_key: str = (os.environ.get("GEMINI_API_KEY", "") or os.environ.get("VISION_API_KEY", "")).strip()
    vision_fallback_enabled: bool = (
        os.environ.get("VISION_FALLBACK_ENABLED", "1").strip().lower() in ("1", "true", "yes", "on")
        and bool((os.environ.get("GEMINI_API_KEY", "") or os.environ.get("VISION_API_KEY", "")).strip())
    )
    image_auto_vision: bool = os.environ.get("DOCLING_IMAGE_AUTO_VISION", "1").strip().lower() in ("1", "true", "yes", "on")
    vision_model: str = os.environ.get("VISION_MODEL", "gemini-flash-latest").strip()
    vision_max_pages: int = int(os.environ.get("VISION_MAX_PAGES", "30"))
    vision_dpi: int = int(os.environ.get("VISION_DPI", "150"))
    vision_base_url: str = os.environ.get("VISION_BASE_URL", "").strip()
    vision_timeout_sec: int = int(os.environ.get("VISION_TIMEOUT_SEC", "120"))
    vision_concurrency: int = int(os.environ.get("VISION_CONCURRENCY", "2"))
    vision_max_retries: int = int(os.environ.get("VISION_MAX_RETRIES", "3"))
    max_upload_size_mb: int = int(os.environ.get("MAX_UPLOAD_SIZE_MB", "100"))
    allowed_origins: str = os.environ.get("ALLOWED_ORIGINS", "*").strip()

    @property
    def max_upload_size_bytes(self) -> int:
        return max(1, self.max_upload_size_mb) * 1024 * 1024


config = ServiceConfig()
_started_at = time.time()


def uptime_sec() -> int:
    return round(time.time() - _started_at)
