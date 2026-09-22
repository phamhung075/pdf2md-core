// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! pdf2md CLI — High-performance native command-line PDF-to-Markdown extractor.
//!
//! Sub-millisecond text extraction and 2D spatial canvas table reconstruction.

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process;
use clap::Parser;

use pdf2md_core::{
    convert_pdf_bytes_to_markdown, is_digital_pdf_bytes, pdf_password_required, ConversionOptions,
    MediaMode,
};

/// CLI spelling of [`MediaMode`] so clap can parse it without a core → clap
/// dependency.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum MediaModeArg {
    None,
    Reference,
    Embed,
}

impl From<MediaModeArg> for MediaMode {
    fn from(m: MediaModeArg) -> Self {
        match m {
            MediaModeArg::None => MediaMode::None,
            MediaModeArg::Reference => MediaMode::Reference,
            MediaModeArg::Embed => MediaMode::Embed,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "pdf2md",
    author = "PDF2MD Core Team",
    version = env!("CARGO_PKG_VERSION"),
    about = "Sub-millisecond native PDF-to-Markdown extraction with 2D spatial table reconstruction",
    long_about = "pdf2md is a high-speed native CLI tool licensed under BSL-1.1 for extracting clean GitHub Flavored Markdown (GFM) and tables from digital PDFs with zero cloud cost."
)]
struct Args {
    /// Input PDF document path (use '-' to read from standard input)
    #[arg(value_name = "INPUT_PDF")]
    input: PathBuf,

    /// Output markdown file path (defaults to standard output)
    #[arg(short, long, value_name = "OUTPUT_MD")]
    output: Option<PathBuf>,

    /// Disable 2D spatial canvas table reconstruction
    #[arg(long, default_value_t = false)]
    no_tables: bool,

    /// Output full conversion metadata and statistics as JSON
    #[arg(long, default_value_t = false)]
    json: bool,

    /// Suppress diagnostic and performance logs on stderr
    #[arg(short, long, default_value_t = false)]
    quiet: bool,

    /// Detect pure-vector figure regions and cut them as clipped PDFs
    #[arg(long, default_value_t = false)]
    vectors: bool,

    /// Media handling: `none` (default; smallest output), `reference`
    /// (extract into the JSON `media` side-channel only) or `embed` (inline
    /// base64 `data:` URIs in the markdown)
    #[arg(long, value_enum, default_value_t = MediaModeArg::None)]
    media_mode: MediaModeArg,

    /// Inline extracted images as base64 `data:` URIs in the markdown
    /// (shorthand for --media-mode embed; opt-in, large output)
    #[arg(long, default_value_t = false)]
    embed_media: bool,

    /// Alias for --media-mode none (the default); kept for compatibility
    #[arg(long, default_value_t = false)]
    no_media: bool,

    /// Alias for --no-media / --media-mode none
    #[arg(long, default_value_t = false)]
    no_images: bool,
}

fn main() {
    let args = Args::parse();

    // Read bytes from file or stdin
    let bytes: Vec<u8> = if args.input.to_str() == Some("-") {
        let mut buffer = Vec::new();
        if let Err(e) = io::stdin().read_to_end(&mut buffer) {
            eprintln!("Error reading from stdin: {}", e);
            process::exit(1);
        }
        buffer
    } else {
        match fs::read(&args.input) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Error reading file '{}': {}", args.input.display(), e);
                process::exit(1);
            }
        }
    };

    if !is_digital_pdf_bytes(&bytes) {
        // An encrypted PDF is not a scanned image: surface the library's
        // distinct encryption error (the same text the conversion returns)
        // instead of the generic "no digital text layer" hint, which would
        // wrongly suggest an OCR rescue that cannot read the file either.
        if pdf_password_required(&bytes) {
            eprintln!("Conversion error: {}", pdf2md_core::ENCRYPTED_PDF_ERROR);
            process::exit(3);
        }
        eprintln!("Error: Document lacks a readable digital text layer or is a scanned image.");
        eprintln!("Tip: this CLI only runs the fast digital-text path; route the document through a vision/OCR rescue service for scanned pages.");
        process::exit(2);
    }

    // Precedence: --no-media/--no-images force `none` (the default; kept as
    // explicit aliases); otherwise --embed-media opts into embedding;
    // otherwise --media-mode (default none) applies.
    let mut media_mode = MediaMode::from(args.media_mode);
    if args.embed_media {
        media_mode = MediaMode::Embed;
    }
    if args.no_media || args.no_images {
        media_mode = MediaMode::None;
    }
    let options = ConversionOptions {
        detect_tables: !args.no_tables,
        detect_vectors: args.vectors,
        media_mode,
        ..Default::default()
    };

    match convert_pdf_bytes_to_markdown(&bytes, &options) {
        Ok(res) => {
            let needs_rescue = res.needs_vision_rescue;
            if !args.quiet {
                eprintln!(
                    "Converted {} pages ({} words) in {:.2} ms",
                    res.total_pages,
                    res.total_words,
                    res.duration_us as f64 / 1000.0
                );
                if needs_rescue {
                    eprintln!(
                        "Warning: text layer is glyph-encoded; emitted a status document instead of empty output. Route through the OCR/Vision pipeline."
                    );
                }
            }

            let output_text = if args.json {
                serde_json::to_string_pretty(&res).unwrap_or_else(|_| res.markdown.clone())
            } else {
                res.markdown
            };

            if let Some(out_path) = args.output {
                if let Err(e) = fs::write(&out_path, output_text) {
                    eprintln!("Error writing output to '{}': {}", out_path.display(), e);
                    process::exit(1);
                }
            } else {
                let stdout = io::stdout();
                let mut handle = stdout.lock();
                if let Err(e) = handle.write_all(output_text.as_bytes()) {
                    eprintln!("Error writing to stdout: {}", e);
                    process::exit(1);
                }
            }

            // The glyph-encoded case now returns a well-formed status document
            // rather than a 0-byte error, but it is still a failure to decode:
            // keep the historical non-zero exit so callers that branch on the
            // status keep routing the document to vision rescue.
            if needs_rescue {
                process::exit(3);
            }
        }
        Err(e) => {
            eprintln!("Conversion error: {}", e);
            process::exit(3);
        }
    }
}
