# pdf2md-server — mini Go dev server

A minimal, high-performance HTTP wrapper around the native Rust `pdf2md-core`
engine. Run it locally to test and develop against the conversion engine with no
Python or ML dependencies.

## Endpoints

| Method | Path        | Description                                              |
| :----- | :---------- | :------------------------------------------------------- |
| GET    | `/`         | Tiny in-browser upload sandbox                            |
| GET    | `/health`   | Liveness probe (`{"status":"ok", ...}`)                  |
| POST   | `/convert`  | Convert a raw PDF body to Markdown (JSON)                |

## Build & run

Prerequisites: [Rust](https://rustup.rs) and [Go](https://go.dev/dl) 1.22+.

```bash
# from pdf2md-core/server
make run
# → listening on http://0.0.0.0:8989  (override with PORT=8080 make run)
```

The Makefile builds the Rust core as a release `cdylib` (`cargo build --release
--no-default-features`) and links it into the Go binary via cgo, embedding the
library path as an rpath so the binary runs without `LD_LIBRARY_PATH`.

### Manual build (without make)

```bash
cd ../crates/pdf2md-core && cargo build --release --no-default-features && cd ../../server
CGO_ENABLED=1 go build -o bin/pdf2md-server .
# run (Linux):
LD_LIBRARY_PATH=../crates/pdf2md-core/target/release ./bin/pdf2md-server
```

## Convert a PDF

```bash
curl -X POST http://127.0.0.1:8989/convert \
  --data-binary @document.pdf \
  -H 'Content-Type: application/pdf'
```

Response:

```json
{
  "ok": true,
  "markdown": "# ...",
  "pages": 3,
  "words": 412,
  "tables": 0,
  "duration_us": 187,
  "duration_ms": 1,
  "engine": "pdf2md-core",
  "version": "0.1.0"
}
```

A scanned (image-only) PDF returns HTTP `422` with `{"ok": false, "error": "..."}`,
because the Rust fast path requires a digital text layer.
