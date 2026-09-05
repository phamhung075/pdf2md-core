//! pdf2md CLI — High-performance native command-line PDF-to-Markdown extractor.
//!
//! Sub-millisecond text extraction and 2D spatial canvas table reconstruction.

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process;
use clap::Parser;

use pdf2md_core::{convert_pdf_bytes_to_markdown, is_digital_pdf_bytes, ConversionOptions};

#[derive(Parser, Debug)]
#[command(
    name = "pdf2md",
    author = "PDF2MD Core Team",
    version = env!("CARGO_PKG_VERSION"),
    about = "Sub-millisecond native PDF-to-Markdown extraction with 2D spatial table reconstruction",
    long_about = "pdf2md is a high-speed, dual-licensed (MIT/Apache-2.0) native CLI tool for extracting clean GitHub Flavored Markdown (GFM) and tables from digital PDFs with zero cloud cost."
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
        eprintln!("Error: Document lacks a readable digital text layer or is a scanned image.");
        eprintln!("Tip: Route through the full Docling/Vision OCR pipeline for optical character recognition.");
        process::exit(2);
    }

    let options = ConversionOptions {
        detect_tables: !args.no_tables,
        ..Default::default()
    };

    match convert_pdf_bytes_to_markdown(&bytes, &options) {
        Ok(res) => {
            if !args.quiet {
                eprintln!(
                    "Converted {} pages ({} words) in {:.2} ms",
                    res.total_pages,
                    res.total_words,
                    res.duration_us as f64 / 1000.0
                );
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
        }
        Err(e) => {
            eprintln!("Conversion error: {}", e);
            process::exit(3);
        }
    }
}
