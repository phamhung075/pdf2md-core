"""Domain layer containing pure business models, ports, and conversion rules."""
from src.domain.model import ConversionResult, DocumentFormat, ExtractionRequest
from src.domain.rules import (
    IMAGE_EXTENSIONS,
    NATIVE_EXTENSIONS,
    PDF_EXTENSIONS,
    SUPPORTED_EXTENSIONS,
    extension_of,
    is_supported_extension,
)

__all__ = [
    "ConversionResult",
    "ExtractionRequest",
    "DocumentFormat",
    "PDF_EXTENSIONS",
    "IMAGE_EXTENSIONS",
    "NATIVE_EXTENSIONS",
    "SUPPORTED_EXTENSIONS",
    "extension_of",
    "is_supported_extension",
]
