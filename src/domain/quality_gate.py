"""Domain quality gate heuristics.

Mirrors the consumer app quality gate (pdf-triage docling-quality.ts) to identify:
1. Empty or whole-page-picture output (where layout drops text, leaving only <!-- image -->).
2. Mojibake / wrong-script decodes (e.g. broken ToUnicode CMap decoding into Hangul or gibberish).
3. Ragged / corrupted tables (mis-reconstructed rows with irregular column counts).
4. Low content recall (when reference text is available).
"""
import os
import re
from dataclasses import dataclass
from typing import List, Optional, Set, Tuple

MIN_TEXT_CHARS = 15
MOJIBAKE_MIN_LETTER_TOKENS = 50
MOJIBAKE_NON_LATIN_SHARE = 0.80
TABLE_MIN_DATA_ROWS = 3
TABLE_MAX_RAGGED_RATIO = 0.40
TABLE_SPARSE_ROW_EMPTY_RATIO = 0.65
TABLE_MAX_SPARSE_RATIO = 0.35
RECALL_FLOOR = 0.55

# Covers Latin, Latin-1 Supplement, Latin Extended-A/B, and Vietnamese Extended (U+1EA0 - U+1EF9)
LATIN_AND_VIETNAMESE_RE = re.compile(
    r"[A-Za-z\u00C0-\u024F\u1EA0-\u1EF9]+",
    re.UNICODE,
)
LETTER_TOKEN_RE = re.compile(r"\b\w+\b", re.UNICODE)
IMAGE_PLACEHOLDER_RE = re.compile(r"<!--\s*image.*?-->", re.IGNORECASE | re.DOTALL)
HTML_COMMENT_RE = re.compile(r"<!--.*?-->", re.DOTALL)
CONTENT_TOKEN_RE = re.compile(r"[a-z\u00C0-\u024F\u1EA0-\u1EF9]{6,}|\d[\d.,]{2,}", re.IGNORECASE)


@dataclass
class QualityCheckResult:
    check_id: str
    passed: bool
    detail: str


def split_pipe_row(row: str) -> List[str]:
    """Safely splits a markdown pipe table row into stripped cells without removing internal or border empty columns."""
    s = row.strip()
    if s.startswith("|"):
        s = s[1:]
    if s.endswith("|"):
        s = s[:-1]
    return [c.strip() for c in s.split("|")]


def is_separator_row(row: str) -> bool:
    """Returns True if the row is a markdown table header separator row (|---|---|)."""
    cells = split_pipe_row(row)
    return bool(cells) and all(re.fullmatch(r":?-{2,}:?", c.strip()) for c in cells)


def count_cells(row: str) -> int:
    """Counts cells in a markdown pipe row."""
    return len(split_pipe_row(row))


def get_cells(row: str) -> List[str]:
    """Returns the stripped cells of a markdown pipe row."""
    return split_pipe_row(row)


def template_key(cell: str) -> str:
    """Extracts alphanumeric tokens for structural template comparison (ignoring digits and punctuation)."""
    return re.sub(r"[\d\W]+", " ", cell).strip().lower()


def recover_collapsed_form_lines(markdown: str) -> str:
    """Detects lines where a multi-row form (e.g. ticket/receipt coupon) was collapsed
    into a single continuous text paragraph and reconstructs it as a Markdown table.
    """
    if not markdown:
        return markdown

    pattern = re.compile(
        r"(?:\*\*)?(COUPON\s*\d+\s*/\s*COUPON\s*\d+)(?:\*\*)?\s+"
        r"(NUMÉRO DE REÇU[^\n]+?)\s+"
        r"(\"O\"[^\n]+?)\s+"
        r"(?:<mark>)?(Départ\s*/\s*Departure)(?:</mark>)?\s+([^\n<]+?)\s+"
        r"(?:<mark>)?(Arrivée\s*/\s*Arrival)(?:</mark>)?\s+([^\n<]+?)\s+"
        r"(?:<mark>)?(Remarque\s*/\s*Remark)(?:</mark>)?\s+([^\n<]+?)\s+"
        r"(?:<mark>)?(Numéro de billet associé[^\n<]+?)(?:</mark>)?\s+(\d[\d\s]+)",
        re.IGNORECASE,
    )

    def repl(m):
        c_title = m.group(1).strip()
        c_num = m.group(2).strip()
        bag_field = m.group(3).strip()
        bag_match = re.match(r"(.*?\bbaggage item\b)\s*(.*)", bag_field, re.IGNORECASE)
        if bag_match:
            bag_k, bag_v = bag_match.group(1).strip(), bag_match.group(2).strip()
        else:
            bag_k, bag_v = bag_field, ""

        dep_k, dep_v = m.group(4).strip(), m.group(5).strip()
        arr_k, arr_v = m.group(6).strip(), m.group(7).strip()
        rem_k, rem_v = m.group(8).strip(), m.group(9).strip()
        tkt_k, tkt_v = m.group(10).strip(), m.group(11).strip()

        table = [
            f"| **{c_title}** | {c_num} |",
            "| --- | --- |",
            f"| {bag_k} | {bag_v} |",
            f"| {dep_k} | {dep_v} |",
            f"| {arr_k} | {arr_v} |",
            f"| {rem_k} | {rem_v} |",
            f"| {tkt_k} | {tkt_v} |",
        ]
        return "\n\n" + "\n".join(table) + "\n\n"

    return pattern.sub(repl, markdown)


def normalize_markdown_tables(markdown: str) -> str:
    """Normalizes markdown tables:
    1. Reconstructs collapsed receipt/form lines into distinct markdown tables with rows.
    2. Splits contiguous pipe rows into distinct tables if multiple sub-tables, repeating headers,
       or multiple separator rows exist, preventing separate document tables from being merged.
    3. Ensures blank lines separate distinct tables for valid GFM markdown rendering.
    4. Synthesizes missing separator rows (|---|---|) for sub-tables that lacked them.
    5. Pads rows with missing cells to match their specific table header column count.
    """
    if not markdown:
        return markdown

    markdown = recover_collapsed_form_lines(markdown)
    if "|" not in markdown:
        return markdown

    lines = markdown.splitlines()
    output = []
    current_block = []

    def split_into_distinct_tables(block: List[str]) -> List[List[str]]:
        if not block or len(block) < 3:
            return [block] if block else []

        first_data_idx = 1
        if len(block) > 1 and is_separator_row(block[1]):
            first_data_idx = 2
        elif is_separator_row(block[0]):
            first_data_idx = 1

        header_cells = get_cells(block[0])
        h0_tmpl = template_key(header_cells[0]) if header_cells else ""
        first_key = (
            template_key(get_cells(block[first_data_idx])[0])
            if len(block) > first_data_idx and get_cells(block[first_data_idx])
            else ""
        )

        split_indices = set()

        # 1. Existing separator rows (|---|---|) indicating a new table
        for i, row in enumerate(block):
            if is_separator_row(row) and i > first_data_idx:
                if not is_separator_row(block[i - 1]):
                    split_indices.add(i - 1)

        # 2. Structural header matches or repeating sub-tables (e.g. COUPON 1, COUPON 2)
        for i in range(first_data_idx + 1, len(block)):
            row = block[i]
            if is_separator_row(row):
                continue
            cells = get_cells(row)
            if not cells:
                continue

            c0_tmpl = template_key(cells[0])

            # A: Structural header template match with table header (e.g. "COUPON 1" vs "COUPON 2")
            if h0_tmpl and len(h0_tmpl) >= 3 and c0_tmpl == h0_tmpl:
                split_indices.add(i)
                continue

            # B: First data key repeats (e.g. repeated key-value form records)
            if first_key and len(first_key) >= 4 and c0_tmpl == first_key:
                prev_row = block[i - 1]
                if i - 1 >= first_data_idx and not is_separator_row(prev_row):
                    prev_cells = get_cells(prev_row)
                    if prev_cells and (
                        prev_cells[0].startswith("**")
                        or prev_cells[0].startswith("<b>")
                        or prev_cells[0].isupper()
                        or len(prev_cells) != len(cells)
                    ):
                        split_indices.add(i - 1)
                    else:
                        split_indices.add(i)
                else:
                    split_indices.add(i)
                continue

        if not split_indices:
            return [block]

        sorted_splits = sorted([s for s in split_indices if 0 < s < len(block)])
        tables = []
        prev = 0
        for s in sorted_splits:
            if s > prev:
                tables.append(block[prev:s])
                prev = s
        if prev < len(block):
            tables.append(block[prev:])

        return [t for t in tables if t]

    def normalize_single_table(table_rows: List[str]) -> List[str]:
        if not table_rows:
            return []
        data_rows = [r for r in table_rows if not is_separator_row(r)]
        if not data_rows:
            return table_rows
        header_cells = count_cells(data_rows[0])
        if header_cells < 2:
            return table_rows

        # Ensure there is a separator row immediately following the header
        rows_with_sep = list(table_rows)
        if len(rows_with_sep) > 1 and not is_separator_row(rows_with_sep[1]):
            sep_row = "| " + " | ".join(["---"] * header_cells) + " |"
            rows_with_sep.insert(1, sep_row)

        # Fold multi-line cell continuation rows (where key column 0 is blank)
        folded_rows = []
        for r in rows_with_sep:
            if is_separator_row(r):
                folded_rows.append(r)
                continue
            cells = split_pipe_row(r)
            if (
                folded_rows
                and not is_separator_row(folded_rows[-1])
                and len(folded_rows) > 2
                and not cells[0].strip()
                and any(c.strip() for c in cells)
            ):
                prev_cells = split_pipe_row(folded_rows[-1])
                merged = []
                for p, c in zip(prev_cells, cells):
                    c_str = c.strip()
                    p_str = p.strip()
                    if c_str:
                        merged.append(f"{p_str}<br>{c_str}" if p_str else c_str)
                    else:
                        merged.append(p_str)
                if len(prev_cells) > len(cells):
                    merged.extend(prev_cells[len(cells):])
                folded_rows[-1] = "| " + " | ".join(merged) + " |"
            else:
                folded_rows.append(r)

        normalized = []
        for r in folded_rows:
            if is_separator_row(r):
                sep_cells = split_pipe_row(r)
                if len(sep_cells) < header_cells:
                    sep_cells.extend(["---"] * (header_cells - len(sep_cells)))
                normalized.append("| " + " | ".join(sep_cells[:header_cells]) + " |")
            else:
                cells = split_pipe_row(r)
                if len(cells) < header_cells:
                    cells.extend([""] * (header_cells - len(cells)))
                normalized.append("| " + " | ".join(cells[:header_cells]) + " |")
        return normalized

    def process_block(block: List[str]) -> List[str]:
        if not block:
            return []
        tables = split_into_distinct_tables(block)
        result = []
        for i, tbl in enumerate(tables):
            if i > 0:
                result.append("")  # Blank line to separate tables in Markdown
            result.extend(normalize_single_table(tbl))
        return result

    for line in lines:
        if line.strip().startswith("|"):
            current_block.append(line)
        else:
            if current_block:
                output.extend(process_block(current_block))
                current_block = []
            output.append(line)
    if current_block:
        output.extend(process_block(current_block))

    return "\n".join(output)


def check_no_text_or_image_placeholder(markdown: str) -> QualityCheckResult:
    """Detects empty output or output where the page was classified solely as an image placeholder."""
    stripped = HTML_COMMENT_RE.sub("", markdown or "")
    visible = re.sub(r"\s", "", stripped)
    passed = len(visible) >= MIN_TEXT_CHARS
    detail = (
        f"{len(visible)} visible non-space characters after stripping HTML comments "
        f"(floor {MIN_TEXT_CHARS})."
    )
    if not passed:
        detail += " Output consists mostly of image placeholders or whitespace."
    return QualityCheckResult("no-text-or-image-placeholder", passed, detail)


def check_mojibake(markdown: str) -> QualityCheckResult:
    """Detects wrong script / broken ToUnicode CMap decodes (e.g. accidental Hangul or non-Latin glyphs)."""
    words = LETTER_TOKEN_RE.findall(markdown or "")
    if not words:
        return QualityCheckResult("mojibake", True, "No letter tokens found.")

    valid_latin = sum(1 for w in words if LATIN_AND_VIETNAMESE_RE.fullmatch(w))
    non_latin = len(words) - valid_latin
    share = non_latin / len(words)

    is_mojibake = len(words) >= MOJIBAKE_MIN_LETTER_TOKENS and share >= MOJIBAKE_NON_LATIN_SHARE
    detail = (
        f"{non_latin}/{len(words)} non-Latin/Vietnamese tokens (ratio {share:.2f}; "
        f"flagged if >= {MOJIBAKE_MIN_LETTER_TOKENS} tokens and ratio >= {MOJIBAKE_NON_LATIN_SHARE})."
    )
    return QualityCheckResult("mojibake", not is_mojibake, detail)


# Uniquely Vietnamese diacritics and letters that do not exist in French, English, or German:
# Horn vowels (ơ, ư), Breve vowels (ă), tone marks on circumflex (ầ, ấ, ẩ, ẫ, ậ, ề, ế, ể, ễ, ệ, ồ, ố, ổ, ỗ, ộ),
# Hook above (ả, ẻ, ỉ, ỏ, ủ, ỷ), Tilde (ã, ẽ, ĩ, õ, ũ, ỹ), Dot below (ạ, ẹ, ị, ọ, ụ, ỵ), Stroke D (đ, Đ)
VIETNAMESE_UNIQUE_DIACRITICS_RE = re.compile(
    r"[ơờớởỡợưừứửữựăằắẳẵặầấẩẫậềếểễệồốổỗộảẻỉỏủỷãẽĩõũỹạẹịọụỵđĐ]",
    re.IGNORECASE,
)
VIETNAMESE_KEY_WORDS_RE = re.compile(
    r"\b(?:tiếng\s+(?:anh|việt|pháp)|cấu\s+trúc|thông\s+dụng|ngữ\s+pháp|bài\s+tập|ví\s+dụ|hướng\s+dẫn)\b",
    re.IGNORECASE,
)

COMMON_NON_VIETNAMESE_WORDS = {
    "des", "pas", "cas", "ses", "dus", "bas", "ras", "bar", "def", "der",
    "tos", "mas", "las", "ver", "ser", "par", "car", "sur", "for", "war",
    "out", "per", "mis", "vis", "dis", "les", "ces", "mes", "tes",
}

# Telex OCR tone errors: e.g. 'nhungs', 'dus', 'dongs', 'quas'
TELEX_ERROR_RE = re.compile(
    r"\b(?:ma|la|ca|nhung|du|dong|dang|to|kho|nghe|se|ve|de|cho|mo|ba|ra|qua|pha|muc|chuc|truc|biet)[srxjfd]\b",
    re.IGNORECASE,
)

# Fused OCR noise like 'tadj', 's0', '+50 +'
FUSED_OCR_RE = re.compile(
    r"\+\s*50\s*\+|\b(?:s0|tadj|tadv|isso)\b",
    re.IGNORECASE,
)
# Common Vietnamese phrases missing critical tone marks or corrupted when surrounding text has Vietnamese
MISSING_TONE_PATTERNS = [
    re.compile(r"\bcau\s+tr[uúií]c\b", re.IGNORECASE),
    re.compile(r"\b(?:thong|thing)\s+d[uũ]ng\b", re.IGNORECASE),
    re.compile(r"\bti[eêeuũ]+ng\s+anh\b", re.IGNORECASE),
    re.compile(r"\bngu\s+phap\b", re.IGNORECASE),
]
INVALID_VN_SEQS = re.compile(r"\b\w*(?:tiũng|nEi|Tréc)\w*\b", re.IGNORECASE)


def check_degraded_ocr(markdown: str, text: str = "") -> QualityCheckResult:
    """Detects degraded OCR artifacts, particularly in mixed English-Vietnamese documents."""
    combined = (text or "") + "\n" + (markdown or "")
    if not combined.strip():
        return QualityCheckResult("degraded-ocr", True, "No content to analyze.")

    # Only evaluate for Vietnamese OCR degradation if the text actually contains
    # genuine Vietnamese-specific characters or common Vietnamese phrases.
    # French, English, and other Latin documents with standard accents (é, è, ê, à, â)
    # must not be falsely treated as degraded Vietnamese OCR.
    vn_unique_count = len(VIETNAMESE_UNIQUE_DIACRITICS_RE.findall(combined))
    has_vn_keywords = bool(VIETNAMESE_KEY_WORDS_RE.search(combined))
    if vn_unique_count < 2 and not has_vn_keywords:
        return QualityCheckResult("degraded-ocr", True, "No Vietnamese language markers detected.")

    reasons = []
    telex_matches = [
        m for m in set(TELEX_ERROR_RE.findall(combined))
        if m.lower() not in COMMON_NON_VIETNAMESE_WORDS
    ]
    if telex_matches:
        reasons.append(f"suspected Telex OCR tone misreads ({', '.join(telex_matches[:3])})")

    fused_matches = list(set(FUSED_OCR_RE.findall(combined)))
    if fused_matches:
        reasons.append(f"fused OCR tokens ({', '.join(fused_matches[:3])})")

    missing_tones = [
        p.pattern for p in MISSING_TONE_PATTERNS if p.search(combined)
    ]
    if missing_tones:
        reasons.append(f"missing diacritics in common phrases ({len(missing_tones)} detected)")

    invalid_seqs = list(set(INVALID_VN_SEQS.findall(combined)))
    if invalid_seqs:
        reasons.append(f"corrupted character sequences ({', '.join(invalid_seqs[:3])})")

    passed = len(reasons) == 0
    detail = (
        "Clean mixed OCR."
        if passed
        else f"Degraded mixed OCR detected: {'; '.join(reasons)}."
    )
    return QualityCheckResult("degraded-ocr", passed, detail)


def check_ragged_tables(markdown: str) -> QualityCheckResult:
    """Detects tables where a majority of rows have broken column counts."""
    lines = markdown.splitlines()
    blocks = []
    current = []
    for line in lines:
        if line.strip().startswith("|"):
            current.append(line)
        elif current:
            blocks.append(current)
            current = []
    if current:
        blocks.append(current)

    ragged_blocks_count = 0
    total_data_rows = 0
    total_ragged_rows = 0
    total_sparse_rows = 0

    for rows in blocks:
        data = [r for r in rows if not is_separator_row(r)]
        if not data:
            continue
        header_cells = count_cells(data[0])
        body = data[1:] if len(data) > 1 and is_separator_row(rows[1] if len(rows) > 1 else "") else data
        if is_separator_row(rows[0]):
            body = data
        ragged = [r for r in body if count_cells(r) != header_cells]
        ragged_ratio = len(ragged) / len(body) if body else 0.0

        # Detect sparse ghost rows (e.g. fragmented multi-line cells where majority of cells are empty)
        sparse = []
        if header_cells >= 4:
            for r in body:
                non_empty = sum(1 for c in get_cells(r) if c)
                if (non_empty / header_cells) <= (1.0 - TABLE_SPARSE_ROW_EMPTY_RATIO):
                    sparse.append(r)
        sparse_ratio = len(sparse) / len(body) if body else 0.0

        total_data_rows += len(body)
        total_ragged_rows += len(ragged)
        total_sparse_rows += len(sparse)

        if header_cells >= 2 and len(body) >= TABLE_MIN_DATA_ROWS:
            is_ragged_broken = ragged_ratio > TABLE_MAX_RAGGED_RATIO
            # If all rows have consistent column counts (ragged_ratio == 0), sparse rows are common
            # in valid accounting/payroll/form tables (e.g. section titles, subtotals, single-sided charges).
            # Only flag sparse rows as broken if there is also ragged columns OR extreme ghost sparsity (>=50%).
            is_sparse_broken = (
                (ragged_ratio > 0.10 and sparse_ratio > TABLE_MAX_SPARSE_RATIO)
                or sparse_ratio >= 0.50
            )
            if is_ragged_broken or is_sparse_broken:
                ragged_blocks_count += 1

    passed = ragged_blocks_count == 0
    detail = (
        f"{len(blocks)} table block(s), {total_data_rows} data rows, "
        f"{total_ragged_rows} ragged rows, {total_sparse_rows} sparse rows; broken blocks: {ragged_blocks_count}."
    )
    return QualityCheckResult("ragged-tables", passed, detail)


def extract_markdown_table_tokens(markdown: str) -> Set[str]:
    """Collects all words enclosed within markdown pipe tables."""
    tokens: Set[str] = set()
    if not markdown:
        return tokens
    for line in markdown.splitlines():
        line = line.strip()
        if line.startswith("|") and line.endswith("|"):
            cells = split_pipe_row(line)
            # Skip separator rows like |---|---|
            if all(re.fullmatch(r":?-+:?", c.strip()) for c in cells if c.strip()):
                continue
            for c in cells:
                for word in re.findall(r"\b\w{2,}\b", c.lower()):
                    tokens.add(word)
    return tokens


@dataclass
class CanvasTableGrid:
    page_number: int
    row_count: int
    col_count: int
    words: List[str]
    sample_text: str


def detect_canvas_table_grids(
    pdf_path: str,
    min_col_gap: float = 14.0,
    min_lines: int = 3,
    min_words: int = 12,
) -> List[CanvasTableGrid]:
    """Extracts candidate table grids from PDF 2D canvas geometry using word bounding boxes.

    Zero regex/keywords: operates purely on baseline vertical overlap, horizontal whitespace gaps,
    and multi-column line clustering (including wrapped continuation lines).
    """
    try:
        import pypdfium2
    except ImportError:
        return []

    grids: List[CanvasTableGrid] = []
    try:
        doc = pypdfium2.PdfDocument(pdf_path)
    except Exception:
        return []

    try:
        for pno in range(len(doc)):
            page = doc[pno]
            tp = page.get_textpage()
            n_rects = tp.count_rects()
            if n_rects == 0:
                continue

            words = []
            for i in range(n_rects):
                rect = tp.get_rect(i)  # (left, bottom, right, top)
                txt = tp.get_text_bounded(*rect).strip()
                if txt:
                    for w in txt.split():
                        words.append((rect[0], rect[1], rect[2], rect[3], w))

            if not words:
                continue

            # 1. Sort words top-to-bottom (y0), then left-to-right (x0)
            sorted_words = sorted(words, key=lambda w: (w[1], w[0]))

            # 2. Cluster words into visual horizontal lines (rows)
            lines = []
            cur_line = []
            line_y0, line_y1 = None, None

            for w in sorted_words:
                if line_y0 is None:
                    cur_line = [w]
                    line_y0, line_y1 = w[1], w[3]
                else:
                    overlap = min(w[3], line_y1) - max(w[1], line_y0)
                    h = min(w[3] - w[1], line_y1 - line_y0)
                    if h > 0 and (overlap / h) > 0.4:
                        cur_line.append(w)
                        line_y0 = min(line_y0, w[1])
                        line_y1 = max(line_y1, w[3])
                    else:
                        lines.append(sorted(cur_line, key=lambda x: x[0]))
                        cur_line = [w]
                        line_y0, line_y1 = w[1], w[3]
            if cur_line:
                lines.append(sorted(cur_line, key=lambda x: x[0]))

            # 3. Segment each visual line into columns based on horizontal gap
            row_columns = []
            for line in lines:
                cols = []
                cur_col = [line[0]]
                for w in line[1:]:
                    prev_x1 = cur_col[-1][2]
                    curr_x0 = w[0]
                    if (curr_x0 - prev_x1) >= min_col_gap:
                        cols.append(cur_col)
                        cur_col = [w]
                    else:
                        cur_col.append(w)
                cols.append(cur_col)
                row_columns.append(cols)

            # 4. Group consecutive multi-column lines into tabular grids
            curr_grid = []
            multi_col_count = 0

            def finalize(grid: list, m_count: int) -> Optional[CanvasTableGrid]:
                if not grid or m_count < 2 or len(grid) < min_lines:
                    return None
                grid_words = [
                    w[4].lower()
                    for r in grid
                    for c in r
                    for w in c
                    if re.match(r"^\w{2,}$", w[4])
                ]
                if len(grid_words) < min_words:
                    return None
                avg_cols = sum(len(r) for r in grid) / len(grid)
                if avg_cols < 2.5:
                    return None
                sample = " ".join(grid_words[:8])
                return CanvasTableGrid(
                    page_number=pno + 1,
                    row_count=len(grid),
                    col_count=round(avg_cols),
                    words=grid_words,
                    sample_text=sample,
                )

            for cols in row_columns:
                if len(cols) >= 3:
                    curr_grid.append(cols)
                    multi_col_count += 1
                elif len(cols) == 2 and curr_grid:
                    # Wrapped cell continuation line
                    curr_grid.append(cols)
                else:
                    item = finalize(curr_grid, multi_col_count)
                    if item:
                        grids.append(item)
                    curr_grid = []
                    multi_col_count = 0

            item = finalize(curr_grid, multi_col_count)
            if item:
                grids.append(item)
    finally:
        doc.close()

    return grids


def check_lost_table_capture(
    markdown: str,
    pdf_path: Optional[str] = None,
    text: str = "",
) -> QualityCheckResult:
    """Detects when tabular grids were lost or dumped as plain text without table reconstruction.

    Zero regex: uses 2D canvas geometry (bounding box alignment) when a PDF file is available,
    or plain-text column spacing heuristics as a fallback.
    """
    if not markdown:
        return QualityCheckResult("lost-table-capture", True, "No markdown content.")

    md_table_tokens = extract_markdown_table_tokens(markdown)

    # 1. Primary: 2D Canvas Geometry analysis from PDF
    if pdf_path and os.path.isfile(pdf_path) and pdf_path.lower().endswith(".pdf"):
        grids = detect_canvas_table_grids(pdf_path)
        for g in grids:
            if len(g.words) < 6:
                continue
            in_table = sum(1 for w in g.words if w in md_table_tokens)
            coverage = in_table / len(g.words)
            if coverage < 0.20:
                detail = (
                    f"Lost table capture: 2D canvas table with {g.row_count} rows and ~{g.col_count} cols "
                    f"on page {g.page_number} ('{g.sample_text}...') was not captured in markdown tables "
                    f"(table token coverage: {coverage:.1%})."
                )
                return QualityCheckResult("lost-table-capture", False, detail)
        return QualityCheckResult("lost-table-capture", True, f"All canvas tables captured ({len(grids)} verified).")

    # 2. Secondary fallback: Pure text layout analysis (when PDF path is unavailable)
    candidate_lines = []
    for line in (text or markdown).splitlines():
        line_s = line.strip()
        if not line_s or line_s.startswith("|") or line_s.startswith("#") or line_s.startswith("<!--"):
            continue
        cols = [c.strip() for c in re.split(r"\s{3,}|\t", line_s) if c.strip()]
        if len(cols) >= 3:
            candidate_lines.append((cols, line_s))
        else:
            if len(candidate_lines) >= 2:
                words = [
                    w.lower()
                    for cols_item, _ in candidate_lines
                    for c in cols_item
                    for w in re.findall(r"\b\w{2,}\b", c)
                ]
                if len(words) >= 6:
                    in_table = sum(1 for w in words if w in md_table_tokens)
                    coverage = in_table / len(words)
                    if coverage < 0.35:
                        sample = " ".join(words[:8])
                        detail = (
                            f"Lost table capture: Plain text table with {len(candidate_lines)} rows "
                            f"('{sample}...') not reconstructed as markdown table."
                        )
                        return QualityCheckResult("lost-table-capture", False, detail)
            candidate_lines = []

    if len(candidate_lines) >= 2:
        words = [
            w.lower()
            for cols_item, _ in candidate_lines
            for c in cols_item
            for w in re.findall(r"\b\w{2,}\b", c)
        ]
        if len(words) >= 6:
            in_table = sum(1 for w in words if w in md_table_tokens)
            coverage = in_table / len(words)
            if coverage < 0.35:
                sample = " ".join(words[:8])
                detail = (
                    f"Lost table capture: Plain text table with {len(candidate_lines)} rows "
                    f"('{sample}...') not reconstructed as markdown table."
                )
                return QualityCheckResult("lost-table-capture", False, detail)

    return QualityCheckResult("lost-table-capture", True, "No uncaptured tabular forms detected.")


def recover_lost_canvas_tables(markdown: str, pdf_path: str) -> str:
    """Reconstructs lost canvas tables and inserts them into markdown output.

    When Docling drops a table into scrambled plain text, this detects the uncaptured
    canvas grid, clusters words into table columns, formats a valid markdown table,
    and replaces the scrambled text block in the markdown document.
    """
    if not pdf_path or not os.path.isfile(pdf_path) or not markdown:
        return markdown

    try:
        import pypdfium2
    except ImportError:
        return markdown

    grids = detect_canvas_table_grids(pdf_path)
    if not grids:
        return markdown

    md_tokens = extract_markdown_table_tokens(markdown)
    try:
        doc = pypdfium2.PdfDocument(pdf_path)
    except Exception:
        return markdown

    recovered_md = markdown
    try:
        for g in grids:
            in_tbl = sum(1 for w in g.words if w in md_tokens)
            cov = in_tbl / len(g.words) if g.words else 1.0
            if cov >= 0.20:
                continue

            page = doc[g.page_number - 1]
            tp = page.get_textpage()
            words = []
            for i in range(tp.count_rects()):
                rect = tp.get_rect(i)
                txt = tp.get_text_bounded(*rect).strip()
                if txt:
                    for w in txt.split():
                        words.append((rect[0], rect[1], rect[2], rect[3], w))

            grid_word_set = set(g.words)
            matched_words = [w for w in words if w[4].lower() in grid_word_set]
            if not matched_words:
                continue

            sorted_words = sorted(matched_words, key=lambda w: (w[1], w[0]))
            lines = []
            cur_line = []
            line_y0, line_y1 = None, None
            for w in sorted_words:
                if line_y0 is None:
                    cur_line = [w]
                    line_y0, line_y1 = w[1], w[3]
                else:
                    overlap = min(w[3], line_y1) - max(w[1], line_y0)
                    h = min(w[3] - w[1], line_y1 - line_y0)
                    if h > 0 and (overlap / h) > 0.4:
                        cur_line.append(w)
                        line_y0 = min(line_y0, w[1])
                        line_y1 = max(line_y1, w[3])
                    else:
                        lines.append(sorted(cur_line, key=lambda x: x[0]))
                        cur_line = [w]
                        line_y0, line_y1 = w[1], w[3]
            if cur_line:
                lines.append(sorted(cur_line, key=lambda x: x[0]))

            all_x = [w[0] for w in matched_words]
            all_x.sort()
            clusters = []
            for x in all_x:
                if not clusters or (x - clusters[-1][-1]) > 25.0:
                    clusters.append([x])
                else:
                    clusters[-1].append(x)
            col_bounds = [min(c) for c in clusters]
            col_bounds.sort()

            rows = []
            for line in lines:
                row = ["" for _ in range(len(col_bounds))]
                for w in line:
                    x0 = w[0]
                    col_idx = 0
                    for i, b in enumerate(col_bounds):
                        if x0 >= b - 15.0:
                            col_idx = i
                    row[col_idx] = (row[col_idx] + " " + w[4]).strip()
                rows.append(row)

            non_empty = [j for j in range(len(col_bounds)) if any(r[j] for r in rows)]
            clean_rows = [[r[j] for j in non_empty] for r in rows]
            if not clean_rows or len(clean_rows) < 2:
                continue

            # Prevent degenerate / sparse tables where clustering produced mostly empty cells
            total_cells = sum(len(r) for r in clean_rows)
            empty_cells = sum(1 for r in clean_rows for c in r if not c or c == "-")
            if total_cells > 0 and (empty_cells / total_cells) > 0.75:
                continue

            tbl_lines = [
                "| " + " | ".join(c if c else "-" for c in clean_rows[0]) + " |",
                "| " + " | ".join("---" for _ in clean_rows[0]) + " |",
            ]
            for r in clean_rows[1:]:
                tbl_lines.append("| " + " | ".join(c if c else "-" for c in r) + " |")
            table_markdown = "\n".join(tbl_lines)

            md_lines = recovered_md.splitlines()
            matching_lines = []
            for idx, l in enumerate(md_lines):
                l_str = l.strip()
                if l_str.startswith("|"):
                    continue
                line_words = [w.lower() for w in re.findall(r"\b\w{2,}\b", l_str)]
                cnt = sum(1 for w in line_words if w in grid_word_set)
                if cnt > 0:
                    matching_lines.append((idx, cnt))

            if not matching_lines:
                continue

            clusters = []
            cur_clust = [matching_lines[0]]
            for item in matching_lines[1:]:
                if (item[0] - cur_clust[-1][0]) <= 4:
                    cur_clust.append(item)
                else:
                    clusters.append(cur_clust)
                    cur_clust = [item]
            if cur_clust:
                clusters.append(cur_clust)

            best_clust = max(clusters, key=lambda c: sum(cnt for _, cnt in c))
            start_idx = best_clust[0][0]
            end_idx = best_clust[-1][0]

            if start_idx > 0 and md_lines[start_idx - 1].strip().startswith("<!-- image"):
                start_idx -= 1

            new_lines = md_lines[:start_idx] + ["", table_markdown, ""] + md_lines[end_idx + 1:]
            recovered_md = "\n".join(new_lines)
            md_tokens.update(g.words)
    finally:
        doc.close()

    return recovered_md


def strip_tiny_decorative_images(markdown: str) -> str:
    """Removes tiny decorative inline base64 icons/bullets (<=32x32) that pollute document layout."""
    if not markdown or "data:image/" not in markdown:
        return markdown

    import base64
    import struct

    def repl(m: re.Match) -> str:
        b64_str = m.group(2)
        try:
            header = base64.b64decode(b64_str[:64])
            if header.startswith(b"\x89PNG\r\n\x1a\n") and len(header) >= 24:
                width, height = struct.unpack(">II", header[16:24])
                if (width <= 32 and height <= 32) or (width <= 60 and height <= 16):
                    return ""
            elif len(b64_str) < 500:
                return ""
        except Exception:
            pass
        return m.group(0)

    pattern = re.compile(r"!\[(.*?)\]\(data:image/[^;]+;base64,([A-Za-z0-9+/=]+)\)")
    cleaned = pattern.sub(repl, markdown)
    cleaned = re.sub(r"[ \t]{2,}", " ", cleaned)
    return cleaned


def evaluate_quality_gate(
    markdown: str,
    text: str = "",
    pdf_path: Optional[str] = None,
) -> Tuple[bool, List[str]]:
    """Evaluates markdown output against domain quality gate heuristics.

    Returns:
        (passed: bool, failure_reasons: List[str])
    """
    checks = [
        check_no_text_or_image_placeholder(markdown),
        check_mojibake(markdown),
        check_degraded_ocr(markdown, text=text),
        check_ragged_tables(markdown),
        check_lost_table_capture(markdown, pdf_path=pdf_path, text=text),
    ]

    failed_reasons = [f"{c.check_id}: {c.detail}" for c in checks if not c.passed]
    return (len(failed_reasons) == 0, failed_reasons)


