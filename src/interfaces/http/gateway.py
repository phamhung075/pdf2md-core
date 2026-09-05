"""FastAPI Web Gateway providing modern ASGI ingress, Keycloak JWT auth, and Celery queue hooks."""
import asyncio
import base64
import json
import os
import re
from typing import Any, Dict, Optional

from fastapi import (
    Depends,
    FastAPI,
    File,
    Header,
    HTTPException,
    Query,
    Request,
    Response,
    UploadFile,
    status,
)
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse, Response as RawResponse, StreamingResponse
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

from src.application.conversion_service import (
    ConversionService,
    UnsupportedExtensionError,
)
from src.domain.model import ConversionResult, ExtractionRequest
from src.domain.rules import (
    SUPPORTED_EXTENSIONS,
    extension_of,
    is_supported_extension,
    sanitize_filename,
)
from src.infrastructure.config import config, uptime_sec
from src.infrastructure.converters.fast_path_adapter import FastPathConverterAdapter
from src.infrastructure.dev_assets import get_dev_asset
from src.infrastructure.logging.stream_logger import logger

# Initialize application
app = FastAPI(
    title="markdown-extract-service",
    description="Deterministic layout-aware Markdown extraction microservice with native Rust fast path.",
    version="1.0.0",
)

# CORS middleware
origins = [o.strip() for o in config.allowed_origins.split(",") if o.strip()]
if not origins or "*" in origins:
    origins = ["*"]

app.add_middleware(
    CORSMiddleware,
    allow_origins=origins,
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

# Optional Keycloak JWT Bearer Authentication
security_bearer = HTTPBearer(auto_error=False)
KEYCLOAK_ENABLED = os.environ.get("KEYCLOAK_ENABLED", "0").strip().lower() in ("1", "true", "yes", "on")

_fast_path_adapter = FastPathConverterAdapter()
_conversion_service = ConversionService()


# Middleware for strict security headers on all responses
@app.middleware("http")
async def add_security_headers_middleware(request: Request, call_next):
    response = await call_next(request)
    response.headers["x-content-type-options"] = "nosniff"
    response.headers["x-frame-options"] = "SAMEORIGIN"
    response.headers["referrer-policy"] = "no-referrer"
    return response


def verify_keycloak_jwt(credentials: Optional[HTTPAuthorizationCredentials] = Depends(security_bearer)) -> Optional[Dict[str, Any]]:
    """Verifies Keycloak JWT Bearer token when KEYCLOAK_ENABLED=1."""
    if not KEYCLOAK_ENABLED:
        return None  # Auth is optional in dev mode

    if not credentials or not credentials.credentials:
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail="Missing Bearer authentication token",
            headers={"WWW-Authenticate": "Bearer"},
        )

    token = credentials.credentials
    try:
        import jwt  # PyJWT is bundled in environment
        # Decode without verification if public key not set, or verify when key configured
        keycloak_key = os.environ.get("KEYCLOAK_PUBLIC_KEY", "")
        if keycloak_key:
            decoded = jwt.decode(token, keycloak_key, algorithms=["RS256"], options={"verify_aud": False})
        else:
            decoded = jwt.decode(token, options={"verify_signature": False})
        return decoded
    except Exception as e:
        logger.warning("Keycloak JWT verification failed: %s", e)
        raise HTTPException(
            status_code=status.HTTP_401_UNAUTHORIZED,
            detail=f"Invalid authentication token: {e}",
            headers={"WWW-Authenticate": "Bearer"},
        )


@app.get("/")
def get_root():
    """Service metadata and active endpoints contract."""
    return {
        "service": config.service_name,
        "status": "ok",
        "endpoints": {
            "health": "/health (GET)",
            "extract": "/extract (POST raw bytes + X-File-Name or multipart, ?stream=1 for SSE)",
            "to_markdown": "/to-markdown (POST alias)",
            "sync_v1": "/v1/convert (POST synchronous conversion)",
            "async_v1": "/v1/jobs (POST async Celery job creation, GET /v1/jobs/{id})",
            "webhook_telegram": "/v1/webhooks/telegram (POST Telegram Bot update)",
            "webhook_whatsapp": "/v1/webhooks/whatsapp (GET verification, POST WhatsApp Cloud notification)",
            "dev_test_ui": "/test (GET; requires DOCLING_DEV_UI=1)"
            if config.dev_ui
            else "disabled (set DOCLING_DEV_UI=1 to enable /test page)",
        },
        "pdfFastPath": _fast_path_adapter.is_enabled(),
        "embedImages": config.embed_images,
        "devUi": config.dev_ui,
        "maxUploadSizeMb": config.max_upload_size_mb,
    }


@app.get("/health")
def get_health():
    """Health check endpoint reflecting engine, fast-path, and vision configuration."""
    return {
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
    }


@app.get("/test")
@app.get("/test/{path:path}")
def get_test_ui(path: str = ""):
    """Dev-only interactive test page and static assets."""
    req_path = "/test" if not path else f"/test/{path}"
    asset = get_dev_asset(req_path)
    if asset is None:
        raise HTTPException(status_code=404, detail="Asset not found")
    body, content_type = asset
    return RawResponse(content=body, media_type=content_type)


async def _parse_upload_payload(
    request: Request,
    file: Optional[UploadFile] = None,
    x_file_name: Optional[str] = None,
    x_filename: Optional[str] = None,
    filename_query: Optional[str] = None,
) -> tuple[bytes, str]:
    """Extracts raw bytes and sanitized filename from either multipart or raw binary stream."""
    if file is not None:
        filename = file.filename or "upload.pdf"
        content = await file.read()
    else:
        # Raw binary upload
        content_length = request.headers.get("content-length")
        if content_length is None:
            raise HTTPException(status_code=411, detail="Length Required: Content-Length header is mandatory")
        try:
            length = int(content_length.strip())
            if length < 0:
                raise ValueError("Negative Content-Length")
        except ValueError:
            raise HTTPException(status_code=400, detail="Invalid Content-Length header")

        if length == 0:
            raise HTTPException(status_code=400, detail="empty body")
        if length > config.max_upload_size_bytes:
            raise HTTPException(
                status_code=413,
                detail=f"Payload Too Large: {length} bytes exceeds limit of {config.max_upload_size_mb} MB",
            )

        content = await request.body()
        if len(content) != length:
            raise HTTPException(
                status_code=400,
                detail=f"Incomplete body: expected {length} bytes, received {len(content)} bytes",
            )

        filename = x_file_name or x_filename or filename_query or "upload.pdf"

    if len(content) == 0:
        raise HTTPException(status_code=400, detail="empty body")

    if len(content) > config.max_upload_size_bytes:
        raise HTTPException(
            status_code=413,
            detail=f"Payload Too Large: {len(content)} bytes exceeds limit of {config.max_upload_size_mb} MB",
        )

    clean_filename = sanitize_filename(filename)
    ext = extension_of(clean_filename)
    if not is_supported_extension(ext):
        raise HTTPException(
            status_code=415,
            detail=f"Unsupported Media Type: '{ext or '(none)'}' not in {sorted(SUPPORTED_EXTENSIONS)}",
        )

    return content, clean_filename


@app.post("/extract")
@app.post("/to-markdown")
async def extract_document(
    request: Request,
    file: Optional[UploadFile] = File(None),
    x_file_name: Optional[str] = Header(None),
    x_filename: Optional[str] = Header(None),
    filename: Optional[str] = Query(None),
    stream: int = Query(0),
    force_vision: Optional[bool] = Query(None),
    fast_path: Optional[bool] = Query(None),
    engine: Optional[str] = Query(None),
    vision_fallback: Optional[bool] = Query(None),
    frontmatter: bool = Query(False, description="Prepend structured YAML frontmatter block"),
    x_force_vision: Optional[str] = Header(None),
    x_fast_path: Optional[str] = Header(None),
    x_engine: Optional[str] = Header(None),
    x_vision_fallback: Optional[str] = Header(None),
):
    """Primary document extraction endpoint supporting raw binary, multipart, and SSE streaming."""
    content, clean_filename = await _parse_upload_payload(
        request, file=file, x_file_name=x_file_name, x_filename=x_filename, filename_query=filename
    )

    # Resolve options
    is_force_vision = False
    if force_vision is not None:
        is_force_vision = force_vision
    elif engine and engine.lower() in ("vision", "gemini"):
        is_force_vision = True
    elif x_force_vision and x_force_vision.lower() in ("1", "true", "yes", "on"):
        is_force_vision = True

    is_allow_fast_path = True
    if fast_path is not None:
        is_allow_fast_path = fast_path
    elif engine and engine.lower() in ("docling", "docling-pdf"):
        is_allow_fast_path = False
    elif x_fast_path and x_fast_path.lower() in ("0", "false", "no", "off"):
        is_allow_fast_path = False
    elif x_engine and x_engine.lower() in ("docling", "docling-pdf"):
        is_allow_fast_path = False

    is_allow_vision_fallback = True
    if vision_fallback is not None:
        is_allow_vision_fallback = vision_fallback
    elif x_vision_fallback and x_vision_fallback.lower() in ("0", "false", "no", "off"):
        is_allow_vision_fallback = False

    req = ExtractionRequest(
        filename=clean_filename,
        content=content,
        embed_images=config.embed_images,
        allow_vision_fallback=is_allow_vision_fallback,
        force_vision=is_force_vision,
        allow_fast_path=is_allow_fast_path,
    )

    accept_header = request.headers.get("accept", "")
    is_sse = stream == 1 or "text/event-stream" in accept_header

    if is_sse:
        async def event_generator():
            yield f"event: progress\ndata: {json.dumps({'stage': 'ingest', 'filename': clean_filename})}\n\n"
            await asyncio.sleep(0.01)
            try:
                loop = asyncio.get_running_loop()
                result = await loop.run_in_executor(None, _conversion_service.convert_request, req)
                payload = result.to_dict()
                yield f"event: done\ndata: {json.dumps(payload, ensure_ascii=False)}\n\n"
            except Exception as e:
                yield f"event: error\ndata: {json.dumps({'error': str(e)})}\n\n"

        return StreamingResponse(event_generator(), media_type="text/event-stream")

    # Synchronous processing
    loop = asyncio.get_running_loop()
    try:
        result: ConversionResult = await loop.run_in_executor(None, _conversion_service.convert_request, req)
        payload = result.to_dict()
        if frontmatter:
            from src.domain.metadata_extractor import prepend_yaml_frontmatter
            payload["markdown"] = prepend_yaml_frontmatter(payload["markdown"], filename=clean_filename)
        return payload
    except Exception as e:
        logger.error("Conversion failed for %s: %s", clean_filename, e)
        raise HTTPException(status_code=500, detail=f"Conversion error: {e}")


# ==============================================================================
# SaaS Step 1 Endpoints: Synchronous V1 & Asynchronous Celery Queue V1
# ==============================================================================

@app.post("/v1/convert")
async def convert_v1(
    request: Request,
    file: Optional[UploadFile] = File(None),
    x_file_name: Optional[str] = Header(None),
    frontmatter: bool = Query(False, description="Prepend structured YAML frontmatter block"),
    user_token: Optional[Dict[str, Any]] = Depends(verify_keycloak_jwt),
):
    """SaaS Synchronous conversion endpoint for interactive PKM / API clients (<15s)."""
    content, clean_filename = await _parse_upload_payload(
        request, file=file, x_file_name=x_file_name
    )

    req = ExtractionRequest(
        filename=clean_filename,
        content=content,
        embed_images=config.embed_images,
        allow_vision_fallback=True,
        force_vision=False,
        allow_fast_path=True,
    )

    loop = asyncio.get_running_loop()
    try:
        result = await loop.run_in_executor(None, _conversion_service.convert_request, req)
        resp = result.to_dict()
        if frontmatter:
            from src.domain.metadata_extractor import prepend_yaml_frontmatter
            resp["markdown"] = prepend_yaml_frontmatter(resp["markdown"], filename=clean_filename)
        resp["success"] = True
        return resp
    except Exception as e:
        logger.error("V1 conversion failed: %s", e)
        raise HTTPException(status_code=500, detail=str(e))


@app.post("/v1/jobs")
async def create_conversion_job(
    request: Request,
    file: Optional[UploadFile] = File(None),
    x_file_name: Optional[str] = Header(None),
    user_token: Optional[Dict[str, Any]] = Depends(verify_keycloak_jwt),
):
    """SaaS Asynchronous conversion job creation enqueuing into Celery + Redis."""
    content, clean_filename = await _parse_upload_payload(
        request, file=file, x_file_name=x_file_name
    )

    file_b64 = base64.b64encode(content).decode("ascii")

    try:
        from src.infrastructure.queue.tasks import convert_document_task
        task = convert_document_task.delay(
            file_b64,
            clean_filename,
            {"embed_images": config.embed_images, "allow_fast_path": True},
        )
        return {
            "job_id": task.id,
            "status": "PENDING",
            "filename": clean_filename,
            "check_status_url": f"/v1/jobs/{task.id}",
        }
    except Exception as e:
        logger.warning("Celery dispatch failed (%s), falling back to synchronous execution", e)
        # Graceful fallback when Redis broker is unreachable locally
        req = ExtractionRequest(filename=clean_filename, content=content)
        result = _conversion_service.convert_request(req)
        return {
            "job_id": "sync-fallback",
            "status": "COMPLETED",
            "filename": clean_filename,
            "result": {
                "engine": result.engine,
                "markdown": result.markdown,
                "duration_ms": result.duration_ms,
            },
        }


@app.get("/v1/jobs/{job_id}")
def get_job_status(job_id: str, user_token: Optional[Dict[str, Any]] = Depends(verify_keycloak_jwt)):
    """Retrieves asynchronous Celery job status and markdown result."""
    if job_id == "sync-fallback":
        return {"job_id": job_id, "status": "COMPLETED"}

    try:
        from celery.result import AsyncResult
        from src.infrastructure.queue.celery_app import celery_app

        res = AsyncResult(job_id, app=celery_app)
        state = res.state
        if state == "SUCCESS":
            return {"job_id": job_id, "status": "COMPLETED", "result": res.result}
        elif state == "FAILURE":
            return {"job_id": job_id, "status": "FAILED", "error": str(res.result)}
        else:
            return {"job_id": job_id, "status": state}
    except Exception as e:
        return {"job_id": job_id, "status": "UNKNOWN", "error": str(e)}


# ==============================================================================
# Step 3: Mobile Bots Webhook Handlers (Telegram & WhatsApp Cloud API)
# ==============================================================================

@app.post("/v1/webhooks/telegram")
async def telegram_webhook(request: Request):
    """Stateless webhook receiver for Telegram Bot updates."""
    try:
        update = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Invalid JSON update payload")

    from src.interfaces.webhooks.telegram_bot import handle_telegram_update
    bot_token = os.environ.get("TELEGRAM_BOT_TOKEN")
    return handle_telegram_update(update, _conversion_service, bot_token=bot_token)


@app.get("/v1/webhooks/whatsapp")
def whatsapp_verify_challenge(
    hub_mode: Optional[str] = Query(None, alias="hub.mode"),
    hub_verify_token: Optional[str] = Query(None, alias="hub.verify_token"),
    hub_challenge: Optional[str] = Query(None, alias="hub.challenge"),
):
    """Meta WhatsApp Cloud API webhook verification challenge handshake."""
    from src.interfaces.webhooks.whatsapp_bot import verify_whatsapp_challenge
    expected_token = os.environ.get("WHATSAPP_VERIFY_TOKEN", "pdf2md_secret")
    challenge = verify_whatsapp_challenge(hub_mode, hub_verify_token, hub_challenge, expected_token)
    if challenge is None:
        raise HTTPException(status_code=403, detail="Verification token mismatch")
    return RawResponse(content=challenge, media_type="text/plain")


@app.post("/v1/webhooks/whatsapp")
async def whatsapp_notification(request: Request):
    """Meta WhatsApp Cloud API incoming message notification webhook."""
    try:
        payload = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Invalid JSON payload")

    from src.interfaces.webhooks.whatsapp_bot import handle_whatsapp_notification
    access_token = os.environ.get("WHATSAPP_ACCESS_TOKEN")
    phone_id = os.environ.get("WHATSAPP_PHONE_NUMBER_ID")
    return handle_whatsapp_notification(
        payload,
        _conversion_service,
        phone_number_id=phone_id,
        access_token=access_token,
    )
