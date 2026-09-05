"""Fast path converter adapter for digital PDFs using PyMuPDF and pymupdf4llm."""
import os
import time
from typing import Optional

from src.domain.model import ConversionResult
from src.domain.ports import DocumentConverterPort
from src.domain.rules import is_digital_document
from src.infrastructure.config import config
from src.infrastructure.logging.stream_logger import logger

ENGINE = "pymupdf4llm"

try:
    import pymupdf  # type: ignore
    _HAVE_PYMUPDF = True
except Exception:  # pragma: no cover
    pymupdf = None
    _HAVE_PYMUPDF = False


# Geometric thresholds (in PDF points) for recovering table boxes that the layout
# classifier mislabeled as plain text. The goal is to reconstruct a structured
# Markdown table from 2D span geometry using rectangle-zone / whitespace analysis
# rather than relying on textline y-alignment, which breaks for sparse bilingual
# layouts where each column wraps independently.
_COL_GUTTER_PT = 15.0  # horizontal whitespace wider than this separates columns
_ALIGNED_MIN_MULTI_COL_LINES = 2
_ALIGNED_MIN_RATIO = 0.5  # majority of textlines must populate >= 2 columns
_ROW_GAP_MIN_PT = 10.0  # vertical whitespace wider than this separates rows
_ROW_GAP_LINE_HEIGHT_FACTOR = 1.5


def _span_geometry(s) -> tuple:
    """Returns (x0, y0, x1, y1) for a pymupdf4llm span, or None if unusable."""
    bbox = s.get("bbox", [0, 0, 0, 0])
    if not bbox or len(bbox) < 4:
        return None
    x0, y0, x1, y1 = (float(v) for v in bbox[:4])
    if x1 <= x0 or y1 <= y0:
        return None
    return x0, y0, x1, y1


def _collect_spans(textlines) -> list:
    """Flattens textlines into span dicts with text and geometry, keyed by textline index."""
    spans = []
    for tl_idx, tl in enumerate(textlines):
        for s in tl.get("spans", []):
            text = (s.get("text") or "").strip()
            if not text:
                continue
            geo = _span_geometry(s)
            if geo is None:
                continue
            x0, y0, x1, y1 = geo
            spans.append(
                {"text": text, "x0": x0, "y0": y0, "x1": x1, "y1": y1, "tl": tl_idx}
            )
    return spans


def _detect_columns(spans: list, box_width: float) -> list:
    """Finds column x-ranges by merging span x-intervals and splitting at wide gutters.

    Returns a list of (x0, x1) occupied column ranges ordered left-to-right.
    """
    # Ignore spans wide enough to bridge columns (e.g. a full-width title).
    bridge_limit = box_width * 0.6
    intervals = sorted(
        (s["x0"], s["x1"]) for s in spans if (s["x1"] - s["x0"]) <= bridge_limit
    )
    columns = []
    for x0, x1 in intervals:
        if not columns or (x0 - columns[-1][1]) > _COL_GUTTER_PT:
            columns.append([x0, x1])
        else:
            columns[-1][1] = max(columns[-1][1], x1)
    return [(c[0], c[1]) for c in columns]


def _assign_column(spans: list, columns: list) -> dict:
    """Maps each span to its closest column center and returns {id(span): col_index}."""
    centers = [(c0 + c1) / 2.0 for c0, c1 in columns]
    assignment = {}
    for s in spans:
        cx = (s["x0"] + s["x1"]) / 2.0
        assignment[id(s)] = min(range(len(centers)), key=lambda i: abs(centers[i] - cx))
    return assignment


def _median(values: list) -> float:
    if not values:
        return 0.0
    vals = sorted(values)
    n = len(vals)
    mid = n // 2
    return vals[mid] if n % 2 else (vals[mid - 1] + vals[mid]) / 2.0


def _clean_cell(text: str) -> str:
    """Escapes markdown pipe characters and collapses internal line breaks."""
    return text.replace("|", "\\|").replace("\n", " ")


def _rows_from_textlines(spans: list, num_cols: int, col_of: dict) -> list:
    """Builds one row per textline (aligned layouts, e.g. PASSAGERS table)."""
    by_tl = {}
    for s in spans:
        by_tl.setdefault(s["tl"], []).append(s)

    rows = []
    for tl_idx in sorted(by_tl):
        row = [""] * num_cols
        for s in sorted(by_tl[tl_idx], key=lambda x: x["x0"]):
            text = _clean_cell(s["text"])
            col = col_of[id(s)]
            row[col] = (row[col] + " " + text).strip()
        rows.append(row)
    return rows


def _rows_from_bands(spans: list, num_cols: int, col_of: dict) -> list:
    """Groups spans into horizontal rectangle zones separated by full-width vertical gaps.

    This handles sparse bilingual layouts (e.g. "AVANT VOTRE DÉPART") where each column
    wraps independently, so textline y-alignment does not reflect logical rows.
    """
    heights = [s["y1"] - s["y0"] for s in spans]
    line_h = _median(heights) or 8.0
    row_gap = max(_ROW_GAP_MIN_PT, _ROW_GAP_LINE_HEIGHT_FACTOR * line_h)

    # Merge span y-intervals; a gap wider than row_gap is a row boundary.
    y_intervals = sorted((s["y0"], s["y1"]) for s in spans)
    bands = []
    for y0, y1 in y_intervals:
        if not bands or (y0 - bands[-1][1]) > row_gap:
            bands.append([y0, y1])
        else:
            bands[-1][1] = max(bands[-1][1], y1)

    rows = []
    for by0, by1 in bands:
        cells = [[] for _ in range(num_cols)]
        for s in spans:
            if s["y0"] < by1 and s["y1"] > by0:
                cells[col_of[id(s)]].append(s)

        row = []
        for col_cells in cells:
            col_cells.sort(key=lambda s: (s["y0"], s["x0"]))
            row.append("<br>".join(_clean_cell(s["text"]) for s in col_cells))
        rows.append(row)
    return rows


def _recover_unclassified_tables(parsed_doc) -> None:
    """Detects multi-column text boxes (e.g. key-value forms or receipt coupon tables)
    that the GNN layout classifier labeled as plain text, and promotes them to table boxes
    so their individual rows are preserved instead of being collapsed into a single line.

    Uses 2D span geometry rather than textline alignment:

    * Vertical whitespace gutters (>15pt) locate columns.
    * If a majority of textlines populate multiple columns, rows come from those textlines.
    * Otherwise, full-width horizontal whitespace gaps split the box into rectangle zones
      (bands), and each band's spans are grouped per column in reading order.
    """
    for page in getattr(parsed_doc, "pages", []):
        for b in getattr(page, "boxes", []):
            if getattr(b, "boxclass", None) != "text":
                continue
            textlines = getattr(b, "textlines", None)
            if not textlines or len(textlines) < 2:
                continue

            spans = _collect_spans(textlines)
            if len(spans) < 2:
                continue

            box_width = float(b.x1 - b.x0)
            columns = _detect_columns(spans, box_width)
            num_cols = len(columns)
            if num_cols < 2:
                continue

            col_of = _assign_column(spans, columns)

            # Decide whether the box is an aligned table (rows == textlines) or a sparse
            # side-by-side section (rows == horizontal bands of independent column flow).
            filled_by_tl = {}
            for s in spans:
                filled_by_tl.setdefault(s["tl"], set()).add(col_of[id(s)])
            multi_col_lines = sum(1 for cols in filled_by_tl.values() if len(cols) >= 2)

            aligned = multi_col_lines >= _ALIGNED_MIN_MULTI_COL_LINES and (
                multi_col_lines / len(textlines)
            ) >= _ALIGNED_MIN_RATIO

            if aligned:
                extracted_rows = _rows_from_textlines(spans, num_cols, col_of)
            else:
                extracted_rows = _rows_from_bands(spans, num_cols, col_of)

            non_empty_rows = [r for r in extracted_rows if any(c.strip() for c in r)]
            if len(non_empty_rows) < 2:
                continue

            header_row = non_empty_rows[0]
            md_lines = [
                "| " + " | ".join(header_row) + " |",
                "| " + " | ".join(["---"] * num_cols) + " |",
            ]
            for r in non_empty_rows[1:]:
                md_lines.append("| " + " | ".join(r) + " |")

            b.boxclass = "table"
            b.table = {
                "bbox": [b.x0, b.y0, b.x1, b.y1],
                "row_count": len(non_empty_rows),
                "col_count": num_cols,
                "cells": None,
                "extract": non_empty_rows,
                "markdown": "\n".join(md_lines) + "\n\n",
            }


class FastPathConverterAdapter(DocumentConverterPort):
    """PyMuPDF / pymupdf4llm implementation for clean digital PDFs."""

    def is_enabled(self) -> bool:
        """Returns True if the fast path is opted-in and dependencies are available."""
        return _HAVE_PYMUPDF and config.pdf_fast_path

    def is_digital(self, path: str) -> bool:
        """Probes whether the document has a digital text layer on almost all pages."""
        if not _HAVE_PYMUPDF or pymupdf is None:
            return False
        try:
            doc = pymupdf.open(path)
            try:
                total = doc.page_count
                if total == 0:
                    return False
                pages_words = [len(page.get_text("words")) for page in doc]
            finally:
                doc.close()
            eligible = is_digital_document(pages_words)
            logger.info(
                "Digital PDF probe for %s: %d pages -> %s",
                os.path.basename(path),
                len(pages_words),
                "eligible for pymupdf4llm" if eligible else "falls through to Docling",
            )
            return eligible
        except Exception as e:
            logger.warning("Fast-path probe failed to read PDF (%s): %s", path, e)
            return False

    def convert(self, file_path: str, filename: str = "", embed_images: bool = True) -> ConversionResult:
        import pymupdf4llm  # type: ignore

        tag = filename or os.path.basename(file_path)
        logger.info("[%s] Converting digital PDF with pymupdf4llm (embed_images=%s)...", tag, embed_images)
        t_start = time.monotonic()
        try:
            from pymupdf4llm.helpers.document_layout import parse_document
            parsed_doc = parse_document(file_path, embed_images=embed_images)
            _recover_unclassified_tables(parsed_doc)
            raw_markdown = parsed_doc.to_markdown(embed_images=embed_images)
        except Exception as e:
            logger.debug("[%s] Document layout parse fallback to standard to_markdown: %s", tag, e)
            raw_markdown = pymupdf4llm.to_markdown(file_path, embed_images=embed_images)

        from src.domain.quality_gate import normalize_markdown_tables
        markdown = normalize_markdown_tables(raw_markdown)
        conv_ms = round((time.monotonic() - t_start) * 1000)

        doc = pymupdf.open(file_path)
        try:
            total = doc.page_count
            parts = [page.get_text("text").strip() for page in doc]
        finally:
            doc.close()

        text = "\n\n".join(p for p in parts if p)
        logger.info("[%s] pymupdf4llm conversion complete (%d pages, %d markdown chars)", tag, total, len(markdown))

        return ConversionResult(
            checksum="",
            markdown=markdown,
            text=text,
            raw_text=text,
            numpages=total,
            engine=ENGINE,
            duration_ms=conv_ms,
            info={"title": tag, "raw_markdown": raw_markdown},
        )
