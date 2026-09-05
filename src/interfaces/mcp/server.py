"""Model Context Protocol (MCP) server for markdown-extract-service.

Enables AI agents (Claude Desktop, Cursor, Antigravity, etc.) to invoke document extraction
tools directly via MCP stdio or SSE transport.
Supports both modern mcp 2.x (MCPServer) and mcp 1.x / standalone FastMCP.
"""
import base64
import os
import sys
from typing import Any, Dict, List, Optional

from src.application.conversion_service import conversion_service
from src.domain.model import ExtractionRequest
from src.domain.rules import SUPPORTED_EXTENSIONS
from src.infrastructure.config import config, uptime_sec
from src.infrastructure.converters.fast_path_adapter import FastPathConverterAdapter

try:
    from mcp.server.mcpserver import MCPServer as _MCPServer  # type: ignore
except ImportError:
    try:
        from mcp.server.fastmcp import FastMCP as _MCPServer  # type: ignore
    except ImportError:
        try:
            from fastmcp import FastMCP as _MCPServer  # type: ignore
        except ImportError:
            _MCPServer = None


def create_mcp_server():
    """Factory creating and configuring the MCP server instance."""
    if _MCPServer is None:
        raise ImportError(
            "The 'mcp' package is required to run the MCP server. "
            "Install it via: pip install 'mcp>=1.2.0'"
        )

    instructions = (
        "Microservice providing deterministic, layout-aware Markdown extraction "
        "from PDFs and office documents (.docx, .xlsx, .pptx, .html, .md, .txt, .asciidoc). "
        "Supports image extraction with embedded base64 Markdown images."
    )

    try:
        mcp = _MCPServer("markdown-extract-service", instructions=instructions)
    except TypeError:
        mcp = _MCPServer("markdown-extract-service")

    @mcp.tool()
    def extract_document(
        file_path: str,
        embed_images: bool = True,
        allow_vision_fallback: bool = True,
        force_vision: bool = False,
    ) -> str:
        """Extracts and converts a local document (PDF, Word, Excel, PPTX, etc.) into Markdown.

        Args:
            file_path: Absolute or relative path to the document on disk.
            embed_images: Whether to extract images/diagrams and embed them as base64 in the Markdown.
            allow_vision_fallback: Whether to rescue degraded scans with Vision LLM (Gemini Flash).
            force_vision: Whether to bypass Docling and transcribe directly with Vision LLM.

        Returns:
            The extracted Markdown string.
        """
        result = conversion_service.convert_file_on_disk(
            file_path,
            embed_images=embed_images,
            allow_vision_fallback=allow_vision_fallback,
            force_vision=force_vision,
        )
        return result.markdown

    @mcp.tool()
    def extract_document_full(
        file_path: str,
        embed_images: bool = True,
        allow_vision_fallback: bool = True,
        force_vision: bool = False,
    ) -> Dict[str, Any]:
        """Converts a local document and returns the full extraction payload.

        Args:
            file_path: Path to the document on disk.
            embed_images: Whether to embed images as base64 data URLs in the markdown.
            allow_vision_fallback: Whether to rescue degraded scans with Vision LLM (Gemini Flash).
            force_vision: Whether to bypass Docling and transcribe directly with Vision LLM.

        Returns:
            Dictionary with markdown, text, raw_text, numpages, engine, checksum, duration_ms.
        """
        result = conversion_service.convert_file_on_disk(
            file_path,
            embed_images=embed_images,
            allow_vision_fallback=allow_vision_fallback,
            force_vision=force_vision,
        )
        return result.to_dict()

    @mcp.tool()
    def extract_document_base64(
        filename: str,
        content_base64: str,
        embed_images: bool = True,
        allow_vision_fallback: bool = True,
        force_vision: bool = False,
    ) -> Dict[str, Any]:
        """Converts document bytes passed as a base64-encoded string into Markdown.

        Args:
            filename: Name of the original file including extension (e.g. report.pdf, data.xlsx).
            content_base64: Raw binary content encoded as a base64 string.
            embed_images: Whether to capture and embed images in the markdown.
            allow_vision_fallback: Whether to rescue degraded scans with Vision LLM (Gemini Flash).
            force_vision: Whether to bypass Docling and transcribe directly with Vision LLM.

        Returns:
            Dictionary with markdown, text, numpages, engine, duration_ms, checksum.
        """
        try:
            data = base64.b64decode(content_base64, validate=True)
        except Exception as e:
            raise ValueError(f"Invalid base64 payload provided in content_base64: {e}") from e

        if len(data) > config.max_upload_size_bytes:
            raise ValueError(
                f"Payload size ({len(data)} bytes) exceeds maximum allowed limit "
                f"of {config.max_upload_size_mb} MB ({config.max_upload_size_bytes} bytes)"
            )

        request = ExtractionRequest(
            content=data,
            filename=filename,
            embed_images=embed_images,
            allow_vision_fallback=allow_vision_fallback,
            force_vision=force_vision,
        )
        result = conversion_service.convert_request(request)
        return result.to_dict()

    @mcp.tool()
    def get_supported_formats() -> List[str]:
        """Returns the list of all file extensions supported by this extraction service."""
        return sorted(list(SUPPORTED_EXTENSIONS))

    @mcp.tool()
    def get_service_health() -> Dict[str, Any]:
        """Checks the status and configuration of the document extraction microservice."""
        adapter = FastPathConverterAdapter()
        return {
            "status": "ok",
            "service": config.service_name,
            "engine": "docling",
            "pdfFastPath": adapter.is_enabled(),
            "visionFallback": {
                "enabled": config.vision_fallback_enabled,
                "model": config.vision_model,
                "hasApiKey": bool(config.gemini_api_key),
                "hasBaseUrl": bool(config.vision_base_url),
            },
            "embedImages": config.embed_images,
            "uptimeSec": uptime_sec(),
            "supportedExtensions": sorted(list(SUPPORTED_EXTENSIONS)),
        }

    return mcp


def run_mcp_server(transport: str = "stdio", host: str = "127.0.0.1", port: int = 3985) -> None:
    """Runs the MCP server using the specified transport ('stdio' or 'sse')."""
    server = create_mcp_server()
    if transport == "sse":
        try:
            server.run(transport="sse", host=host, port=port)
        except TypeError:
            if hasattr(server, "settings"):
                server.settings.host = host
                server.settings.port = port
            server.run(transport="sse")
    else:
        server.run(transport="stdio")


if __name__ == "__main__":
    transport = sys.argv[1] if len(sys.argv) > 1 else "stdio"
    run_mcp_server(transport=transport)
