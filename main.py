#!/usr/bin/env python3
"""markdown-extract-service — Docling file→Markdown microservice.

Owns one job: deterministic, layout-aware Markdown for PDFs and native office formats.
See README.md for contract and integration details.
"""
from src.interfaces.http.server import run_server

if __name__ == "__main__":
    run_server()
