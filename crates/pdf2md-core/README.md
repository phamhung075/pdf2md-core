# pdf2md-core: Native Rust Engine for Sub-Millisecond PDF to Markdown

High-performance native Rust core engine delivering sub-millisecond digital PDF parsing, 2D spatial canvas table reconstruction, and zero-allocation text stream extraction.

## Highlights
- **Sub-Millisecond Speed:** Up to 200x faster than pure Python libraries.
- **2D Spatial Canvas Geometry:** Reconstructs borderless and fused table grids into pristine GitHub Flavored Markdown (GFM) pipe tables.
- **Permissive Dual Licensing:** Dual MIT / Apache-2.0. Zero GPL or AGPL copyleft viral risks.
- **Multi-Target:**
  - Native Python wheel via PyO3 / Maturin (`pdf2md_core`).
  - WebAssembly target (`wasm32-unknown-unknown`) for client-side, zero-cloud-cost in-browser document processing.

## Building with Maturin
```bash
cd crates/pdf2md-core
maturin develop --release
```

## Rust Usage
```rust
use pdf2md_core::{convert_pdf_bytes_to_markdown, ConversionOptions};

let pdf_bytes = std::fs::read("document.pdf")?;
let options = ConversionOptions::default();
let result = convert_pdf_bytes_to_markdown(&pdf_bytes, &options)?;
println!("{}", result.markdown);
```
