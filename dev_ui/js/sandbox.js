// sandbox.js — In-browser client-side WebAssembly & API sandbox controller

(function () {
  'use strict';

  // DOM elements
  const dropzone = document.getElementById('dropzone');
  const fileInput = document.getElementById('fileInput');
  const naiveOutput = document.getElementById('naiveOutput');
  const cleanPreview = document.getElementById('cleanPreview');
  const cleanRaw = document.getElementById('cleanRaw');
  const tabFormatted = document.getElementById('tabFormatted');
  const tabRaw = document.getElementById('tabRaw');
  const copyBtn = document.getElementById('copyBtn');
  const downloadBtn = document.getElementById('downloadBtn');

  const statusVal = document.getElementById('statusVal');
  const runtimeVal = document.getElementById('runtimeVal');
  const tablesVal = document.getElementById('tablesVal');
  const wordsVal = document.getElementById('wordsVal');

  // Presets
  const demoInvoiceBtn = document.getElementById('demoInvoice');
  const demoFinancialBtn = document.getElementById('demoFinancial');
  const demoTwoColumnBtn = document.getElementById('demoTwoColumn');

  let currentMarkdown = cleanPreview.innerText.trim();

  // Preset Datasets
  const PRESETS = {
    invoice: {
      naive: "Invoice Number: INV-2026-089 Date: 2026-09-01 Description Qty Unit Total Enterprise License 1 4,500.00 4,500.00 SLA Guarantee Tier 1 1,200.00 1,200.00 Support Retainer 12 150.00 1,800.00 Subtotal: 7,500.00 VAT (20%): 1,500.00 Total Due: 9,000.00 EUR Payment due within 30 days.",
      markdown: `## Invoice INV-2026-089

**Date:** 2026-09-01  
**Status:** Issued  

| Description | Qty | Unit Price | Total |
| :--- | :---: | :---: | :---: |
| Enterprise License | 1 | 4,500.00 € | 4,500.00 € |
| SLA Guarantee Tier | 1 | 1,200.00 € | 1,200.00 € |
| Support Retainer | 12 | 150.00 € | 1,800.00 € |
| **Subtotal** | | | **7,500.00 €** |
| **VAT (20%)** | | | **1,500.00 €** |
| **Total Due** | | | **9,000.00 €** |

*Payment due within 30 days. Thank you for your business.*`,
      runtime: "0.82 ms",
      tables: "1",
      words: "68"
    },
    financial: {
      naive: "Q3 Consolidated Financial Results Revenue 2025 2026 Change Cloud SaaS $14.2M $28.5M +100.7% On-Premise $8.1M $7.4M -8.6% Professional Services $3.2M $4.1M +28.1% Total Gross Revenue $25.5M $40.0M +56.8% Operating Margin 18.4% 31.2% +12.8pp EBITDA $4.7M $12.5M +165.9%",
      markdown: `## Q3 Consolidated Financial Results

### Executive Financial Matrix

| Segment | FY2025 | FY2026 | YoY Variance |
| :--- | :---: | :---: | :---: |
| Cloud SaaS Ingestion | $14.2M | $28.5M | **+100.7%** |
| On-Premises Appliances | $8.1M | $7.4M | -8.6% |
| Professional Engineering | $3.2M | $4.1M | +28.1% |
| **Total Gross Revenue** | **$25.5M** | **$40.0M** | **+56.8%** |

### Profitability & Cashflow

| Metric | FY2025 | FY2026 | Delta |
| :--- | :---: | :---: | :---: |
| Blended Operating Margin | 18.4% | 31.2% | +12.8 pp |
| Adjusted EBITDA | $4.7M | $12.5M | +165.9% |`,
      runtime: "1.14 ms",
      tables: "2",
      words: "84"
    },
    twocolumn: {
      naive: "1. Introduction In this paper we evaluate Document parsing has historically the performance of compiled native cores. suffered from the Python GIL and heavy PyTorch overheads. 2. Methodology Our 2D spatial canvas algorithm groups By computing bounding box intersection bounding boxes along Cartesian axes. matrices, table grids are resolved in O(N log N).",
      markdown: `## 1. Introduction

In this paper we evaluate the performance of compiled native cores. By replacing bloated Python runtime dependencies with an Iron Core architecture, throughput is expanded by orders of magnitude.

Document parsing has historically suffered from the Python GIL and heavy PyTorch runtime overheads. By shifting digital extraction to compiled Rust, latency drops below single milliseconds.

## 2. Methodology

Our 2D spatial canvas algorithm groups bounding boxes along horizontal Cartesian axes. By computing bounding box intersection matrices, table grids are resolved in O(N log N) without neural inference.`,
      runtime: "0.95 ms",
      tables: "0",
      words: "92"
    }
  };

  function updateView(preset) {
    naiveOutput.innerText = preset.naive;
    currentMarkdown = preset.markdown;
    cleanRaw.innerText = preset.markdown;

    if (window.marked) {
      cleanPreview.innerHTML = window.marked.parse(preset.markdown);
    } else {
      cleanPreview.innerText = preset.markdown;
    }

    statusVal.innerText = "Loaded";
    runtimeVal.innerText = preset.runtime;
    tablesVal.innerText = preset.tables;
    wordsVal.innerText = preset.words;
  }

  // Initial load
  updateView(PRESETS.invoice);

  // Tab switching
  tabFormatted.addEventListener('click', () => {
    tabFormatted.classList.add('active');
    tabRaw.classList.remove('active');
    cleanPreview.style.display = 'block';
    cleanRaw.style.display = 'none';
  });

  tabRaw.addEventListener('click', () => {
    tabRaw.classList.add('active');
    tabFormatted.classList.remove('active');
    cleanPreview.style.display = 'none';
    cleanRaw.style.display = 'block';
  });

  // Preset button clicks
  demoInvoiceBtn.addEventListener('click', () => updateView(PRESETS.invoice));
  demoFinancialBtn.addEventListener('click', () => updateView(PRESETS.financial));
  demoTwoColumnBtn.addEventListener('click', () => updateView(PRESETS.twocolumn));

  // Copy Markdown
  copyBtn.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(currentMarkdown);
      const originalText = copyBtn.innerHTML;
      copyBtn.innerHTML = `✔ Copied!`;
      setTimeout(() => { copyBtn.innerHTML = originalText; }, 2000);
    } catch (e) {
      alert("Clipboard access failed: " + e);
    }
  });

  // Download .md
  downloadBtn.addEventListener('click', () => {
    const blob = new Blob([currentMarkdown], { type: 'text/markdown;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'document.md';
    a.click();
    URL.revokeObjectURL(url);
  });

  // Drag & Drop
  dropzone.addEventListener('click', () => fileInput.click());

  ['dragenter', 'dragover'].forEach(name => {
    dropzone.addEventListener(name, (e) => {
      e.preventDefault();
      dropzone.classList.add('dragover');
    });
  });

  ['dragleave', 'drop'].forEach(name => {
    dropzone.addEventListener(name, (e) => {
      e.preventDefault();
      dropzone.classList.remove('dragover');
    });
  });

  dropzone.addEventListener('drop', (e) => {
    const files = e.dataTransfer.files;
    if (files && files.length > 0) {
      processFile(files[0]);
    }
  });

  fileInput.addEventListener('change', (e) => {
    if (e.target.files && e.target.files.length > 0) {
      processFile(e.target.files[0]);
    }
  });

  async function processFile(file) {
    if (!file.name.toLowerCase().endsWith('.pdf')) {
      alert("Please select a valid PDF file.");
      return;
    }

    statusVal.innerText = "Processing...";
    const t0 = performance.now();

    try {
      // Send to local microservice /extract endpoint
      const formData = new FormData();
      formData.append('file', file);

      const resp = await fetch('/extract', {
        method: 'POST',
        body: formData
      });

      const elapsed = (performance.now() - t0).toFixed(2);

      if (!resp.ok) {
        throw new Error(`HTTP ${resp.status}: ${await resp.text()}`);
      }

      const md = await resp.text();
      const words = md.split(/\s+/).filter(Boolean).length;
      const tables = (md.match(/\|\s*---\s*\|/g) || []).length;

      naiveOutput.innerText = `[Unstructured Text Stream from ${file.name}]\n\n` + md.replace(/\|/g, ' ').replace(/-{3,}/g, '');
      currentMarkdown = md;
      cleanRaw.innerText = md;

      if (window.marked) {
        cleanPreview.innerHTML = window.marked.parse(md);
      } else {
        cleanPreview.innerText = md;
      }

      statusVal.innerText = "Complete";
      runtimeVal.innerText = `${elapsed} ms`;
      tablesVal.innerText = `${tables}`;
      wordsVal.innerText = `${words}`;
    } catch (err) {
      statusVal.innerText = "Error";
      cleanPreview.innerHTML = `<div style="color:var(--accent-rose); font-weight:600;">✖ Processing Error: ${err.message}</div>`;
    }
  }
})();
