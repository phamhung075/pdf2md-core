"""Domain entities and value objects for document extraction."""
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, List


class DocumentFormat(str, Enum):
    PDF = "pdf"
    IMAGE = "image"
    NATIVE = "native"
    UNSUPPORTED = "unsupported"


@dataclass(frozen=True)
class ExtractionRequest:
    """Represents a document extraction request."""
    content: bytes
    filename: str
    extension: str = ""
    embed_images: bool = True
    allow_vision_fallback: bool = True
    force_vision: bool = False
    allow_fast_path: bool = True

    def __post_init__(self):
        from src.domain.rules import extension_of, sanitize_filename
        clean_name = sanitize_filename(self.filename)
        object.__setattr__(self, "filename", clean_name)
        if not self.extension:
            object.__setattr__(self, "extension", extension_of(clean_name))



@dataclass
class ConversionResult:
    """Represents the standardized result of a document conversion."""
    checksum: str
    markdown: str
    text: str
    raw_text: str
    numpages: int
    engine: str
    duration_ms: int = 0
    info: Dict[str, Any] = field(default_factory=dict)
    pipeline_trace: List[Dict[str, Any]] = field(default_factory=list)

    def to_dict(self) -> Dict[str, Any]:
        """Serializes to the JSON dictionary expected by client contracts."""
        payload: Dict[str, Any] = {
            "checksum": self.checksum,
            "markdown": self.markdown,
            "text": self.text,
            "raw_text": self.raw_text,
            "numpages": self.numpages,
            "engine": self.engine,
            "duration_ms": self.duration_ms,
            "info": self.info,
            "pipeline_trace": self.pipeline_trace,
        }
        return payload
