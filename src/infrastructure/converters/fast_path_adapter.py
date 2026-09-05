"""Fast path converter adapter for digital PDFs using compiled Rust (pdf-oxide) with pypdf fallbacks."""
import os
import time
from typing import Optional

from src.domain.model import ConversionResult
from src.domain.ports import DocumentConverterPort
from src.domain.rules import is_digital_document
from src.infrastructure.config import config
from src.infrastructure.logging.stream_logger import logger

ENGINE_PDF_OXIDE = "pdf-oxide-fast-path"
ENGINE_PYPDF = "pypdf-fast-path"

try:
    from pdf_oxide import PdfDocument  # type: ignore
    _HAVE_PDF_OXIDE = True
except Exception:  # pragma: no cover
    PdfDocument = None
    _HAVE_PDF_OXIDE = False

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
    """High-performance Rust (pdf-oxide) with permissive pypdf / pypdfium2 fallbacks."""

    def is_enabled(self) -> bool:
        """Returns True if the fast path is opted-in and dependencies are available."""
        return (_HAVE_PDF_OXIDE or _HAVE_PYPDF or _HAVE_PDFIUM) and config.pdf_fast_path

    def is_digital(self, path: str) -> bool:
        """Probes whether the document has a digital text layer on almost all pages."""
        if not self.is_enabled():
            return False

        # 1. Primary probe: pdf-oxide (compiled Rust, instant)
        if _HAVE_PDF_OXIDE and PdfDocument is not None:
            try:
                doc = PdfDocument(path)
                total = int(doc.page_count)
                if total == 0:
                    return False
                pages_words = [len(doc.extract_words(p)) for p in range(total)]
                eligible = is_digital_document(pages_words)
                logger.info(
                    "Digital PDF probe (pdf-oxide) for %s: %d pages -> %s",
                    os.path.basename(path),
                    len(pages_words),
                    "eligible for fast-path" if eligible else "falls through to Docling",
                )
                return eligible
            except Exception as e:
                logger.debug("pdf-oxide probe failed (%s): %s", path, e)

        # 2. Fallback probe: pypdf
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

        # 3. Tertiary fallback probe: pypdfium2
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
        t_start = time.monotonic()

        total_pages = 0
        raw_markdown = ""
        text = ""
        engine_used = ENGINE_PDF_OXIDE

        # Tier 1: Primary engine — pdf-oxide (Rust compiled core, native Markdown)
        if _HAVE_PDF_OXIDE and PdfDocument is not None:
            try:
                logger.info("[%s] Converting digital PDF with pdf-oxide (Rust core)...", tag)
                doc = PdfDocument(file_path)
                total_pages = int(doc.page_count)
                raw_markdown = doc.to_markdown_all()
                text = doc.to_plain_text_all() if hasattr(doc, "to_plain_text_all") else ""
                engine_used = ENGINE_PDF_OXIDE
            except Exception as e:
                logger.warning("[%s] pdf-oxide conversion error: %s, falling back to pypdf", tag, e)

        # Tier 2: Secondary fallback — pypdf layout extraction
        if not raw_markdown and _HAVE_PYPDF and pypdf is not None:
            try:
                logger.info("[%s] Converting digital PDF with pypdf fallback...", tag)
                reader = pypdf.PdfReader(file_path)
                total_pages = len(reader.pages)
                page_chunks = []
                for pno, page in enumerate(reader.pages, start=1):
                    try:
                        p_text = page.extract_text(extraction_mode="layout") or page.extract_text() or ""
                    except Exception:
                        p_text = page.extract_text() or ""
                    p_text = p_text.strip()
                    if p_text:
                        page_chunks.append(p_text)
                raw_markdown = "\n\n---\n\n".join(page_chunks)
                text = "\n\n".join(page_chunks)
                engine_used = ENGINE_PYPDF
            except Exception as e:
                logger.warning("[%s] pypdf conversion error: %s, attempting pypdfium2 fallback", tag, e)

        # Tier 3: Tertiary fallback — pypdfium2 text extraction
        if not raw_markdown and _HAVE_PDFIUM and pypdfium2 is not None:
            try:
                logger.info("[%s] Converting digital PDF with pypdfium2 fallback...", tag)
                doc = pypdfium2.PdfDocument(file_path)
                total_pages = len(doc)
                page_chunks = []
                for pno in range(total_pages):
                    tp = doc[pno].get_textpage()
                    p_text = tp.get_text_range().strip()
                    if p_text:
                        page_chunks.append(p_text)
                raw_markdown = "\n\n---\n\n".join(page_chunks)
                text = "\n\n".join(page_chunks)
                engine_used = ENGINE_PYPDF
            except Exception as e:
                logger.error("[%s] pypdfium2 conversion error: %s", tag, e)

        from src.domain.quality_gate import normalize_markdown_tables, recover_lost_canvas_tables
        markdown = normalize_markdown_tables(raw_markdown)
        # Reconstruct uncaptured 2D canvas tables if layout geometry is present
        markdown = recover_lost_canvas_tables(markdown, file_path)

        conv_ms = round((time.monotonic() - t_start) * 1000)
        if not text:
            text = markdown

        logger.info("[%s] %s complete (%d pages, %d markdown chars in %d ms)", tag, engine_used, total_pages, len(markdown), conv_ms)

        return ConversionResult(
            checksum="",
            markdown=markdown,
            text=text,
            raw_text=text,
            numpages=total_pages,
            engine=engine_used,
            duration_ms=conv_ms,
            info={"title": tag, "raw_markdown": raw_markdown},
        )
