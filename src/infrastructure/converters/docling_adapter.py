"""Adapter implementing DocumentConverterPort using the Docling pipeline."""
import os
import time
from typing import Dict, Any

try:
    from docling_core.types.doc import ImageRefMode  # type: ignore
except ImportError:
    ImageRefMode = None

from src.domain.model import ConversionResult
from src.domain.ports import DocumentConverterPort
from src.domain.rules import IMAGE_EXTENSIONS, PDF_EXTENSIONS, extension_of
from src.infrastructure.converters.docling_pipeline import get_converter
from src.infrastructure.logging.stream_logger import logger


class DoclingConverterAdapter(DocumentConverterPort):
    """Docling implementation of DocumentConverterPort."""

    def convert(self, file_path: str, filename: str = "", embed_images: bool = True) -> ConversionResult:
        tag = filename or os.path.basename(file_path)
        ext = extension_of(tag)
        is_pdf = ext in PDF_EXTENSIONS
        is_image = ext in IMAGE_EXTENSIONS

        conv = get_converter()
        logger.info("[%s] Running Docling conversion...", tag)
        t_start = time.monotonic()
        res = conv.convert(file_path)
        conv_ms = round((time.monotonic() - t_start) * 1000)
        logger.info("[%s] Docling model pipeline finished in %d ms.", tag, conv_ms)

        doc = res.document
        logger.info("[%s] Exporting Markdown and plain text (embed_images=%s)...", tag, embed_images)
        if embed_images:
            markdown = doc.export_to_markdown(image_mode=ImageRefMode.EMBEDDED)
        else:
            markdown = doc.export_to_markdown()

        from src.domain.quality_gate import strip_tiny_decorative_images
        markdown = strip_tiny_decorative_images(markdown)

        try:
            text = doc.export_to_text()
        except Exception:  # noqa: BLE001
            text = ""

        numpages = doc.num_pages()
        logger.info(
            "[%s] Export complete: %d markdown chars, %d text chars, %d pages.",
            tag, len(markdown), len(text), numpages
        )

        engine = "docling-pdf" if is_pdf else ("docling-image" if is_image else "docling-native")

        return ConversionResult(
            checksum="",  # set by application service
            markdown=markdown,
            text=text,
            raw_text=text,
            numpages=numpages,
            engine=engine,
            duration_ms=conv_ms,
            info={"title": tag},
        )
