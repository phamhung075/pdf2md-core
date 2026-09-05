"""Application service orchestrating document conversions."""
import hashlib
import os
import tempfile
import threading
import time
from typing import Optional

from src.domain.model import ConversionResult, ExtractionRequest
from src.domain.ports import (
    ConversionServicePort,
    DocumentConverterPort,
    VisionRescuePort,
)
from src.domain.quality_gate import evaluate_quality_gate, recover_lost_canvas_tables
from src.domain.rules import (
    IMAGE_EXTENSIONS,
    PDF_EXTENSIONS,
    SUPPORTED_EXTENSIONS,
    extension_of,
    is_supported_extension,
)
from src.infrastructure.config import config
from src.infrastructure.converters.docling_adapter import DoclingConverterAdapter
from src.infrastructure.converters.fast_path_adapter import FastPathConverterAdapter
from src.infrastructure.converters.vision_gemini_adapter import VisionGeminiAdapter
from src.infrastructure.logging.stream_logger import logger


class UnsupportedExtensionError(ValueError):
    """Raised when an unsupported file extension is requested."""
    pass


class ConversionService(ConversionServicePort):
    """Orchestrates conversion routing, concurrency locking, and execution."""

    def __init__(
        self,
        docling_converter: Optional[DocumentConverterPort] = None,
        fast_path_converter: Optional[FastPathConverterAdapter] = None,
        vision_rescue: Optional[VisionRescuePort] = None,
    ):
        self._docling = docling_converter or DoclingConverterAdapter()
        self._fast_path = fast_path_converter or FastPathConverterAdapter()
        self._vision_rescue = vision_rescue or VisionGeminiAdapter()
        self._convert_lock = threading.Lock()
        self._warned_fast_fallback = False


    def _warn_fast_fallback(self, filename: str, exc: Exception) -> None:
        """Log the first fast-path failure per process, then fall back to Docling silently."""
        if not self._warned_fast_fallback:
            self._warned_fast_fallback = True
            logger.warning(
                "[fast-path] pypdf fast-path failed for %s — falling back to Docling (%s); "
                "disable DOCLING_PDF_FAST_PATH if this repeats",
                filename,
                exc,
            )

    def convert_request(self, request: ExtractionRequest) -> ConversionResult:
        """Processes an extraction request and returns the conversion result."""
        ext = request.extension or extension_of(request.filename)
        if not is_supported_extension(ext):
            raise UnsupportedExtensionError(
                f"unsupported extension '{ext or '(none)'}' for {request.filename} — "
                f"supported: {sorted(SUPPORTED_EXTENSIONS)}"
            )

        checksum = hashlib.sha256(request.content).hexdigest()
        suffix = ext if ext in SUPPORTED_EXTENSIONS else ".bin"
        logger.info(
            "[%s] Upload payload: %d bytes, sha256=%s...%s, extension=%s",
            request.filename, len(request.content), checksum[:12], checksum[-6:], ext
        )

        with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as f:
            f.write(request.content)
            temp_path = f.name

        try:
            t0 = time.monotonic()
            result = self._convert_file(
                temp_path,
                request.filename,
                ext,
                embed_images=request.embed_images,
                allow_vision_fallback=request.allow_vision_fallback,
                force_vision=request.force_vision,
                allow_fast_path=request.allow_fast_path,
            )
            duration_ms = round((time.monotonic() - t0) * 1000)
            result.duration_ms = duration_ms
            result.checksum = checksum
            if "title" not in result.info:
                result.info["title"] = request.filename
            logger.info(
                "[%s] Finished conversion in %d ms (engine: %s).",
                request.filename, duration_ms, result.engine
            )
            return result
        finally:
            try:
                os.unlink(temp_path)
            except OSError:
                pass

    def convert_file_on_disk(
        self,
        file_path: str,
        embed_images: Optional[bool] = None,
        allow_vision_fallback: bool = True,
        force_vision: bool = False,
    ) -> ConversionResult:
        """Convenience method for MCP/CLI converting a local file already on disk."""
        if not file_path or "\x00" in file_path:
            raise ValueError("Invalid file path provided")

        real_path = os.path.realpath(file_path)
        if not os.path.isfile(real_path):
            raise FileNotFoundError(f"File not found: {file_path}")

        file_size = os.path.getsize(real_path)
        if file_size > config.max_upload_size_bytes:
            raise ValueError(
                f"File size ({file_size} bytes) exceeds maximum allowed limit "
                f"of {config.max_upload_size_mb} MB ({config.max_upload_size_bytes} bytes)"
            )

        filename = os.path.basename(real_path)
        ext = extension_of(filename)
        if not is_supported_extension(ext):
            raise UnsupportedExtensionError(
                f"unsupported extension '{ext or '(none)'}' for {filename} — "
                f"supported: {sorted(SUPPORTED_EXTENSIONS)}"
            )

        with open(real_path, "rb") as f:
            content = f.read()

        should_embed = config.embed_images if embed_images is None else embed_images
        request = ExtractionRequest(
            content=content,
            filename=filename,
            extension=ext,
            embed_images=should_embed,
            allow_vision_fallback=allow_vision_fallback,
            force_vision=force_vision,
        )
        return self.convert_request(request)

    def _convert_file(
        self,
        path: str,
        filename: str,
        ext: str,
        embed_images: bool,
        allow_vision_fallback: bool = True,
        force_vision: bool = False,
        allow_fast_path: bool = True,
    ) -> ConversionResult:
        is_pdf = ext in PDF_EXTENSIONS
        is_image = ext in IMAGE_EXTENSIONS

        pipeline_trace: list = []

        # 1. Force vision if explicitly requested
        if (is_pdf or is_image) and force_vision:
            if self._vision_rescue.is_enabled():
                logger.info("[%s] Force Vision LLM requested — running vision pipeline...", filename)
                pipeline_trace.append({
                    "stage": "routing",
                    "action": "force_vision",
                    "model": config.vision_model,
                })
                rescued = self._vision_rescue.rescue(path, filename, embed_images=embed_images)
                pipeline_trace.append({
                    "stage": "tier_c_vision_rescue",
                    "status": "success",
                    "model": config.vision_model,
                    "duration_ms": rescued.duration_ms,
                })
                rescued.pipeline_trace = pipeline_trace
                rescued.info["pipeline_trace"] = pipeline_trace
                return rescued
            logger.warning("[%s] Force Vision LLM requested but vision engine is not enabled/configured.", filename)
            pipeline_trace.append({
                "stage": "routing",
                "action": "force_vision_unavailable",
                "fallback": "standard_pipeline",
            })

        # 2. Standard conversion: Fast Path (if eligible) or Docling
        result: Optional[ConversionResult] = None
        if is_pdf:
            if allow_fast_path and self._fast_path.is_enabled():
                try:
                    logger.info("[%s] Checking digital-PDF fast path (DOCLING_PDF_FAST_PATH=1)...", filename)
                    t_probe = time.monotonic()
                    is_dig = self._fast_path.is_digital(path)
                    probe_ms = round((time.monotonic() - t_probe) * 1000)
                    pipeline_trace.append({
                        "stage": "tier_a_fast_path_probe",
                        "status": "eligible" if is_dig else "ineligible",
                        "duration_ms": probe_ms,
                    })
                    if is_dig:
                        fast_result = self._fast_path.convert(path, filename, embed_images=embed_images)
                        pipeline_trace.append({
                            "stage": "tier_a_fast_path_convert",
                            "engine": fast_result.engine,
                            "status": "success",
                            "duration_ms": fast_result.duration_ms,
                            "markdown_chars": len(fast_result.markdown),
                            "numpages": fast_result.numpages,
                        })
                        # Evaluate fast path output before accepting it
                        eval_md = fast_result.markdown
                        passed, reasons = evaluate_quality_gate(eval_md, fast_result.text, pdf_path=path)
                        pipeline_trace.append({
                            "stage": "tier_a_quality_gate",
                            "status": "passed" if passed else "failed",
                            "passed": passed,
                            "reasons": reasons,
                        })
                        if passed:
                            logger.info("[%s] Fast path converted digital PDF via %s.", filename, fast_result.engine)
                            result = fast_result
                        else:
                            logger.warning(
                                "[%s] Fast path output failed quality gate (%s); falling through to local Docling pipeline (TableFormer).",
                                filename,
                                "; ".join(reasons),
                            )
                    else:
                        logger.info("[%s] Fast path probe passed; falling through to Docling PDF pipeline.", filename)
                except Exception as e:
                    self._warn_fast_fallback(filename, e)
                    pipeline_trace.append({
                        "stage": "tier_a_fast_path_error",
                        "error": str(e),
                        "fallback": "docling",
                    })
            else:
                logger.info(
                    "[%s] Routing to Docling PDF pipeline (Heron layout + TableFormer + RapidOCR PP-OCRv6).",
                    filename,
                )
                pipeline_trace.append({
                    "stage": "routing",
                    "action": "docling_pdf",
                    "fast_path_enabled": False,
                })
        elif is_image:
            if allow_fast_path and config.image_auto_vision and self._vision_rescue.is_enabled():
                try:
                    logger.info(
                        "[%s] Auto-routing photo/image to Vision LLM (%s) for high-fidelity multimodal extraction...",
                        filename,
                        config.vision_model,
                    )
                    rescued = self._vision_rescue.rescue(path, filename, embed_images=embed_images)
                    pipeline_trace.append({
                        "stage": "tier_c_auto_vision",
                        "status": "success",
                        "model": config.vision_model,
                        "duration_ms": rescued.duration_ms,
                    })
                    rescued.pipeline_trace = pipeline_trace
                    rescued.info["pipeline_trace"] = pipeline_trace
                    return rescued
                except Exception as e:
                    logger.warning(
                        "[%s] Vision LLM for image failed (%s) — falling back to Docling image pipeline.",
                        filename,
                        e,
                    )
                    pipeline_trace.append({
                        "stage": "tier_c_auto_vision_failed",
                        "error": str(e),
                    })
            logger.info(
                "[%s] Routing to Docling Image pipeline (Heron layout + TableFormer + RapidOCR PP-OCRv6).",
                filename,
            )
            pipeline_trace.append({"stage": "routing", "action": "docling_image"})
        else:
            logger.info("[%s] Routing to Docling native reader for %s (no OCR/layout models).", filename, ext)
            pipeline_trace.append({"stage": "routing", "action": "docling_native", "ext": ext})

        if result is None:
            # Threading lock around Docling converter invocation
            tag = filename or os.path.basename(path)
            t_wait = time.monotonic()
            logger.info("[%s] Waiting for Docling converter lock...", tag)
            with self._convert_lock:
                wait_ms = round((time.monotonic() - t_wait) * 1000)
                logger.info("[%s] Acquired converter lock (waited %d ms).", tag, wait_ms)
                t_doc = time.monotonic()
                result = self._docling.convert(path, filename, embed_images=embed_images)
                doc_ms = round((time.monotonic() - t_doc) * 1000)
                pipeline_trace.append({
                    "stage": "tier_b_docling_convert",
                    "status": "success",
                    "engine": result.engine,
                    "wait_ms": wait_ms,
                    "duration_ms": doc_ms,
                    "markdown_chars": len(result.markdown),
                    "numpages": result.numpages,
                })

        # 3. Canvas Table Recovery & Quality Gate Evaluation
        if is_pdf and result and result.markdown:
            t_rec = time.monotonic()
            recovered = recover_lost_canvas_tables(result.markdown, path)
            rec_ms = round((time.monotonic() - t_rec) * 1000)
            table_recovered = (recovered != result.markdown)
            pipeline_trace.append({
                "stage": "canvas_table_recovery",
                "status": "recovered" if table_recovered else "no_tables_needed",
                "recovered": table_recovered,
                "duration_ms": rec_ms,
            })
            if table_recovered:
                logger.info(
                    "[%s] Reconstructed uncaptured canvas table(s) locally via 2D geometry.",
                    filename,
                )
                result.markdown = recovered

        if (is_pdf or is_image) and allow_vision_fallback and self._vision_rescue.is_enabled():
            gate_path = path if is_pdf else None
            t_gate = time.monotonic()
            passed, reasons = evaluate_quality_gate(result.markdown, result.text, pdf_path=gate_path)
            gate_ms = round((time.monotonic() - t_gate) * 1000)
            pipeline_trace.append({
                "stage": "final_quality_gate",
                "status": "passed" if passed else "failed",
                "passed": passed,
                "reasons": reasons,
                "duration_ms": gate_ms,
            })
            if not passed:
                logger.warning(
                    "[%s] Quality gate failed for %s (%s). Attempting Vision LLM rescue...",
                    filename,
                    result.engine,
                    "; ".join(reasons),
                )
                try:
                    t_vis = time.monotonic()
                    rescued = self._vision_rescue.rescue(path, filename, embed_images=embed_images)
                    vis_ms = round((time.monotonic() - t_vis) * 1000)
                    pipeline_trace.append({
                        "stage": "tier_c_vision_rescue",
                        "status": "success",
                        "model": config.vision_model,
                        "duration_ms": vis_ms,
                        "original_engine": result.engine,
                    })
                    rescued.info["original_engine"] = result.engine
                    rescued.info["quality_gate_reasons"] = reasons
                    rescued.pipeline_trace = pipeline_trace
                    rescued.info["pipeline_trace"] = pipeline_trace
                    return rescued
                except Exception as exc:
                    logger.error(
                        "[%s] Vision LLM rescue failed: %s — retaining original %s output",
                        filename,
                        exc,
                        result.engine,
                    )
                    pipeline_trace.append({
                        "stage": "tier_c_vision_rescue_failed",
                        "error": str(exc),
                    })

        result.pipeline_trace = pipeline_trace
        result.info["pipeline_trace"] = pipeline_trace
        return result


# Default singleton application service instance
conversion_service = ConversionService()
