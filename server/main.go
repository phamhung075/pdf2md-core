// Command pdf2md-server is a minimal, high-performance HTTP wrapper around the
// native Rust pdf2md-core engine. It exists so users can test and develop
// against the conversion engine locally without any Python or ML dependencies.
//
// Endpoints:
//   GET  /            — tiny in-browser upload sandbox
//   GET  /health      — liveness probe
//   POST /convert     — convert a PDF (raw body) to Markdown (JSON)
package main

/*
#cgo LDFLAGS: -L${SRCDIR}/../crates/pdf2md-core/target/release -lpdf2md_core -lm -ldl
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>

char *pdf2md_convert(const uint8_t *data, size_t len);
int   pdf2md_is_digital(const uint8_t *data, size_t len);
void  pdf2md_free_string(char *ptr);
char *pdf2md_version(void);
*/
import "C"

import (
	"encoding/json"
	"io"
	"log"
	"net/http"
	"os"
	"strings"
	"time"
	"unsafe"
)

const maxUploadBytes = 100 << 20 // 100 MB

type mediaItem struct {
	Page       int     `json:"page"`
	X0         float64 `json:"x0"`
	Y0         float64 `json:"y0"`
	X1         float64 `json:"x1"`
	Y1         float64 `json:"y1"`
	Width      int     `json:"width"`
	Height     int     `json:"height"`
	Format     string  `json:"format"`
	Kind       string  `json:"kind"`
	Decorative bool    `json:"decorative"`
	Repeat     int     `json:"repeat"`
	DataB64    string  `json:"data_b64,omitempty"`
}

type convertResponse struct {
	OK         bool        `json:"ok"`
	Markdown   string      `json:"markdown,omitempty"`
	Pages      int         `json:"pages"`
	Words      int         `json:"words"`
	Tables     int         `json:"tables"`
	Media      []mediaItem `json:"media,omitempty"`
	DurationUs uint64      `json:"duration_us"`
	DurationMs int64       `json:"duration_ms,omitempty"`
	Engine     string      `json:"engine"`
	Version    string      `json:"version,omitempty"`
	Error      string      `json:"error,omitempty"`
}

func engineVersion() string {
	c := C.pdf2md_version()
	if c == nil {
		return "unknown"
	}
	defer C.pdf2md_free_string(c)
	return C.GoString(c)
}

func convertPDF(data []byte) convertResponse {
	start := time.Now()
	if len(data) == 0 {
		return convertResponse{OK: false, Engine: "pdf2md-core", Error: "empty request body"}
	}

	var ptr *C.uint8_t
	ptr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	cstr := C.pdf2md_convert(ptr, C.size_t(len(data)))
	if cstr == nil {
		return convertResponse{OK: false, Engine: "pdf2md-core", Error: "engine returned null"}
	}
	defer C.pdf2md_free_string(cstr)

	var out convertResponse
	if err := json.Unmarshal([]byte(C.GoString(cstr)), &out); err != nil {
		return convertResponse{OK: false, Engine: "pdf2md-core", Error: "failed to parse engine output: " + err.Error()}
	}
	out.Engine = "pdf2md-core"
	out.Version = engineVersion()
	out.DurationMs = time.Since(start).Milliseconds()
	return out
}

func writeJSON(w http.ResponseWriter, status int, payload any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(payload)
}

func handleHealth(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeJSON(w, http.StatusMethodNotAllowed, map[string]string{"error": "method not allowed"})
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"status":  "ok",
		"service": "pdf2md-core",
		"version": engineVersion(),
		"engine":  "rust-core",
	})
}

func handleConvert(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeJSON(w, http.StatusMethodNotAllowed, convertResponse{OK: false, Engine: "pdf2md-core", Error: "method not allowed"})
		return
	}

	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, maxUploadBytes))
	if err != nil {
		writeJSON(w, http.StatusBadRequest, convertResponse{OK: false, Engine: "pdf2md-core", Error: "failed to read body: " + err.Error()})
		return
	}

	resp := convertPDF(body)
	status := http.StatusOK
	if !resp.OK {
		status = http.StatusUnprocessableEntity
	}
	writeJSON(w, status, resp)
}

func handleIndex(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_, _ = io.WriteString(w, sandboxHTML)
}

func main() {
	port := strings.TrimSpace(os.Getenv("PORT"))
	if port == "" {
		port = "8989"
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/health", handleHealth)
	mux.HandleFunc("/convert", handleConvert)
	mux.HandleFunc("/", handleIndex)

	srv := &http.Server{
		Addr:              "0.0.0.0:" + port,
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       2 * time.Minute,
		WriteTimeout:      2 * time.Minute,
		IdleTimeout:       2 * time.Minute,
	}

	log.Printf("pdf2md-core server listening on http://0.0.0.0:%s (engine v%s)", port, engineVersion())
	log.Printf("  sandbox: http://127.0.0.1:%s/", port)
	log.Printf("  health:  http://127.0.0.1:%s/health", port)
	log.Printf("  convert: POST http://127.0.0.1:%s/convert", port)

	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatalf("server error: %v", err)
	}
}

const sandboxHTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>pdf2md-core sandbox</title>
<style>
  :root { color-scheme: light dark; }
  body { font-family: system-ui, -apple-system, sans-serif; margin: 2rem auto; max-width: 960px; padding: 0 1rem; }
  h1 { font-size: 1.4rem; }
  #drop { border: 2px dashed #888; border-radius: 10px; padding: 2rem; text-align: center; color: #666; cursor: pointer; }
  #drop.dragover { border-color: #4f8; color: #2a2; }
  pre { background: #111; color: #eee; padding: 1rem; border-radius: 8px; white-space: pre-wrap; word-wrap: break-word; min-height: 120px; }
  #status { margin: 0.5rem 0; font-size: 0.9rem; }
</style>
</head>
<body>
<h1>pdf2md-core sandbox</h1>
<p>Drop a digital (text-layer) PDF to convert it to Markdown via the native Rust engine.</p>
<div id="drop">Drop PDF here, or click to choose</div>
<input id="file" type="file" accept="application/pdf,.pdf" hidden>
<div id="status"></div>
<pre id="out">(converted Markdown will appear here)</pre>
<script>
const drop = document.getElementById('drop');
const file = document.getElementById('file');
const out = document.getElementById('out');
const status = document.getElementById('status');

drop.addEventListener('click', () => file.click());
drop.addEventListener('dragover', e => { e.preventDefault(); drop.classList.add('dragover'); });
drop.addEventListener('dragleave', () => drop.classList.remove('dragover'));
drop.addEventListener('drop', e => { e.preventDefault(); drop.classList.remove('dragover'); if (e.dataTransfer.files.length) handle(e.dataTransfer.files[0]); });
file.addEventListener('change', () => { if (file.files.length) handle(file.files[0]); });

async function handle(f) {
  status.textContent = 'Converting ' + f.name + ' ...';
  try {
    const resp = await fetch('/convert', { method: 'POST', body: f });
    const data = await resp.json();
    if (data.ok) {
      out.textContent = data.markdown;
      status.textContent = f.name + ' → ' + (data.pages ?? 0) + ' pages, ' + (data.words ?? 0) + ' words, ' + (data.duration_us ?? 0) + ' µs (Rust)';
    } else {
      out.textContent = '';
      status.textContent = 'Conversion failed: ' + (data.error || 'unknown error');
    }
  } catch (err) {
    status.textContent = 'Request error: ' + err;
  }
}
</script>
</body>
</html>
`
