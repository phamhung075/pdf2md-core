"""HTTP Request Handler for markdown-extract-service."""
import json
import os
import time
from http.server import BaseHTTPRequestHandler
from urllib.parse import parse_qs, unquote, urlparse

from src.application.conversion_service import (
    UnsupportedExtensionError,
    conversion_service,
)
from src.domain.model import ExtractionRequest
from src.domain.rules import SUPPORTED_EXTENSIONS, extension_of, sanitize_filename
from src.infrastructure.config import config, uptime_sec
from src.infrastructure.converters.fast_path_adapter import FastPathConverterAdapter
from src.infrastructure.dev_assets import get_dev_asset
from src.infrastructure.logging.stream_logger import (
    reset_log_callback,
    set_log_callback,
)

_fast_path_adapter = FastPathConverterAdapter()


class ExtractServiceHandler(BaseHTTPRequestHandler):
    """Handles HTTP requests: /health, /test, and file extraction with optional SSE streaming."""

    protocol_version = "HTTP/1.1"

    def log_message(self, *args) -> None:
        """Silences standard BaseHTTPRequestHandler stderr log noise."""
        pass

    def _send_security_headers(self) -> None:
        """Injects defense-in-depth HTTP security headers."""
        self.send_header("x-content-type-options", "nosniff")
        self.send_header("x-frame-options", "SAMEORIGIN")
        self.send_header("referrer-policy", "no-referrer")

    def _send_cors_headers(self) -> None:
        """Injects CORS headers based on service configuration."""
        allowed = config.allowed_origins
        if allowed == "*":
            self.send_header("access-control-allow-origin", "*")
        elif allowed:
            req_origin = self.headers.get("origin", "")
            origins = [o.strip() for o in allowed.split(",") if o.strip()]
            if req_origin in origins:
                self.send_header("access-control-allow-origin", req_origin)
                self.send_header("vary", "Origin")

    def _send_json(self, code: int, payload: dict, send_body: bool = True) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self._send_security_headers()
        self._send_cors_headers()
        self.send_header("content-type", "application/json; charset=utf-8")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        if send_body:
            self.wfile.write(body)

    def _send_sse_event(self, event: str, data: dict) -> bool:
        payload = f"event: {event}\ndata: {json.dumps(data, ensure_ascii=False)}\n\n".encode("utf-8")
        try:
            self.wfile.write(payload)
            self.wfile.flush()
            return True
        except (BrokenPipeError, ConnectionResetError, OSError):
            return False

    def do_OPTIONS(self) -> None:
        """Handles CORS preflight requests."""
        self.send_response(204)
        self._send_security_headers()
        self._send_cors_headers()
        self.send_header("access-control-allow-methods", "GET, POST, OPTIONS, HEAD")
        self.send_header(
            "access-control-allow-headers",
            "Content-Type, Content-Length, X-File-Name, X-Vision-Fallback, X-Force-Vision, Accept, Authorization",
        )
        self.send_header("access-control-max-age", "86400")
        self.end_headers()

    def do_HEAD(self) -> None:
        """Standard HTTP HEAD method handling (same headers as GET without body)."""
        self._handle_get(send_body=False)

    def do_GET(self) -> None:
        """HTTP GET routing for service root, health checks, and dev UI assets."""
        self._handle_get(send_body=True)

    def _handle_get(self, send_body: bool = True) -> None:
        parsed = urlparse(self.path)
        req_path = parsed.path

        if req_path in ("/", ""):
            self._send_json(
                200,
                {
                    "service": config.service_name,
                    "status": "ok",
                    "endpoints": {
                        "health": "/health (GET)",
                        "extract": "/extract (POST raw bytes + X-File-Name, ?stream=1 for SSE)",
                        "to_markdown": "/to-markdown (POST alias)",
                        "dev_test_ui": "/test (GET; requires DOCLING_DEV_UI=1)"
                        if config.dev_ui
                        else "disabled (set DOCLING_DEV_UI=1 to enable /test page)",
                    },
                    "pdfFastPath": _fast_path_adapter.is_enabled(),
                    "embedImages": config.embed_images,
                    "devUi": config.dev_ui,
                    "maxUploadSizeMb": config.max_upload_size_mb,
                },
                send_body=send_body,
            )
            return

        if req_path == "/health":
            self._send_json(
                200,
                {
                    "status": "ok",
                    "service": config.service_name,
                    "engine": "docling",
                    "pdfFastPath": _fast_path_adapter.is_enabled(),
                    "visionFallback": {
                        "enabled": config.vision_fallback_enabled,
                        "model": config.vision_model,
                        "hasApiKey": bool(config.gemini_api_key),
                        "hasBaseUrl": bool(config.vision_base_url),
                        "concurrency": config.vision_concurrency,
                        "maxRetries": config.vision_max_retries,
                    },
                    "embedImages": config.embed_images,
                    "devUi": config.dev_ui,
                    "maxUploadSizeMb": config.max_upload_size_mb,
                    "uptimeSec": uptime_sec(),
                },
                send_body=send_body,
            )
            return

        # Dev-only test page + assets — every /test path 404s unless DOCLING_DEV_UI=1.
        asset = get_dev_asset(req_path)
        if asset is None:
            self._send_json(404, {"error": "Not found"}, send_body=send_body)
            return

        body, content_type = asset
        self.send_response(200)
        self._send_security_headers()
        self._send_cors_headers()
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        if send_body:
            self.wfile.write(body)

    def do_POST(self) -> None:
        """HTTP POST routing for file extraction (/extract and /to-markdown)."""
        parsed = urlparse(self.path)
        req_path = parsed.path
        if req_path not in ("/extract", "/to-markdown"):
            self._send_json(404, {"error": "Not found"})
            return

        qs = parse_qs(parsed.query)
        is_stream = "stream" in qs or self.headers.get("accept", "").startswith("text/event-stream")

        # Vision options via query params or headers
        allow_vision = True
        vf_param = qs.get("vision_fallback", [""])[0].lower() or self.headers.get("x-vision-fallback", "").lower()
        if vf_param in ("0", "false", "no", "off"):
            allow_vision = False

        force_vision = (
            qs.get("force_vision", [""])[0].lower() in ("1", "true", "yes", "on")
            or qs.get("engine", [""])[0].lower() in ("vision", "gemini")
            or self.headers.get("x-force-vision", "").lower() in ("1", "true", "yes", "on")
        )

        # Fast path option (?fast_path=0 or ?engine=docling bypasses pypdf fast-path)
        allow_fast_path = True
        fp_param = qs.get("fast_path", [""])[0].lower() or self.headers.get("x-fast-path", "").lower()
        engine_param = qs.get("engine", [""])[0].lower() or self.headers.get("x-engine", "").lower()
        if fp_param in ("0", "false", "no", "off") or engine_param in ("docling", "docling-pdf"):
            allow_fast_path = False

        cl_header = self.headers.get("content-length")
        if cl_header is None:
            self._send_json(411, {"error": "Length Required: Content-Length header is mandatory"})
            return

        try:
            length = int(cl_header.strip())
            if length < 0:
                raise ValueError("Negative Content-Length")
        except ValueError:
            self._send_json(400, {"error": "Invalid Content-Length header"})
            return

        if length == 0:
            self._send_json(400, {"error": "empty body"})
            return

        if length > config.max_upload_size_bytes:
            self._send_json(
                413,
                {
                    "error": (
                        f"Payload Too Large: {length} bytes exceeds maximum allowed upload limit "
                        f"of {config.max_upload_size_mb} MB ({config.max_upload_size_bytes} bytes)"
                    )
                },
            )
            return

        # Safely stream request payload in bounded chunks
        remaining = length
        chunks = []
        chunk_size = 64 * 1024
        while remaining > 0:
            read_size = min(remaining, chunk_size)
            chunk = self.rfile.read(read_size)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)

        data = b"".join(chunks)
        if len(data) != length:
            self._send_json(400, {"error": f"Incomplete body: expected {length} bytes, received {len(data)} bytes"})
            return

        raw_header = (
            self.headers.get("x-file-name")
            or self.headers.get("x-filename")
            or qs.get("filename", [""])[0]
            or qs.get("name", [""])[0]
            or "upload.bin"
        )
        raw_name = sanitize_filename(unquote(raw_header))
        ext = extension_of(raw_name) or (f".{qs.get('ext', [''])[0].lstrip('.')}" if qs.get("ext") else "")

        if is_stream:
            self._handle_extract_stream(
                data, raw_name, ext,
                allow_vision=allow_vision,
                force_vision=force_vision,
                allow_fast_path=allow_fast_path,
            )
        else:
            self._handle_extract_sync(
                data, raw_name, ext,
                allow_vision=allow_vision,
                force_vision=force_vision,
                allow_fast_path=allow_fast_path,
            )

    def _handle_extract_stream(
        self,
        data: bytes,
        raw_name: str,
        ext: str,
        allow_vision: bool = True,
        force_vision: bool = False,
        allow_fast_path: bool = True,
    ) -> None:
        self.close_connection = True
        self.send_response(200)
        self._send_security_headers()
        self._send_cors_headers()
        self.send_header("content-type", "text/event-stream; charset=utf-8")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.send_header("x-accel-buffering", "no")
        self.end_headers()

        if len(data) == 0:
            self._send_sse_event("error", {"error": "empty body"})
            self._send_sse_event("done", {})
            return

        if ext not in SUPPORTED_EXTENSIONS:
            self._send_sse_event(
                "error",
                {
                    "error": f"unsupported extension '{ext or '(none)'}' for {raw_name} — "
                             f"supported: {sorted(SUPPORTED_EXTENSIONS)}"
                },
            )
            self._send_sse_event("done", {})
            return

        token = set_log_callback(lambda entry: self._send_sse_event("log", entry))
        try:
            self._send_sse_event(
                "log",
                {
                    "ts": round(time.time(), 3),
                    "level": "INFO",
                    "logger": config.service_name,
                    "message": f"Stream opened for {raw_name} ({len(data)} bytes, extension: {ext})",
                },
            )
            request = ExtractionRequest(
                content=data,
                filename=raw_name,
                extension=ext,
                embed_images=config.embed_images,
                allow_vision_fallback=allow_vision,
                force_vision=force_vision,
                allow_fast_path=allow_fast_path,
            )
            result = conversion_service.convert_request(request)
            self._send_sse_event("result", result.to_dict())
        except Exception as e:  # noqa: BLE001
            self._send_sse_event("error", {"error": str(e)})
        finally:
            reset_log_callback(token)
            self._send_sse_event("done", {})

    def _handle_extract_sync(
        self,
        data: bytes,
        raw_name: str,
        ext: str,
        allow_vision: bool = True,
        force_vision: bool = False,
        allow_fast_path: bool = True,
    ) -> None:
        if len(data) == 0:
            self._send_json(400, {"error": "empty body"})
            return

        if ext not in SUPPORTED_EXTENSIONS:
            self._send_json(
                415,
                {
                    "error": f"unsupported extension '{ext or '(none)'}' for {raw_name} — "
                             f"supported: {sorted(SUPPORTED_EXTENSIONS)}"
                },
            )
            return

        try:
            request = ExtractionRequest(
                content=data,
                filename=raw_name,
                extension=ext,
                embed_images=config.embed_images,
                allow_vision_fallback=allow_vision,
                force_vision=force_vision,
                allow_fast_path=allow_fast_path,
            )
            result = conversion_service.convert_request(request)
            self._send_json(200, result.to_dict())
        except UnsupportedExtensionError as e:
            self._send_json(415, {"error": str(e)})
        except Exception as e:  # noqa: BLE001
            self._send_json(500, {"error": str(e)})
