import os
import re
from typing import List

from src.domain.model import DocumentFormat

# Extensions the service routes. Docling cannot read legacy binary .doc — those (and anything
# else) stay on the consumer's flat-text branch; see README.md.
PDF_EXTENSIONS = {".pdf"}
IMAGE_EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp", ".bmp", ".tiff", ".tif"}
NATIVE_EXTENSIONS = {".docx", ".xlsx", ".pptx", ".html", ".md", ".txt", ".asciidoc"}
SUPPORTED_EXTENSIONS = PDF_EXTENSIONS | IMAGE_EXTENSIONS | NATIVE_EXTENSIONS

# Digital PDF fast-path thresholds
FAST_PATH_MIN_WORDS_PER_PAGE = 5
FAST_PATH_MIN_DIGITAL_FRACTION = 0.90

_CONTROL_CHAR_RE = re.compile(r"[\x00-\x1f\x7f-\x9f]")


def sanitize_filename(filename: str, default: str = "upload.bin") -> str:
    """Sanitizes an untrusted filename from HTTP headers or external inputs.

    - Strips path traversal sequences and directory separators.
    - Strips non-printable ASCII control characters (CRLF, null bytes, tabs).
    - Caps filename length to 255 characters while preserving valid extensions.
    - Falls back to `default` if sanitized result is empty.
    """
    if not filename or not isinstance(filename, str):
        return default

    # Normalize Windows backslashes and extract pure basename
    clean = os.path.basename(filename.replace("\\", "/")).strip()

    # Strip dangerous control characters (CR, LF, null byte, etc.)
    clean = _CONTROL_CHAR_RE.sub("", clean).strip()

    # Disallow relative traversal dots or empty names
    while clean.startswith("."):
        # Allow extension dot if remaining is valid, but disallow '.' and '..'
        if clean in (".", ".."):
            clean = ""
            break
        # Keep leading dot only if it's an extension like '.pdf' without base
        if len(clean) > 1 and clean[1] != ".":
            break
        clean = clean[1:].strip()

    if not clean:
        return default

    # Cap length to 255 chars while preserving extension
    if len(clean) > 255:
        base, ext = os.path.splitext(clean)
        ext = ext[:32]
        clean = base[: 255 - len(ext)] + ext

    return clean or default


def extension_of(filename: str) -> str:
    """Extracts lowercase file extension including dot."""
    clean = sanitize_filename(filename)
    return os.path.splitext(clean)[1].lower()


def is_supported_extension(ext: str) -> bool:
    """Checks if an extension is supported by this service."""
    return ext.lower() in SUPPORTED_EXTENSIONS


def classify_format(ext: str) -> DocumentFormat:
    """Classifies an extension into DocumentFormat."""
    e = ext.lower()
    if e in PDF_EXTENSIONS:
        return DocumentFormat.PDF
    if e in IMAGE_EXTENSIONS:
        return DocumentFormat.IMAGE
    if e in NATIVE_EXTENSIONS:
        return DocumentFormat.NATIVE
    return DocumentFormat.UNSUPPORTED


def is_digital_document(pages_words: List[int]) -> bool:
    """Pure domain rule determining if a PDF qualifies as digital (vs scanned/mixed).

    A PDF qualifies as digital only when >= 90% of its pages carry >= 5 words.
    """
    if not pages_words:
        return False
    digital_pages = sum(1 for words in pages_words if words >= FAST_PATH_MIN_WORDS_PER_PAGE)
    fraction = digital_pages / len(pages_words)
    return fraction >= FAST_PATH_MIN_DIGITAL_FRACTION
