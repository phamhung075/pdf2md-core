"""Pure domain metadata extraction and non-destructive YAML frontmatter engine.

Strictly follows DDD: Zero external HTTP, ML framework, or I/O imports.
"""
import datetime
import re
from typing import Any, Dict, List, Optional, Tuple


# Common stop words for fast, dependency-free language classification
_LANGUAGE_PATTERNS = {
    "en": {"the", "and", "to", "of", "in", "for", "with", "on", "that", "by", "this", "from", "at", "as", "is"},
    "fr": {"le", "la", "les", "et", "de", "du", "des", "en", "pour", "dans", "avec", "sur", "par", "une", "est"},
    "es": {"el", "la", "los", "las", "y", "de", "del", "en", "para", "con", "por", "un", "una", "es", "que"},
    "de": {"der", "die", "das", "und", "in", "von", "zu", "mit", "auf", "für", "eine", "einer", "ist", "nicht"},
    "vi": {"và", "của", "cho", "trong", "với", "các", "những", "được", "người", "không", "này", "khi", "tại"},
}


def detect_language(text: str) -> str:
    """Heuristically detects primary natural language using token frequency intersection.

    Returns ISO 639-1 code ('en', 'fr', 'es', 'de', 'vi') or 'und' (undetermined).
    """
    if not text:
        return "und"

    words = set(re.findall(r"\b[\w\u00C0-\u024F\u1EA0-\u1EF9]+\b", text.lower()[:5000]))
    if not words:
        return "und"

    scores: Dict[str, int] = {}
    for lang, stop_words in _LANGUAGE_PATTERNS.items():
        overlap = len(words.intersection(stop_words))
        if overlap > 0:
            scores[lang] = overlap

    if not scores:
        return "en"  # fallback default

    best_lang, best_score = max(scores.items(), key=lambda item: item[1])
    return best_lang if best_score >= 2 else "en"


def count_markdown_tables(text: str) -> int:
    """Accurately counts GitHub Flavored Markdown (GFM) pipe tables.

    Matches table separator row e.g. | :--- | :---: | --- | or |---|---|
    """
    if not text:
        return 0

    count = 0
    for line in text.splitlines():
        trimmed = line.strip()
        if trimmed.startswith("|") and trimmed.endswith("|") and "---" in trimmed:
            # Verify each column segment is dashes/colons
            parts = [p.strip() for p in trimmed.strip("|").split("|")]
            if len(parts) >= 1 and all(re.match(r"^:?-{3,}:?$", p) for p in parts):
                count += 1
    return count


def extract_document_title(text: str, fallback_filename: Optional[str] = None) -> str:
    """Extracts high-confidence document title from Markdown headings or first clean line."""
    if not text:
        return _clean_filename_title(fallback_filename)

    # 1. Check for Level-1 heading: # Title
    h1_match = re.search(r"^\s*#\s+([^\n]+)", text, re.MULTILINE)
    if h1_match:
        title = h1_match.group(1).strip()
        # Strip bold/italic markup from title if present
        title = re.sub(r"[*_~`]", "", title).strip()
        if title:
            return title

    # 2. Check for Level-2 heading: ## Title
    h2_match = re.search(r"^\s*##\s+([^\n]+)", text, re.MULTILINE)
    if h2_match:
        title = h2_match.group(1).strip()
        title = re.sub(r"[*_~`]", "", title).strip()
        if title:
            return title

    # 3. First non-empty, non-punctuation line under 80 characters
    for line in text.splitlines()[:10]:
        cleaned = line.strip().lstrip("#").strip()
        cleaned = re.sub(r"[*_~`]", "", cleaned).strip()
        if 3 <= len(cleaned) <= 80 and not cleaned.startswith("|") and not cleaned.startswith("---"):
            return cleaned

    return _clean_filename_title(fallback_filename)


def _clean_filename_title(filename: Optional[str]) -> str:
    if not filename:
        return "Untitled Document"
    base = re.sub(r"\.[a-zA-Z0-9]+$", "", filename)
    base = re.sub(r"[-_]+", " ", base).strip()
    return base.title() if base else "Untitled Document"


def parse_existing_frontmatter(markdown: str) -> Tuple[Dict[str, Any], str]:
    """Parses existing YAML frontmatter block if present.

    Returns (metadata_dict, remaining_body_markdown).
    """
    if not markdown.startswith("---"):
        return {}, markdown

    lines = markdown.splitlines(keepends=True)
    if not lines or lines[0].strip() != "---":
        return {}, markdown

    end_idx = -1
    for idx in range(1, len(lines)):
        if lines[idx].strip() == "---":
            end_idx = idx
            break

    if end_idx == -1:
        return {}, markdown

    frontmatter_lines = lines[1:end_idx]
    body = "".join(lines[end_idx + 1:]).lstrip("\r\n")

    metadata: Dict[str, Any] = {}
    for line in frontmatter_lines:
        line_clean = line.strip()
        if not line_clean or line_clean.startswith("#"):
            continue
        if ":" in line_clean:
            key, val = line_clean.split(":", 1)
            k = key.strip()
            v = val.strip().strip("'\"")
            if v.isdigit():
                metadata[k] = int(v)
            elif v.lower() in ("true", "yes"):
                metadata[k] = True
            elif v.lower() in ("false", "no"):
                metadata[k] = False
            else:
                metadata[k] = v

    return metadata, body


def extract_metadata(
    markdown: str,
    filename: Optional[str] = None,
    page_count: Optional[int] = None,
    extra_fields: Optional[Dict[str, Any]] = None,
) -> Dict[str, Any]:
    """Extracts structured document metadata dictionary suitable for RAG and vector databases."""
    existing_meta, body = parse_existing_frontmatter(markdown)

    raw_words = body.split()
    word_count = len(raw_words)
    reading_time_min = max(1, round(word_count / 200)) if word_count > 0 else 0
    tables_count = count_markdown_tables(body)
    lang = detect_language(body)
    title = existing_meta.get("title") or extract_document_title(body, filename)

    now_iso = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")

    metadata: Dict[str, Any] = {
        "title": title,
        "date": existing_meta.get("date", now_iso),
        "language": existing_meta.get("language", lang),
        "words": word_count,
        "reading_time_min": reading_time_min,
        "tables": tables_count,
        "generator": "pdf2md-core",
    }

    if page_count is not None and page_count > 0:
        metadata["pages"] = page_count
    elif "pages" in existing_meta:
        metadata["pages"] = existing_meta["pages"]

    if filename:
        metadata["source_file"] = filename

    # Merge any user-provided extra fields
    if extra_fields:
        metadata.update(extra_fields)

    # Preserve any other previously existing fields
    for k, v in existing_meta.items():
        if k not in metadata:
            metadata[k] = v

    return metadata


def render_yaml_frontmatter(metadata: Dict[str, Any]) -> str:
    """Renders a metadata dictionary into a valid, formatted YAML frontmatter string block."""
    lines = ["---"]
    for key, value in metadata.items():
        if isinstance(value, str):
            # Escape strings with quotes if they contain special characters or colons
            if any(c in value for c in (":", "#", "[", "]", "{", "}", "\n", "'", "\"")):
                safe_val = value.replace('"', '\\"')
                lines.append(f'{key}: "{safe_val}"')
            else:
                lines.append(f"{key}: {value}")
        elif isinstance(value, bool):
            lines.append(f"{key}: {'true' if value else 'false'}")
        elif isinstance(value, (int, float)):
            lines.append(f"{key}: {value}")
        elif isinstance(value, list):
            lines.append(f"{key}:")
            for item in value:
                lines.append(f"  - {item}")
        else:
            lines.append(f"{key}: {value}")
    lines.append("---")
    return "\n".join(lines)


def prepend_yaml_frontmatter(
    markdown: str,
    filename: Optional[str] = None,
    page_count: Optional[int] = None,
    extra_fields: Optional[Dict[str, Any]] = None,
) -> str:
    """Non-destructively prepends valid YAML frontmatter to Markdown text without altering body or tables."""
    _, body = parse_existing_frontmatter(markdown)
    meta = extract_metadata(markdown, filename=filename, page_count=page_count, extra_fields=extra_fields)
    frontmatter = render_yaml_frontmatter(meta)
    return f"{frontmatter}\n\n{body.lstrip()}"
