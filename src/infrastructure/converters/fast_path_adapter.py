"""Fast path converter adapter for digital PDFs using permissive pypdf and pypdfium2."""
import os
import time
from typing import Optional

from src.domain.model import ConversionResult
from src.domain.ports import DocumentConverterPort
from src.domain.rules import is_digital_document
from src.infrastructure.config import config
from src.infrastructure.logging.stream_logger import logger

ENGINE = "pypdf-fast-path"

try:
    import pypdf  # type: ignore
    _HAVE_PYPDF = True
except Exception:  # pragma: no cover
    pypdf = None
    _HAVE_PYPDF = False

try:
    import pypdfium2  # type: ignore
    _HAVE_PDFIUM = True
except Exception:  # pragma: no cover
    pypdfium2 = None
    _HAVE_PDFIUM = False


class FastPathConverterAdapter(DocumentConverterPort):
    """Permissive pypdf / pypdfium2 implementation for clean digital PDFs without copyleft risk."""

    def is_enabled(self) -> bool:
        """Returns True if the fast path is opted-in and dependencies are available."""
        return (_HAVE_PYPDF or _HAVE_PDFIUM) and config.pdf_fast_path

    def is_digital(self, path: str) -> bool:
        """Probes whether the document has a digital text layer on almost all pages."""
        if not self.is_enabled():
            return False

        # 1. Try pypdf probing
        if _HAVE_PYPDF and pypdf is not None:
            try:
                reader = pypdf.PdfReader(path)
                total = len(reader.pages)
                if total == 0:
                    return False
                pages_words = [len((page.extract_text() or "").split()) for page in reader.pages]
                eligible = is_digital_document(pages_words)
                logger.info(
                    "Digital PDF probe (pypdf) for %s: %d pages -> %s",
                    os.path.basename(path),
                    len(pages_words),
                    "eligible for fast-path" if eligible else "falls through to Docling",
                )
                return eligible
            except Exception as e:
                logger.debug("pypdf probe failed (%s): %s", path, e)

        # 2. Fallback to pypdfium2 probing
        if _HAVE_PDFIUM and pypdfium2 is not None:
            try:
                doc = pypdfium2.PdfDocument(path)
                total = len(doc)
                if total == 0:
                    return False
                pages_words = []
                for p in doc:
                    tp = p.get_textpage()
                    txt = tp.get_text_range()
                    pages_words.append(len(txt.split()))
                eligible = is_digital_document(pages_words)
                logger.info(
                    "Digital PDF probe (pypdfium2) for %s: %d pages -> %s",
                    os.path.basename(path),
                    len(pages_words),
                    "eligible for fast-path" if eligible else "falls through to Docling",
                )
                return eligible
            except Exception as e:
                logger.warning("pypdfium2 probe failed (%s): %s", path, e)

        return False

    def convert(self, file_path: str, filename: str = "", embed_images: bool = True) -> ConversionResult:
        tag = filename or os.path.basename(file_path)
        logger.info("[%s] Converting digital PDF with pypdf fast-path...", tag)
        t_start = time.monotonic()

        page_chunks = []
        raw_parts = []
        total_pages = 0

        # Primary engine: pypdf layout extraction
        if _HAVE_PYPDF and pypdf is not None:
            try:
                reader = pypdf.PdfReader(file_path)
                total_pages = len(reader.pages)
                for pno, page in enumerate(reader.pages, start=1):
                    try:
                        p_text = page.extract_text(extraction_mode="layout") or page.extract_text() or ""
                    except Exception:
                        p_text = page.extract_text() or ""

                    p_text = p_text.strip()
                    if p_text:
                        raw_parts.append(p_text)
                        page_chunks.append(p_text)
            except Exception as e:
                logger.warning("[%s] pypdf conversion error: %s, attempting pypdfium2 fallback", tag, e)

        # Secondary fallback: pypdfium2 text extraction
        if not page_chunks and _HAVE_PDFIUM and pypdfium2 is not None:
            try:
                doc = pypdfium2.PdfDocument(file_path)
                total_pages = len(doc)
                for pno in range(total_pages):
                    tp = doc[pno].get_textpage()
                    p_text = tp.get_text_range().strip()
                    if p_text:
                        raw_parts.append(p_text)
                        page_chunks.append(p_text)
            except Exception as e:
                logger.error("[%s] pypdfium2 conversion error: %s", tag, e)

        raw_markdown = "\n\n---\n\n".join(page_chunks)
        from src.domain.quality_gate import normalize_markdown_tables
        markdown = normalize_markdown_tables(raw_markdown)
        conv_ms = round((time.monotonic() - t_start) * 1000)
        text = "\n\n".join(raw_parts)

        logger.info("[%s] pypdf fast-path complete (%d pages, %d markdown chars in %d ms)", tag, total_pages, len(markdown), conv_ms)

        return ConversionResult(
            checksum="",
            markdown=markdown,
            text=text,
            raw_text=text,
            numpages=total_pages,
            engine=ENGINE,
            duration_ms=conv_ms,
            info={"title": tag, "raw_markdown": raw_markdown},
        )
