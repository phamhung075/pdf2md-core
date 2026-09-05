"""Domain ports (interfaces) for document conversion."""
from typing import Protocol, runtime_checkable

from src.domain.model import ConversionResult, ExtractionRequest


@runtime_checkable
class DocumentConverterPort(Protocol):
    """Port for document conversion engines."""

    def convert(self, file_path: str, filename: str = "", embed_images: bool = True) -> ConversionResult:
        """Converts a file on disk into a ConversionResult."""
        ...


@runtime_checkable
class ConversionServicePort(Protocol):
    """Port for the application conversion use case."""

    def convert_request(self, request: ExtractionRequest) -> ConversionResult:
        """Processes an extraction request and returns the conversion result."""
        ...


@runtime_checkable
class VisionRescuePort(Protocol):
    """Port for fallback vision rescue converters (e.g. Gemini Vision)."""

    def is_enabled(self) -> bool:
        """Returns True if the vision rescue engine is configured and ready."""
        ...

    def rescue(
        self,
        file_path: str,
        filename: str = "",
        embed_images: bool = True,
    ) -> ConversionResult:
        """Rescues a document by rendering pages to images and converting via Vision LLM."""
        ...

