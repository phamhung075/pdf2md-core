# pdf2md-core

> **Sub-millisecond, native Rust PDF→Markdown engine** — digital text-layer
> extraction with 2D spatial canvas table reconstruction. Source-available under
> the Business Source License 1.1 (BSL-1.1). Zero GPL/AGPL copyleft dependencies
> (permissive third-party attribution is in [NOTICE](NOTICE) and
> [THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES)).

[![License](https://img.shields.io/badge/license-BSL--1.1-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)

`pdf2md-core` is a compiled, native engine that converts digital (text-layer)
PDFs into clean GitHub Flavored Markdown (GFM) — including pipe tables
reconstructed from 2D span geometry — in microseconds. It is the fast,
zero-cloud-cost foundation for RAG ingestion pipelines, CLI tooling, and
in-browser document processing.

## What's inside

| Component | Description |
| :--- | :--- |
| [`crates/pdf2md-core`](crates/pdf2md-core) | The native Rust engine: byte-level parsing, digital-PDF triage, 2D canvas table reconstruction, AST generation. Ships as a Rust library, static archive (`libpdf2md_core.a`), Python wheel (PyO3/Maturin), and C ABI (`pdf2md.h`). |
| [`crates/pdf2md-cli`](crates/pdf2md-cli) | `pdf2md` — a pipe-friendly, native command-line tool built on the core. |
| [`crates/pdf2md-wasm`](crates/pdf2md-wasm) | WebAssembly target for 100% client-side, in-browser conversion. |
| [`server`](server) | A minimal Go dev server (`/health`, `/convert`, sandbox UI) that calls the Rust core directly via cgo. |

## Quick start

### CLI (native binary)

```bash
cargo install --path crates/pdf2md-cli
pdf2md document.pdf -o document.md
```

### Go dev server

```bash
cd server
make run
# http://127.0.0.1:8989  — upload sandbox
curl -X POST http://127.0.0.1:8989/convert --data-binary @document.pdf
```

See [`server/README.md`](server/README.md).

### Python wheel (PyO3)

```bash
cd crates/pdf2md-core
maturin develop --release
```

```python
import pdf2md_core
pdf2md_core.convert_pdf_bytes(open("doc.pdf", "rb").read())
```

### In-browser (WASM)

```bash
cd crates/pdf2md-wasm
wasm-pack build --target web --out-dir pkg --release
```

## C ABI (embedding)

`crates/pdf2md-core` exports a stable C ABI declared in
[`include/pdf2md.h`](crates/pdf2md-core/include/pdf2md.h):

```c
char *pdf2md_convert(const uint8_t *bytes, size_t len);  /* -> JSON, free with pdf2md_free_string */
char *pdf2md_convert_ex(const uint8_t *bytes, size_t len, int detect_vectors);
int   pdf2md_is_digital(const uint8_t *bytes, size_t len);
void  pdf2md_free_string(char *ptr);
char *pdf2md_version(void);
```

Build the static and shared libraries:

```bash
cd crates/pdf2md-core
cargo build --release --no-default-features
# -> target/release/libpdf2md_core.a  (static archive for cgo / musl)
# -> target/release/libpdf2md_core.so (shared object)
```

## Modular Engine Architecture

The core engine is structured into clean, modular submodules:
- `layout::ast`: CommonMark/GFM semantic AST representation.
- `layout::xy_cut`: Dynamic recursive XY-Cut++ bounding box segmentation.
- `layout::semantic`: Statistical heading (H1–H6), list, and task classifiers.
- `layout::glyph_stream`: CTM matrix transformation tracking, font advance width lookup.
- `layout::reading_order`: Prose multi-column flow and structured DocBlock generation.
- `layout::tables`: Bordered and borderless 2D spatial table reconstruction with multi-pass ruler scanning.
- `media`: Raster XObject extraction, PNG re-encoding, and vector diagram clipping.

## Scope & Tiered Architecture

`pdf2md-core` handles **digital PDFs** — documents that already contain a text
layer. Scanned / image-only documents, multi-modal Vision LLM rescue (Gemini Flash),
deep OCR layout, and enterprise `/dev/shm` RAM isolation
belong to the commercial hosted microservice tier (`commercial-server`).

## License & Commercial Use

Licensed under the **[Business Source License 1.1 (BSL-1.1)](LICENSE)**.

- **Free for Developers & Local Use:** You are free to use, test, modify, and build local software, academic research, desktop applications, and personal knowledge management tools (such as Obsidian or Logseq plugins).
- **Anti-Competition SaaS Restriction:** You may **NOT** use this software or any derivative works to offer a commercial hosted, managed, or cloud-based document conversion service / API that competes directly with the Licensor.
- **Conversion to Open Source:** On **September 1, 2029**, this license automatically converts to the permissive **Apache License, Version 2.0 OR MIT License**.
- **Commercial SaaS Licensing:** For enterprise cloud exemptions, white-label licenses, or proprietary integration, please contact the author ([@phamhung075](https://github.com/phamhung075)).

### Third-Party Notices

`pdf2md-core` has **zero GPL/AGPL copyleft dependencies**. However, one module
(`crates/pdf2md-core/src/cpdf_textpage.rs`) is a Rust port/derivative of
PDFium's `CPDF_TextPage` analytic algorithms and is therefore distributed under
the **Apache License 2.0** with the **BSD-3-Clause** copyright notice from the
PDFium Authors. The `glyph_data.rs` tables use the Adobe Glyph List.
All required attribution, dependency inventory, and license texts are in
**[NOTICE](NOTICE)** and **[THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES)**.
