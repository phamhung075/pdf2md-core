# pdf2md-core

> **Sub-millisecond, native Rust PDF→Markdown engine** — digital text-layer
> extraction with 2D spatial canvas table reconstruction. Dual-licensed MIT /
> Apache-2.0. Zero GPL/AGPL dependencies.

[![License](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)

`pdf2md-core` is a compiled, native engine that converts digital (text-layer)
PDFs into clean GitHub Flavored Markdown (GFM) — including pipe tables
reconstructed from 2D span geometry — in microseconds. It is the fast,
zero-cloud-cost foundation for RAG ingestion pipelines, CLI tooling, and
in-browser document processing.

## What's inside

| Component | Description |
| :--- | :--- |
| [`crates/pdf2md-core`](crates/pdf2md-core) | The native Rust engine: byte-level parsing, digital-PDF triage, 2D canvas table reconstruction. Ships as a Rust library, a Python wheel (PyO3/Maturin), and a C ABI (`libpdf2md_core`) for Go/cgo and other languages. |
| [`crates/pdf2md-cli`](crates/pdf2md-cli) | `pdf2md` — a pipe-friendly, native command-line tool built on the core. |
| [`crates/pdf2md-wasm`](crates/pdf2md-wasm) | WebAssembly target for 100% client-side, in-browser conversion. |
| [`server`](server) | A minimal Go dev server (`/health`, `/convert`, sandbox UI) that calls the Rust core directly via cgo. |

## Quick start

### CLI (native binary)

```bash
cargo install --path crates/pdf2md-cli
pdf2md document.pdf -o document.md
```

### Go mini dev server

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
int   pdf2md_is_digital(const uint8_t *bytes, size_t len);
void  pdf2md_free_string(char *ptr);
char *pdf2md_version(void);
```

Build the shared library without Python bindings:

```bash
cd crates/pdf2md-core
cargo build --release --no-default-features
# -> target/release/libpdf2md_core.so
```

## Scope

`pdf2md-core` handles **digital PDFs** — documents that already contain a text
layer. Scanned / image-only documents (OCR, vision LLM rescue) and heavy ML
layout models are intentionally out of scope here and belong to the hosted
service tier.

## License

Licensed under either of [MIT](LICENSE) or Apache License 2.0, at your option.
