#!/usr/bin/env python3
"""test_service.py — headless test/debug client for markdown-extract-service.

POSTs one or more files to /extract (or /to-markdown) and prints a detailed, step-ordered report
per file, designed for agents and CI as much as for humans:

  1. transport      file → HTTP status/timing
  2. routing        extension → engine (docling-pdf | docling-native | pymupdf4llm)
  3. conversion     markdown/text char counts, numpages, server + client duration, checksum
  4. structure      headings, GFM table blocks (cells/ragged rows), code fences, images/links,
                    list items, paragraphs — cheap syntactic map of what the Markdown actually is
  5. quality        Python mirror of the pdf-triage gate heuristics (assessDoclingMarkdown):
                    no-text/image-placeholder, wrong-script mojibake, majority-ragged tables,
                    content-recall floor vs the returned text. WARN-only unless --require-gate.

Exit codes: 0 = all files converted & checks passed · 1 = transport/HTTP/conversion error ·
2 = at least one quality check FAILED (only with --require-gate) · 3 = usage error.

Usage:
  python3 test_service.py <file> [more files...] [options]
  python3 test_service.py --health                       # probe /health only
Options:
  -u, --url URL        service base URL (default http://127.0.0.1:3984)
  --to-markdown        use the /to-markdown alias instead of /extract
  -t, --timeout SEC    per-request timeout (default 600 — docling OCR of scans takes minutes)
  --json               machine-readable: one JSON object on stdout, nothing else
  --out DIR            save per-file artifacts: <name>.markdown.md / .text.txt / .response.json
  -v, --verbose        extra detail (per-block table layout, all checks incl. PASS detail)
  --require-gate       exit 2 when any quality check FAILS (default: report only)
  --no-gate            skip the quality heuristics entirely
"""

import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

# --- Engine / routing hints -------------------------------------------------

ENGINE_LABELS = {
    "docling-pdf": "Docling PDF pipeline (Heron layout + TableFormer + RapidOCR PP-OCRv6 fr)",
    "docling-image": "Docling Image pipeline (Heron layout + TableFormer + RapidOCR PP-OCRv6 fr)",
    "docling-native": "Docling native readers (office formats; file's own structure, no OCR)",
    "pdf-oxide-fast-path": "PDF Oxide fast path (Rust-compiled core; opt-in DOCLING_PDF_FAST_PATH)",
    "pypdf-fast-path": "pypdf fast path (digital text-layer PDF; secondary fallback)",
    "vision:gemini-flash-latest": "Vision LLM rescue (Gemini Flash Latest via Vision API)",
    "vision:gemini-3.8-flash": "Vision LLM rescue (Gemini 3.8 Flash via Vision API)",
    "vision:gemini-1.5-flash": "Vision LLM rescue (Gemini 1.5 Flash via Vision API)",
}
EXT_ROUTING = {
    ".pdf": "pdf → docling-pdf (or pdf-oxide fast path, with Vision LLM fallback if gate fails)",
    ".jpg": "image → docling-image (with Vision LLM fallback if gate fails)",
    ".jpeg": "image → docling-image (with Vision LLM fallback if gate fails)",
    ".png": "image → docling-image (with Vision LLM fallback if gate fails)",
    ".webp": "image → docling-image (with Vision LLM fallback if gate fails)",
    ".bmp": "image → docling-image (with Vision LLM fallback if gate fails)",
    ".tiff": "image → docling-image (with Vision LLM fallback if gate fails)",
    ".tif": "image → docling-image (with Vision LLM fallback if gate fails)",
}

# --- Cheap quality mirrors of pdf-triage's docling-quality gate --------------
# (heuristics only — the authoritative gate is src/domain/docling-quality.ts app-side)

MIN_TEXT_CHARS = 10
MOJIBAKE_MIN_LETTER_TOKENS = 50
MOJIBAKE_NON_LATIN_SHARE = 0.80
TABLE_MIN_DATA_ROWS = 6
TABLE_MAX_RAGGED_RATIO = 0.50
RECALL_FLOOR = 0.55

LETTER_RE = re.compile(r"[A-Za-zÀ-ÿ]+")
CONTENT_TOKEN_RE = re.compile(r"[a-zà-ÿ]{6,}|\d[\d.,]{2,}")
IMAGE_PLACEHOLDER_RE = re.compile(r"<!--\s*image.*?-->", re.IGNORECASE | re.DOTALL)


def _content_tokens(text: str):
    return [m.group(0) for m in CONTENT_TOKEN_RE.finditer(text.lower())]


def count_cells(row: str) -> int:
    return len(row.strip().strip("|").split("|"))


def is_separator_row(row: str) -> bool:
    cells = row.strip().strip("|").split("|")
    return bool(cells) and all(re.fullmatch(r":?-{2,}:?", c.strip()) for c in cells)


def analyze_tables(markdown: str):
    """Scans contiguous pipe-row blocks; returns summary + per-block detail."""
    lines = markdown.splitlines()
    blocks, current, starts = [], [], []
    for i, line in enumerate(lines):
        if line.strip().startswith("|"):
            if not current:
                starts.append(i)
            current.append(line)
        elif current:
            blocks.append((starts.pop(0), current))
            current = []
    if current:
        blocks.append((starts.pop(0), current))

    summary = {"blocks": len(blocks), "dataRows": 0, "raggedRows": 0, "raggedBlocks": []}
    detail = []
    for start, rows in blocks:
        data = [r for r in rows if not is_separator_row(r)]
        if not data:
            continue
        header_cells = count_cells(data[0])
        body = data[1:] if len(data) > 1 and is_separator_row(rows[1] if len(rows) > 1 else "") else data
        if is_separator_row(rows[0]):
            body = data
        ragged = [r for r in body if count_cells(r) != header_cells]
        ratio = len(ragged) / len(body) if body else 0.0
        summary["dataRows"] += len(body)
        summary["raggedRows"] += len(ragged)
        if header_cells >= 2 and len(body) >= TABLE_MIN_DATA_ROWS and ratio > TABLE_MAX_RAGGED_RATIO:
            summary["raggedBlocks"].append({"startLine": start, "headerCells": header_cells,
                                            "dataRows": len(body), "raggedRatio": round(ratio, 3)})
        detail.append({"startLine": start, "headerCells": header_cells, "dataRows": len(body),
                       "raggedRows": len(ragged), "raggedRatio": round(ratio, 3)})
    return summary, detail


def analyze_structure(markdown: str):
    headings = {"h1": 0, "h2": 0, "h3": 0, "h4+": 0}
    fences = 0
    images = len(re.findall(r"!\[[^\]]*\]\([^)]+\)", markdown))
    links = len(re.findall(r"(?<!!)\[[^\]]*\]\([^)]+\)", markdown))
    bullets = hr = paragraphs = 0
    in_fence = False
    table_block = False
    for line in markdown.splitlines():
        s = line.strip()
        if s.startswith("```") or s.startswith("~~~"):
            fences += 1
            in_fence = not in_fence
            continue
        if in_fence or s.startswith("|") or not s:
            continue
        m = re.match(r"^(#{1,6})\s", line)
        if m:
            lvl = len(m.group(1))
            headings["h1" if lvl == 1 else "h2" if lvl == 2 else "h3" if lvl == 3 else "h4+"] += 1
        elif re.match(r"^\s*([-*_])\s*([-*_]\s*){2,}$", s):
            hr += 1
        elif re.match(r"^\s*(?:[-*+]|\d+[.)])\s+", s):
            bullets += 1
        else:
            paragraphs += 1
    return {
        **headings, "codeFences": fences // 2, "images": images, "links": links,
        "listItems": bullets, "horizontalRules": hr, "paragraphs": paragraphs,
    }, {"fenceBalanceOk": fences % 2 == 0}


def quality_checks(markdown: str, text: str):
    """Mirror of the app-side gate heuristics → list of {id, pass, detail}."""
    checks = []

    stripped = IMAGE_PLACEHOLDER_RE.sub("", markdown or "")
    visible = re.sub(r"\s", "", stripped)
    checks.append({
        "id": "no-text-or-image-placeholder",
        "pass": len(visible) >= MIN_TEXT_CHARS,
        "detail": f"{len(visible)} visible non-space chars after stripping HTML comments "
                  f"(floor {MIN_TEXT_CHARS}) — a whole-page-picture PDF leaves only '<!-- image -->'",
    })

    letters = LETTER_RE.findall(markdown or "")
    latin = sum(1 for w in letters if re.fullmatch(r"[A-Za-zÀ-ÿ]+", w))
    non_latin = len(letters) - latin
    share = non_latin / len(letters) if letters else 0.0
    checks.append({
        "id": "mojibake",
        "pass": not (len(letters) >= MOJIBAKE_MIN_LETTER_TOKENS and share >= MOJIBAKE_NON_LATIN_SHARE),
        "detail": f"{non_latin}/{len(letters)} non-Latin letter tokens (share {share:.2f}; flag "
                  f"≥{MOJIBAKE_MIN_LETTER_TOKENS} tokens at ≥{MOJIBAKE_NON_LATIN_SHARE}) — the "
                  f"broken-ToUnicode/CMap decode class decodes to Hangul",
    })

    table_summary, _ = analyze_tables(markdown)
    checks.append({
        "id": "ragged-tables",
        "pass": not table_summary["raggedBlocks"],
        "detail": f"{table_summary['blocks']} table block(s), {table_summary['dataRows']} data "
                  f"rows, {table_summary['raggedRows']} ragged rows; majority-ragged blocks with "
                  f"≥{TABLE_MIN_DATA_ROWS} rows: {len(table_summary['raggedBlocks'])}",
    })

    raw = sorted(set(_content_tokens(text or "")))
    md_tokens = set(_content_tokens(markdown or ""))
    missing = [t for t in raw
               if t not in md_tokens and not (len(t) >= 15 or t.count(",") >= 2)]
    measurable = len(raw) >= 40
    recall = (len(raw) - len(missing)) / len(raw) if raw else 1.0
    checks.append({
        "id": "content-recall",
        "pass": (not measurable) or recall >= RECALL_FLOOR,
        "detail": f"{recall:.3f} recall vs returned text ({len(raw)} distinct content tokens, "
                  f"{len(missing)} missing, measurable={measurable}; floor {RECALL_FLOOR} — "
                  f"unmeasurable below 40 tokens)",
    })
    return checks


# --- HTTP -------------------------------------------------------------------

def http_post_json(url: str, data: bytes, filename: str, timeout: int, extra_headers: dict = None):
    headers = {
        "content-type": "application/octet-stream",
        "x-file-name": urllib.parse.quote(filename),
    }
    if extra_headers:
        headers.update(extra_headers)
    req = urllib.request.Request(url, data=data, method="POST", headers=headers)
    t0 = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as res:
            total_ms = round((time.monotonic() - t0) * 1000)
            raw = res.read()
            return {"status": res.status, "totalMs": total_ms, "json": json.loads(raw),
                    "raw": raw}
    except urllib.error.HTTPError as e:
        total_ms = round((time.monotonic() - t0) * 1000)
        body = e.read().decode("utf-8", "replace")
        try:
            payload = json.loads(body)
        except ValueError:
            payload = {"error": body}
        return {"status": e.code, "totalMs": total_ms, "json": payload, "raw": body}
    except urllib.error.URLError as e:
        return {"transportError": str(e.reason)}


def http_get_json(url: str, timeout: int):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as res:
            return {"status": res.status, "json": json.loads(res.read())}
    except Exception as e:  # noqa: BLE001
        return {"transportError": str(e)}


# --- Per-file report --------------------------------------------------------

def test_file(path: str, cfg: dict, verbose: bool):
    base = cfg["url"].rstrip("/") + cfg["endpoint"]
    name = os.path.basename(path)
    ext = os.path.splitext(name)[1].lower()
    with open(path, "rb") as f:
        data = f.read()

    steps = [("info", f"Step 1 — file: {name} ({len(data):,} bytes, extension '{ext}')")]
    extra_headers = {}
    if cfg.get("force_vision"):
        extra_headers["x-force-vision"] = "1"
        steps.append(("info", "Step 1 — flag: force Vision LLM (X-Force-Vision: 1)"))
    if cfg.get("no_vision_fallback"):
        extra_headers["x-vision-fallback"] = "0"
        steps.append(("info", "Step 1 — flag: disable Vision fallback (X-Vision-Fallback: 0)"))
    steps.append(("info", f"Step 1 — POST {base}  (X-File-Name: {urllib.parse.quote(name)})"))
    resp = http_post_json(base, data, name, cfg["timeout"], extra_headers=extra_headers)
    if "transportError" in resp:
        steps.append(("err", f"Step 2 — transport error: {resp['transportError']} — is the "
                             f"service up? (curl {cfg['url'].rstrip('/')}/health)"))
        return {"ok": False, "steps": steps, "exit": 1, "body": resp}

    body = resp["json"]
    status = resp["status"]
    steps.append(("ok" if status == 200 else "err",
                  f"Step 2 — HTTP {status} in {resp['totalMs']} ms client-side"))

    record = {"file": name, "sizeBytes": len(data), "extension": ext, "url": base,
              "httpStatus": status, "ok": status == 200, "totalMs": resp["totalMs"],
              "body": body if status != 200 else None}
    if status != 200:
        steps.append(("err", f"Step 3 — service error: {json.dumps(body, ensure_ascii=False)}"))
        return {"ok": False, "steps": steps, "exit": 1, "record": record}

    engine = body.get("engine")
    markdown = body.get("markdown", "")
    text = body.get("text", body.get("raw_text", ""))
    steps.append(("ok", f"Step 3 — routing: {EXT_ROUTING.get(ext, '') or ext} → engine "
                        f"'{engine}' — {ENGINE_LABELS.get(engine, 'unknown engine')}"))

    structure, extra = analyze_structure(markdown)
    steps.append(("ok", f"Step 4 — converted: markdown {len(markdown):,} chars · text "
                        f"{len(text):,} chars · numpages {body.get('numpages')} · server "
                        f"{body.get('duration_ms', '?')} ms"))
    if engine == "docling-native" and body.get("numpages") == 0:
        steps.append(("warn", "note — numpages is 0 for native (office) conversions; expected"))
    if engine == "pymupdf4llm":
        steps.append(("warn", "note — fast path: text-layer probe passed; model-free engine"))
    if not extra["fenceBalanceOk"]:
        steps.append(("warn", "note — unbalanced code fences in markdown (odd count)"))

    steps.append(("ok", f"Step 5 — checksum sha256 …{body.get('checksum', '')[-16:]}"))

    if cfg["gate"]:
        checks = quality_checks(markdown, text)
        for c in checks:
            kind = "ok" if c["pass"] else "warn"
            steps.append((kind, f"Step 6 — gate[{c['id']}]: {'PASS' if c['pass'] else 'FAIL'} — "
                                f"{c['detail']}"))
        failed = [c["id"] for c in checks if not c["pass"]]
        steps.append(("err" if failed else "ok",
                      f"Step 6 — quality gate: {'FAILED: ' + ', '.join(failed) if failed else 'all checks pass'}"))

    record.update({
        "engine": engine, "numpages": body.get("numpages"), "markdownChars": len(markdown),
        "textChars": len(text), "durationMs": body.get("duration_ms"),
        "checksum": body.get("checksum"), "structure": structure,
        "tables": analyze_tables(markdown)[0],
    })
    if cfg["gate"]:
        record["gateChecks"] = quality_checks(markdown, text)
        record["gateFailed"] = [c["id"] for c in record["gateChecks"] if not c["pass"]]

    if cfg["out_dir"]:
        stem = re.sub(r"[^A-Za-z0-9._-]+", "_", name)[:120]
        saved = []
        for suffix, content in ((".markdown.md", markdown), (".text.txt", text),
                                (".response.json", json.dumps(body, ensure_ascii=False, indent=2))):
            p = os.path.join(cfg["out_dir"], stem + suffix)
            with open(p, "w", encoding="utf-8") as f:
                f.write(content)
            saved.append(p)
        record["savedFiles"] = saved
        steps.append(("ok", f"Step 7 — artifacts saved to {', '.join(saved)}"))

    if verbose:
        steps.append(("info", "structure: " + json.dumps(structure, ensure_ascii=False)))
        blocks = analyze_tables(markdown)
        for b in blocks[1]:
            steps.append(("info", f"table @line {b['startLine']}: header {b['headerCells']} "
                                  f"cells · {b['dataRows']} data rows · {b['raggedRows']} ragged "
                                  f"(ratio {b['raggedRatio']})"))

    record["steps"] = steps
    return {"ok": True, "steps": steps, "exit": 0, "record": record}


# --- CLI --------------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(description="Headless test/debug client for markdown-extract-service.")
    ap.add_argument("files", nargs="*", help="file(s) to convert (none → /health probe)")
    ap.add_argument("-u", "--url", default="http://127.0.0.1:3984")
    ap.add_argument("--to-markdown", action="store_true", help="use /to-markdown alias")
    ap.add_argument("-t", "--timeout", type=int, default=600)
    ap.add_argument("--json", action="store_true", dest="as_json")
    ap.add_argument("--out", dest="out_dir", metavar="DIR", help="save .markdown.md/.text.txt/.response.json")
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("--require-gate", action="store_true", help="exit 2 when any quality check FAILS")
    ap.add_argument("--no-gate", action="store_true", help="skip quality heuristics")
    ap.add_argument("--force-vision", action="store_true", help="force Vision LLM (Gemini Flash) extraction")
    ap.add_argument("--no-vision-fallback", action="store_true", help="disable automatic Vision LLM fallback on gate failure")
    ap.add_argument("--health", action="store_true", help="only probe /health and exit")
    args = ap.parse_args(argv)

    cfg = {
        "url": args.url,
        "endpoint": "/to-markdown" if args.to_markdown else "/extract",
        "timeout": args.timeout,
        "gate": not args.no_gate,
        "out_dir": args.out_dir,
        "force_vision": args.force_vision,
        "no_vision_fallback": args.no_vision_fallback,
    }
    if args.out_dir:
        os.makedirs(args.out_dir, exist_ok=True)

    if args.health or not args.files:
        health = http_get_json(args.url.rstrip("/") + "/health", args.timeout)
        if "transportError" in health:
            print(f"HEALTH ERROR: {health['transportError']}")
            return 1
        print(json.dumps(health["json"], ensure_ascii=False, indent=2) if args.as_json
              else f"health: HTTP {health['status']} " + json.dumps(health["json"], ensure_ascii=False))
        return 0 if health["status"] == 200 else 1

    results = []
    exit_code = 0
    for f in args.files:
        if not os.path.isfile(f):
            print(f"ERROR: no such file: {f}", file=sys.stderr)
            return 3
        r = test_file(f, cfg, args.verbose)
        results.append(r)

        if args.as_json:
            continue
        print("\n" + "=" * 78)
        for kind, text in r["steps"]:
            prefix = {"ok": "  ✓", "err": "  ✗", "warn": "  !", "info": "  ·"}[kind]
            print(f"{prefix} {text}")
        print("RESULT: " + ("OK" if r["ok"] else "ERROR"))

        if r.get("exit") == 1:
            exit_code = max(exit_code, 1)

    if args.as_json:
        print(json.dumps({
            "tool": "test_service.py",
            "url": cfg["url"] + cfg["endpoint"],
            "files": [r.get("record", {}) for r in results],
        }, ensure_ascii=False, indent=2))
    elif args.require_gate:
        for r in results:
            if r.get("record", {}).get("gateFailed"):
                exit_code = max(exit_code, 2)
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
