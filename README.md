# pdf2md-core

> **High-Performance, Multi-Tier Document-to-Markdown & RAG Ingestion Engine**

[![License](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue.svg)](LICENSE)
[![Python](https://img.shields.io/badge/python-3.12%2B-blue.svg)](https://www.python.org/)
[![Docker](https://img.shields.io/badge/docker-ready-green.svg)](Dockerfile)
[![MCP](https://img.shields.io/badge/MCP-2.1.1-purple.svg)](https://modelcontextprotocol.io)

`pdf2md-core` is an open-source, production-ready document conversion engine designed for Retrieval-Augmented Generation (RAG) pipelines, knowledge bases, and LLM agent ingestion. It converts PDFs, Office documents, and scans into pristine GitHub Flavored Markdown (GFM) while preserving **2D table matrices, multi-column reading order, and LaTeX math formulas**.

---

## Key Features

- **Tri-Tier Hybrid Conversion Pipeline:**
  - **Tier A (Fast Path, ~0.05s):** Instantaneous digital PDF text extraction with 2D geometric span alignment for zero-cost conversions.
  - **Tier B (Local CPU Layout, ~2.0s):** IBM Docling with TableFormer for complex tables, multi-column layouts, and hierarchical reading order. Operates entirely offline on CPU.
  - **Tier C (Vision Rescue, ~1.5s):** Automatic fallback to Gemini Flash API for degraded scans, low-contrast photos, and rotated pages.
- **Model Context Protocol (MCP) Server:** Native MCP integration supporting both `stdio` and `SSE` transports for Claude Desktop, Cursor, and AI coding agents.
- **Zero-GPU Footprint:** Optimized for standard CPU hardware (x86_64 and ARM64 / Apple Silicon).
- **Built-in Dev UI:** Includes an interactive web sandbox with PDF preview and split-screen comparison.
- **Privacy First:** Stateless, in-memory processing with zero persistent document retention.

---

## Architecture Overview

```
                      ┌───────────────────────────┐
                      │     Incoming Document     │
                      └─────────────┬─────────────┘
                                    │
                                    ▼
                      ┌───────────────────────────┐
                      │    Fast Digital Triage    │
                      │       (0.01s - 0.1s)      │
                      └─────────────┬─────────────┘
                                    │
                         Is Digital Text Layer OK?
                                   / \
                            YES   /   \   NO / Empty
                                 /     \
                                ▼       ▼
 ┌────────────────────────────────┐   ┌────────────────────────────────┐
 │     Fast Digital Markdown      │   │    Quality & Language Gate     │
 │   Export (span-level layout)   │   │  (Alphanumeric / Noise ratio)  │
 └────────────────┬───────────────┘   └───────────────┬────────────────┘
                  │                                   │
                  ▼                                   ▼
        Fails Quality Gate?                 Layout Complexity Check
               / \                                   / \
        NO    /   \   YES                     Tables/   \  Scanned /
             /     \                          Columns    \ Degraded
            ▼       ▼                            │        \   │
 ┌─────────────────────┐                         ▼         ▼  ▼
 │ Final Markdown OK   │          ┌───────────────────┐  ┌───────────────────┐
 │ Return to Client    │          │  Local Layout     │  │  Gemini Flash     │
 └─────────────────────┘          │  IBM Docling      │  │  Vision Rescue    │
                                  │  (TableFormer)    │  │  (Remote API)     │
                                  └─────────┬─────────┘  └─────────┬─────────┘
                                            │                      │
                                            ▼                      ▼
                                  ┌──────────────────────────────────────────┐
                                  │        Clean Output Verification         │
                                  │  • Format GFM pipe tables                │
                                  │  • Preserve inline/display LaTeX math    │
                                  └──────────────────────────────────────────┘
```

---

## Quick Start with Docker

### 1. Run via Docker Compose

```bash
git clone https://github.com/phamhung075/pdf2md-core.git
cd pdf2md-core

# Start the microservice (downloads models on first build)
docker compose up -d --build
```

### 2. Verify Health

```bash
curl http://127.0.0.1:3984/health
```

Expected output:
```json
{
  "status": "ok",
  "service": "markdown-extract",
  "engine": "docling",
  "pdfFastPath": true,
  "visionFallback": {
    "enabled": true,
    "model": "gemini-flash-latest",
    "hasApiKey": false
  },
  "devUi": true
}
```

### 3. Open the Interactive Dev UI

Navigate to [http://127.0.0.1:3984/](http://127.0.0.1:3984/) in your browser to test documents with live PDF side-by-side comparison.

---

## API Usage

### Convert Document to Markdown

```bash
curl -X POST http://127.0.0.1:3984/extract \
  -H "X-File-Name: sample.pdf" \
  -H "Content-Type: application/pdf" \
  --data-binary "@path/to/document.pdf"
```

Response format:
```json
{
  "status": "ok",
  "engine": "fast_path",
  "duration_ms": 45,
  "page_count": 4,
  "markdown": "# Document Title\n\n| Column 1 | Column 2 |\n|---|---|\n| Data A | Data B |\n"
}
```

### Force Vision Rescue (Gemini Fallback)

To force visual OCR rescue on degraded scans:

```bash
curl -X POST "http://127.0.0.1:3984/extract?force_vision=1" \
  -H "X-File-Name: scan.pdf" \
  --data-binary "@path/to/scan.pdf"
```

---

## Model Context Protocol (MCP) Integration

`pdf2md-core` includes a built-in MCP server for AI coding assistants and agents:

### Stdio Transport (e.g., Claude Desktop, Cursor)

Add to your `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "pdf2md": {
      "command": "python",
      "args": ["/path/to/pdf2md-core/run_mcp.py"]
    }
  }
}
```

---

## Environment Variables

| Variable | Default | Description |
| :--- | :--- | :--- |
| `DOCLING_SERVICE_PORT` | `3984` | HTTP server port |
| `DOCLING_SERVICE_HOST` | `0.0.0.0` | Bind host address |
| `DOCLING_PDF_FAST_PATH` | `1` | Enable fast-path digital text triage |
| `DOCLING_EMBED_IMAGES` | `1` | Enable image extraction and referencing |
| `DOCLING_OCR_LANGS` | `eng,fra,vie` | OCR languages for fallback engine |
| `DOCLING_DEV_UI` | `1` | Enable/disable browser test UI |
| `GEMINI_API_KEY` | `""` | Google Gemini API key for Tier C vision rescue |
| `VISION_MODEL` | `gemini-flash-latest` | Gemini model identifier for vision rescue |
| `MAX_UPLOAD_SIZE_MB` | `100` | Maximum file upload size in megabytes |

---

## Running Tests

```bash
# Install dependencies
pip install -r requirements.txt

# Run unit and integration tests
pytest tests/ -v

# Run automated service check
python test_service.py --health
```

---

## License

This project is licensed under the **MIT License** or **Apache License 2.0** — see the [LICENSE](LICENSE) file for details.
