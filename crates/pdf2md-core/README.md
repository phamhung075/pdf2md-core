# pdf2md-core: High-Performance Native Rust Core Engine

`pdf2md-core` is a compiled, native Rust engine delivering sub-millisecond digital PDF parsing, 2D spatial canvas table reconstruction, multi-column reading order recovery, and zero-allocation text stream extraction.

## Features

- **Sub-Millisecond Execution:** Converts digital PDFs in microseconds to low milliseconds (~24 ms for multi-page complex tickets).
- **2D Spatial Canvas Geometry:** Reconstructs both bordered and borderless tables (such as financial balance sheets and receipts) into formatted GitHub Flavored Markdown (GFM) pipe tables.
- **Multi-Column Reading Order:** Automatically identifies 2-column page structures, detects gutters, suppresses running headers/footers, and produces structured `DocBlock` units in human reading order.
- **Automatic Skew Correction (Hough/Radon):** A super-lightweight, sparse-point Hough line-angle vote (grouped lines) and Radon projection scan (raw span cloud) estimate page tilt from glyph baselines — no rasterization — and deskew the geometry before **XY-Cut segmentation and `build_lines` clustering**, so skewed scans and tilted pages still get clean rows, columns, and horizontal/vertical valley cuts.
- **Visual Media Extraction:** Extracts raster image XObjects with base64 data URIs, handles JPEG passthrough, encodes RGBA PNGs, and clips standalone vector diagrams into cropped PDFs.
- **C ABI Compatible:** Seamlessly embeds into Go (via cgo), Python (via PyO3), C/C++, Node.js, and WebAssembly (`wasm32-unknown-unknown`).
- **Source-Available Licensing:** Licensed under the **Business Source License 1.1 (BSL-1.1)**. Converts to Apache-2.0 / MIT on Sept 1, 2029.

---

## Modular Architecture

The crate is organized into clean, single-responsibility submodules:

```
src/
├── lib.rs                  # Document pipeline coordinator & markdown link formatting
├── models.rs               # BoundingBox, TextSpan, CanvasTable, ConversionOptions, ConversionResult
├── ffi.rs                  # C ABI export functions (pdf2md_convert, pdf2md_is_digital, etc.)
├── cpdf_textpage.rs        # Text word clustering, matrix transformations, coordinate spaces
├── glyph_data.rs           # Unicode glyph mapping tables
├── text_extract.rs         # Multilingual PDF decoder & CMap resolution
├── layout/                 # Layout analysis and reconstruction
│   ├── mod.rs              # ModernLayoutEngine facade & unit test suite
│   ├── ast.rs              # Semantic AST nodes & CommonMark serializer
│   ├── xy_cut.rs           # Dynamic recursive XY-Cut++ & document statistics
│   ├── skew.rs             # Lightweight Hough skew estimation & deskewing
│   ├── semantic.rs         # Heading (H1-H6), list item, and checkbox detectors
│   ├── glyph_stream.rs     # CTM matrix tracking, font advance width lookup
│   ├── reading_order.rs    # Gutter splitting, two-column streams & DocBlock builder
│   └── tables/             # 2D table extraction
│       ├── mod.rs          # Pipe table renderer & public table API
│       ├── bordered.rs     # Line segment intersection & cell grid recovery
│       ├── borderless.rs   # Projection profiles & column alignment
│       ├── consolidation.rs# Multi-line cell merging & complementary columns
│       ├── rulers.rs       # Multi-pass grid scanning & token alignment
│       └── validation.rs   # Table density scoring & false-positive filters
└── media/                  # Media extraction
    ├── mod.rs              # MediaKind, MediaItem & media API
    ├── codecs.rs           # Base64, ASCII85, ASCIIHex, RunLength, PNG encoder
    ├── raster.rs           # XObject image extraction & RGBA decoder
    └── vector.rs           # Vector path clustering & standalone PDF cuts
```

---

## Rust API Usage

```rust
use pdf2md_core::{convert_pdf_bytes_to_markdown, ConversionOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pdf_bytes = std::fs::read("invoice.pdf")?;
    
    let mut options = ConversionOptions::default();
    options.detect_tables = true;   // 2D spatial table reconstruction
    options.detect_layout = true;   // Multi-column & reading order flow
    options.detect_media = true;    // Image and vector figure extraction
    options.embed_media = true;     // Inline base64 image tags in markdown

    let result = convert_pdf_bytes_to_markdown(&pdf_bytes, &options)?;
    
    println!("Pages: {}", result.total_pages);
    println!("Words: {}", result.total_words);
    println!("Tables detected: {}", result.tables_detected);
    println!("Execution time: {} µs", result.duration_us);
    println!("\nMarkdown:\n{}", result.markdown);
    
    Ok(())
}
```

---

## C ABI Exports (`include/pdf2md.h`)

For embedding in foreign languages:

```c
#include <stdint.h>
#include <stddef.h>

// Convert PDF bytes to a heap-allocated JSON string (caller must free with pdf2md_free_string)
char *pdf2md_convert(const uint8_t *pdf_ptr, size_t pdf_len);

// Convert with explicit vector detection flag (0 = off, 1 = on)
char *pdf2md_convert_ex(const uint8_t *pdf_ptr, size_t pdf_len, int detect_vectors);

// Quick digital text layer probe (1 = digital, 0 = scanned/image-only)
int pdf2md_is_digital(const uint8_t *pdf_ptr, size_t pdf_len);

// Frees heap-allocated C string returned by the engine
void pdf2md_free_string(char *ptr);

// Returns engine version string
char *pdf2md_version(void);
```

### Building Static and Shared Libraries

```bash
# Build static library for Go / cgo / musl static builds:
cargo build --release --no-default-features
# Output: target/release/libpdf2md_core.a

# Or build as shared library:
# Output: target/release/libpdf2md_core.so (Linux) or .dylib (macOS) / .dll (Windows)
```

---

## Building Python Wheel (PyO3 / Maturin)

```bash
maturin develop --release
```

```python
import pdf2md_core

pdf_bytes = open("document.pdf", "rb").read()
markdown = pdf2md_core.convert_pdf_bytes(pdf_bytes)
print(markdown)
```

---

## Testing

The crate includes 32 unit and regression tests covering matrix transforms, CMap
decoding, table ruler detection, tagged-PDF structure, and the itinerary-table
regression, plus one structural benchmark:

```bash
cargo test
```

### Golden-Corpus Structural Benchmark

`cargo test` also runs a **labeled structural benchmark** (`tests/structural_benchmark.rs`)
that replaces substring-check assertions ("contains AF7331") with a corpus of
generated PDFs whose ground-truth structure is known by construction. It scores:

- **table cell F1** — exact, whitespace-normalized cell match against the GT grid
- **reading-order accuracy** — pairwise block-order concordance
- **heading exact-match F1** — normalized heading text must match exactly

it enforces aggregate gates (table F1 ≥ 0.97, order acc ≥ 0.90, heading F1 ≥ 0.95)
so structural regressions fail `cargo test`. See
[`tests/README.md`](tests/README.md) for the metric definitions, the corpus, and
the two bugs it caught and that are now fixed (heading-size skew in
`build_doc_blocks`, and two-row tables dropped by `scan_aligned_grids`).
