# pdf2md CLI

> **Sub-Millisecond Native Command-Line PDF-to-Markdown Tool**  
> 2D Spatial Table Geometry • Zero-Cloud Dependency • Pipe-Friendly Unix Utility

`pdf2md` is the official standalone command-line client built on top of the native Rust `pdf2md-core` engine.

## Installation

```bash
# Build and install locally via Cargo
cargo install --path .

# Or download pre-compiled single binaries from GitHub Releases
curl -sSL https://raw.githubusercontent.com/phamhung075/pdf2md-core/main/install.sh | bash
```

## Quick Start

```bash
# Basic conversion to stdout
pdf2md document.pdf

# Save directly to Markdown file
pdf2md invoice.pdf -o invoice.md

# Pipe via stdin
cat report.pdf | pdf2md - > report.md

# Output JSON structure with table counts, word counts, and latency
pdf2md input.pdf --json
```

## CLI Flags

| Flag | Description |
| :--- | :--- |
| `INPUT_PDF` | Path to PDF document (or `-` for stdin) |
| `-o, --output <FILE>` | Output markdown file destination |
| `--no-tables` | Disables 2D spatial canvas table reconstruction |
| `--json` | Outputs conversion payload and runtime metrics as JSON |
| `-q, --quiet` | Suppresses runtime performance diagnostic logs on stderr |
| `-V, --version` | Prints engine version |

## License

Dual-licensed under MIT or Apache-2.0. Permissive and commercial-friendly.
