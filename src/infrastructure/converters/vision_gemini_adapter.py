"""Vision LLM adapter using Gemini (or OpenAI-compatible) API to rescue degraded or OCR-failed documents."""
import base64
import concurrent.futures
import json
import os
import random
import re
import socket
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

from src.domain.model import ConversionResult
from src.domain.ports import VisionRescuePort
from src.domain.rules import IMAGE_EXTENSIONS, extension_of
from src.infrastructure.config import config
from src.infrastructure.logging.stream_logger import logger

try:
    import pypdfium2  # type: ignore
    _HAVE_PDFIUM = True
except Exception:  # pragma: no cover
    pypdfium2 = None
    _HAVE_PDFIUM = False

VISION_PROMPT = """You are an expert document OCR engine. Transcribe this document page into clean, standard GitHub-Flavored Markdown.
- Accurately preserve all text, numbers, dates, formulas, and accents (including French accents and Vietnamese tonal diacritics: ă, â, đ, ê, ô, ơ, ư).
- Reconstruct tables using standard markdown pipe syntax (| Header | Header |). Preserve row and column alignments faithfully.
- Reconstruct section headings with appropriate levels (#, ##, ###) reflecting the visual hierarchy and natural reading order.
- Do not omit content, do not summarize, and do not hallucinate missing information.
- Output ONLY the transcribed markdown text without any introductory text, concluding text, or enclosing ```markdown code fences."""


class VisionGeminiAdapter(VisionRescuePort):
    """Rescues documents by rendering pages to images and processing with a Vision LLM."""

    def is_enabled(self) -> bool:
        """Returns True if PDF renderer and API keys are available."""
        return _HAVE_PDFIUM and config.vision_fallback_enabled and bool(config.gemini_api_key or config.vision_base_url)

    def _execute_http_request_with_retry(
        self,
        req: urllib.request.Request,
        tag: str = "vision",
        max_retries: Optional[int] = None,
        initial_delay: float = 2.0,
    ) -> dict:
        """Executes an HTTP request with exponential backoff and jitter on transient errors."""
        retries = max_retries if max_retries is not None else getattr(config, "vision_max_retries", 3)
        last_exc: Optional[Exception] = None

        for attempt in range(1, retries + 1):
            try:
                with urllib.request.urlopen(req, timeout=config.vision_timeout_sec) as resp:
                    return json.loads(resp.read().decode("utf-8"))
            except urllib.error.HTTPError as exc:
                last_exc = exc
                # 429 (Rate Limit), 500/502/503/504 (Server Overloaded / Gateway errors)
                if exc.code in (429, 500, 502, 503, 504) and attempt < retries:
                    delay = initial_delay * (2 ** (attempt - 1)) + random.uniform(0.1, 0.9)
                    retry_after = exc.headers.get("Retry-After") if exc.headers else None
                    if retry_after:
                        try:
                            delay = max(delay, float(retry_after) + 0.5)
                        except ValueError:
                            pass
                    else:
                        try:
                            err_body = exc.read().decode("utf-8", errors="replace")
                            err_json = json.loads(err_body)
                            for d in err_json.get("error", {}).get("details", []):
                                if "retryDelay" in d:
                                    delay = max(delay, float(d["retryDelay"].rstrip("s")) + 0.5)
                        except Exception:
                            pass
                    logger.warning(
                        "[%s] API call failed with HTTP %d (%s). Retrying (%d/%d) in %.1fs...",
                        tag, exc.code, exc.reason, attempt, retries, delay,
                    )
                    time.sleep(delay)
                else:
                    raise
            except (urllib.error.URLError, TimeoutError, socket.timeout, ConnectionResetError) as exc:
                last_exc = exc
                if attempt < retries:
                    jitter = random.uniform(0.1, 0.9)
                    delay = initial_delay * (2 ** (attempt - 1)) + jitter
                    logger.warning(
                        "[%s] API call network/timeout error: %s. Retrying (%d/%d) in %.1fs...",
                        tag, exc, attempt, retries, delay,
                    )
                    time.sleep(delay)
                else:
                    raise

        if last_exc:
            raise last_exc
        raise RuntimeError(f"[{tag}] Request failed without explicit exception.")

    def _call_gemini_api(self, image_b64: str, mime_type: str = "image/jpeg", page_num: int = 1) -> str:
        """Calls Google Gemini GenerateContent REST API with automatic retries and model fallback on transient errors."""
        models_to_try = [config.vision_model]
        for candidate in ("gemini-3.7-flash", "gemini-3.8-flash", "gemini-flash-latest"):
            if candidate not in models_to_try:
                models_to_try.append(candidate)

        payload = {
            "contents": [
                {
                    "parts": [
                        {"text": VISION_PROMPT},
                        {
                            "inline_data": {
                                "mime_type": mime_type,
                                "data": image_b64,
                            }
                        },
                    ]
                }
            ],
            "generationConfig": {
                "temperature": 0.1,
                "maxOutputTokens": 8192,
            },
        }

        data = json.dumps(payload).encode("utf-8")
        headers = {
            "Content-Type": "application/json",
            "X-goog-api-key": config.gemini_api_key,
        }

        last_error = None
        for model in models_to_try:
            url = f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
            req = urllib.request.Request(
                url,
                data=data,
                headers=headers,
                method="POST",
            )
            try:
                body = self._execute_http_request_with_retry(req, tag=f"vision-p{page_num}")
                candidates = body.get("candidates", [])
                if not candidates:
                    err_msg = body.get("error", {}).get("message", "No candidates returned from Gemini API")
                    raise ValueError(f"Gemini API error: {err_msg}")
                parts = candidates[0].get("content", {}).get("parts", [])
                text_parts = [p.get("text", "") for p in parts if not p.get("thought", False)]
                text = "".join(text_parts) if text_parts else "".join(p.get("text", "") for p in parts)
                return text.strip()
            except Exception as exc:
                last_error = exc
                logger.warning(
                    "[%s] Model '%s' failed (%s); trying fallback model if available...",
                    f"vision-p{page_num}", model, exc,
                )
                continue

        if last_error:
            raise last_error
        raise RuntimeError("No models available for Gemini Vision transcription.")

    def _call_openai_compatible_api(self, image_b64: str, mime_type: str = "image/jpeg", page_num: int = 1) -> str:
        """Calls an OpenAI-compatible Vision endpoint (e.g. OpenRouter, vLLM, Ollama) with retries."""
        base_url = config.vision_base_url.rstrip("/")
        parsed_url = urllib.parse.urlparse(base_url)
        if parsed_url.scheme not in ("http", "https"):
            raise ValueError(
                f"Invalid VISION_BASE_URL scheme: '{parsed_url.scheme}'. "
                "Only 'http://' and 'https://' URLs are permitted."
            )

        if not base_url.endswith("/v1"):
            url = f"{base_url}/v1/chat/completions"
        else:
            url = f"{base_url}/chat/completions"

        payload = {
            "model": config.vision_model,
            "messages": [
                {"role": "system", "content": VISION_PROMPT},
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Transcribe this page to Markdown."},
                        {
                            "type": "image_url",
                            "image_url": {"url": f"data:{mime_type};base64,{image_b64}"},
                        },
                    ],
                },
            ],
            "temperature": 0.1,
        }

        headers = {"Content-Type": "application/json"}
        if config.gemini_api_key:
            headers["Authorization"] = f"Bearer {config.gemini_api_key}"

        data = json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(url, data=data, headers=headers, method="POST")

        body = self._execute_http_request_with_retry(req, tag=f"vision-p{page_num}")

        return body["choices"][0]["message"]["content"].strip()

    def _transcribe_image(self, image_b64: str, mime_type: str = "image/jpeg", page_num: int = 1) -> str:
        """Routes the transcription call to the appropriate API provider."""
        if config.vision_base_url:
            raw_output = self._call_openai_compatible_api(image_b64, mime_type, page_num=page_num)
        else:
            raw_output = self._call_gemini_api(image_b64, mime_type, page_num=page_num)

        # Clean markdown code block fences if present
        clean = raw_output.strip()
        if clean.startswith("```markdown"):
            clean = clean[len("```markdown"):].strip()
        elif clean.startswith("```"):
            clean = clean[3:].strip()
        if clean.endswith("```"):
            clean = clean[:-3].strip()

        return clean

    def _render_and_transcribe_page(self, page_tuple: Tuple[int, Any]) -> Tuple[int, str]:
        """Renders one PDF page using pypdfium2 and sends it to the Vision LLM."""
        page_num, page = page_tuple
        scale = config.vision_dpi / 72.0
        bitmap = page.render(scale=scale)
        pil_image = bitmap.to_pil()
        buffer = io.BytesIO()
        pil_image.save(buffer, format="JPEG", quality=85)
        b64_str = base64.b64encode(buffer.getvalue()).decode("utf-8")
        t0 = time.monotonic()
        page_md = self._transcribe_image(b64_str, "image/jpeg", page_num=page_num + 1)
        elapsed = round((time.monotonic() - t0) * 1000)
        logger.info("[vision-rescue] Page %d transcribed in %d ms (%d chars)", page_num + 1, elapsed, len(page_md))
        return page_num, page_md

    def rescue(
        self,
        file_path: str,
        filename: str = "",
        embed_images: bool = True,
    ) -> ConversionResult:
        """Rescues a document using Vision LLM page-by-page."""
        tag = filename or os.path.basename(file_path)
        ext = extension_of(tag)
        logger.info("[%s] Initiating Vision LLM rescue (%s)...", tag, config.vision_model)
        t_start = time.monotonic()

        if ext in IMAGE_EXTENSIONS:
            with open(file_path, "rb") as f:
                img_bytes = f.read()

            mime_type = "image/jpeg"
            if ext == ".png":
                mime_type = "image/png"
            elif ext == ".webp":
                mime_type = "image/webp"
            elif ext == ".bmp":
                mime_type = "image/bmp"
            elif ext in (".tiff", ".tif"):
                mime_type = "image/tiff"

            b64_str = base64.b64encode(img_bytes).decode("utf-8")
            page_md = self._transcribe_image(b64_str, mime_type, page_num=1)
            total_duration = round((time.monotonic() - t_start) * 1000)

            clean_text = re.sub(r"[#*`|_\-\+]", " ", page_md)
            clean_text = re.sub(r"\s+", " ", clean_text).strip()
            engine_name = f"vision:{config.vision_model}"

            logger.info(
                "[%s] Vision rescue complete: 1 image in %d ms (%d markdown chars).",
                tag, total_duration, len(page_md),
            )

            return ConversionResult(
                checksum="",
                markdown=page_md,
                text=clean_text,
                raw_text=clean_text,
                numpages=1,
                engine=engine_name,
                duration_ms=total_duration,
                info={
                    "title": tag,
                    "rescued_by_vision": True,
                    "model": config.vision_model,
                    "pages_processed": 1,
                    "failed_pages": [],
                    "partial_rescue": False,
                },
            )

        if not _HAVE_PDFIUM or pypdfium2 is None:
            raise RuntimeError("pypdfium2 is required for Vision rescue of PDF documents")

        doc = pypdfium2.PdfDocument(file_path)
        try:
            total_pages = len(doc)
            if total_pages == 0:
                raise ValueError(f"Empty PDF document: {file_path}")

            pages_to_process = min(total_pages, config.vision_max_pages)
            if total_pages > config.vision_max_pages:
                logger.warning(
                    "[%s] Document has %d pages, exceeding VISION_MAX_PAGES (%d); processing first %d pages.",
                    tag, total_pages, config.vision_max_pages, pages_to_process,
                )

            # Load page objects
            page_items = [(i, doc[i]) for i in range(pages_to_process)]

            # Concurrently process pages with controlled concurrency to prevent 503/429 overload
            max_workers = min(config.vision_concurrency, max(1, pages_to_process))
            page_results = ["" for _ in range(pages_to_process)]
            failed_pages = []

            with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as executor:
                future_to_item = {
                    executor.submit(self._render_and_transcribe_page, item): item
                    for item in page_items
                }
                for future in concurrent.futures.as_completed(future_to_item):
                    p_num, p_obj = future_to_item[future]
                    try:
                        _, p_md = future.result()
                        page_results[p_num] = p_md
                    except Exception as exc:
                        failed_pages.append(p_num + 1)
                        logger.warning(
                            "[%s] Vision transcription failed for page %d after retries: %s. Using local text fallback for this page.",
                            tag, p_num + 1, exc,
                        )
                        # Extract local text if available as graceful per-page fallback
                        try:
                            fallback_text = p_obj.get_textpage().get_text_range().strip()
                        except Exception:
                            fallback_text = ""
                        if fallback_text:
                            page_results[p_num] = (
                                f"<!-- [vision-rescue: Page {p_num + 1} fallback to extracted text] -->\n\n{fallback_text}"
                            )
                        else:
                            page_results[p_num] = f"*[Page {p_num + 1} content could not be transcribed]*"

            if len(failed_pages) == pages_to_process:
                raise RuntimeError(f"All {pages_to_process} pages failed Vision LLM rescue.")

        finally:
            doc.close()

        # Assemble full document markdown
        assembled_md = "\n\n".join(page_results).strip()
        total_duration = round((time.monotonic() - t_start) * 1000)

        # Generate plain text projection
        clean_text = re.sub(r"[#*`|_\-\+]", " ", assembled_md)
        clean_text = re.sub(r"\s+", " ", clean_text).strip()

        engine_name = f"vision:{config.vision_model}"
        fallback_note = f" (pages {sorted(failed_pages)} used local fallback)" if failed_pages else ""
        logger.info(
            "[%s] Vision rescue complete: %d pages in %d ms (%d markdown chars)%s.",
            tag, pages_to_process, total_duration, len(assembled_md), fallback_note,
        )

        return ConversionResult(
            checksum="",
            markdown=assembled_md,
            text=clean_text,
            raw_text=clean_text,
            numpages=total_pages,
            engine=engine_name,
            duration_ms=total_duration,
            info={
                "title": tag,
                "rescued_by_vision": True,
                "model": config.vision_model,
                "pages_processed": pages_to_process,
                "failed_pages": sorted(failed_pages),
                "partial_rescue": bool(failed_pages),
            },
        )
