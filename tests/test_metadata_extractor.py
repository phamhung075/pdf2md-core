"""Unit tests for pure domain metadata extractor and YAML frontmatter engine."""
import unittest

from src.domain.metadata_extractor import (
    count_markdown_tables,
    detect_language,
    extract_document_title,
    extract_metadata,
    parse_existing_frontmatter,
    prepend_yaml_frontmatter,
    render_yaml_frontmatter,
)


class TestMetadataExtractor(unittest.TestCase):
    def test_count_markdown_tables(self):
        # 1 table
        md1 = """
# Report
Here is a table:

| Name | Role | Location |
| :--- | :--- | :--- |
| Alice | Admin | Paris |
| Bob | User | Tokyo |

End of table.
"""
        self.assertEqual(count_markdown_tables(md1), 1)

        # 2 tables
        md2 = md1 + """
Another table:

| Qty | Item | Price |
| --- | --- | --- |
| 1 | Widget | $10 |
"""
        self.assertEqual(count_markdown_tables(md2), 2)

        # 0 tables
        self.assertEqual(count_markdown_tables("Just regular paragraph text."), 0)

    def test_detect_language(self):
        english_text = "This is a comprehensive report on document parsing and text extraction with high performance."
        french_text = "Le rapport présente les résultats de l'extraction de texte et de tableaux dans un document."
        vietnamese_text = "Tài liệu này trình bày các phương pháp trích xuất văn bản và bảng biểu cho người dùng."

        self.assertEqual(detect_language(english_text), "en")
        self.assertEqual(detect_language(french_text), "fr")
        self.assertEqual(detect_language(vietnamese_text), "vi")

    def test_extract_document_title(self):
        md_h1 = "# Strategic Architecture Review 2026\n\nBody content..."
        self.assertEqual(extract_document_title(md_h1), "Strategic Architecture Review 2026")

        md_h2 = "## Executive Summary\n\nContent..."
        self.assertEqual(extract_document_title(md_h2), "Executive Summary")

        md_clean = "Invoice Statement\n2026-09-01\n..."
        self.assertEqual(extract_document_title(md_clean), "Invoice Statement")

        md_empty = ""
        self.assertEqual(extract_document_title(md_empty, "quarterly_financial_report.pdf"), "Quarterly Financial Report")

    def test_parse_existing_frontmatter(self):
        raw = """---
title: "Prior Document"
pages: 12
author: Admin
---

# Real Content
Paragraph text.
"""
        meta, body = parse_existing_frontmatter(raw)
        self.assertEqual(meta["title"], "Prior Document")
        self.assertEqual(meta["pages"], 12)
        self.assertEqual(meta["author"], "Admin")
        self.assertTrue(body.startswith("# Real Content"))

    def test_prepend_yaml_frontmatter(self):
        doc = """# Technical Proposal

This document outlines the architecture for cloud document parsing.

| Component | Stack |
| :--- | :--- |
| Core | Rust |
| Gateway | FastAPI |
"""
        enriched = prepend_yaml_frontmatter(doc, filename="proposal_2026.pdf", page_count=4)
        self.assertTrue(enriched.startswith("---"))
        self.assertIn("title: Technical Proposal", enriched)
        self.assertIn("pages: 4", enriched)
        self.assertIn("tables: 1", enriched)
        self.assertIn("generator: pdf2md-core", enriched)
        self.assertIn("| Component | Stack |", enriched)

    def test_non_destructive_frontmatter_merge(self):
        doc_with_meta = """---
custom_id: 12345
confidential: true
---

# Preserved Heading
Body text.
"""
        enriched = prepend_yaml_frontmatter(doc_with_meta, filename="doc.pdf")
        self.assertIn("custom_id: 12345", enriched)
        self.assertIn("confidential: true", enriched)
        self.assertIn("title: Preserved Heading", enriched)
        self.assertIn("Body text.", enriched)


if __name__ == "__main__":
    unittest.main()
