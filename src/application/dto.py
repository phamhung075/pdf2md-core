"""Data Transfer Objects for application inputs and outputs."""
from dataclasses import dataclass
from typing import Any, Dict, Optional


@dataclass(frozen=True)
class ConversionRequestDTO:
    file_bytes: bytes
    filename: str
    embed_images: bool = True


@dataclass(frozen=True)
class ConversionResponseDTO:
    checksum: str
    markdown: str
    text: str
    raw_text: str
    numpages: int
    engine: str
    duration_ms: int
    info: Dict[str, Any]
