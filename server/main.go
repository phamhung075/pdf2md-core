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
char *pdf2md_convert_ex(const uint8_t *data, size_t len, int detect_vectors);
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

type docBlock struct {
	Page int     `json:"page,omitempty"`
	Kind string  `json:"kind"`
	X0   float64 `json:"x0"`
	Y0   float64 `json:"y0"`
	X1   float64 `json:"x1"`
	Y1   float64 `json:"y1"`
	Text string  `json:"text"`
}

type convertResponse struct {
	OK         bool        `json:"ok"`
	Markdown   string      `json:"markdown,omitempty"`
	Pages      int         `json:"pages"`
	Words      int         `json:"words"`
	Tables     int         `json:"tables"`
	Media      []mediaItem `json:"media,omitempty"`
	Blocks     []docBlock  `json:"blocks,omitempty"`
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

func convertPDF(data []byte, detectVectors bool) convertResponse {
	start := time.Now()
	if len(data) == 0 {
		return convertResponse{OK: false, Engine: "pdf2md-core", Error: "empty request body"}
	}

	var ptr *C.uint8_t
	ptr = (*C.uint8_t)(unsafe.Pointer(&data[0]))
	var cstr *C.char
	if detectVectors {
		cstr = C.pdf2md_convert_ex(ptr, C.size_t(len(data)), 1)
	} else {
		cstr = C.pdf2md_convert(ptr, C.size_t(len(data)))
	}
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

	detectVectors := r.URL.Query().Get("vectors") == "1" || r.URL.Query().Get("vectors") == "true"
	resp := convertPDF(body, detectVectors)
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
  body { font-family: system-ui, -apple-system, sans-serif; margin: 2rem auto; max-width: 1080px; padding: 0 1rem; }
  h1 { font-size: 1.4rem; }
  #drop { border: 2px dashed #888; border-radius: 10px; padding: 2rem; text-align: center; color: #666; cursor: pointer; }
  #drop.dragover { border-color: #4f8; color: #2a2; }
  pre { background: #111; color: #eee; padding: 1rem; border-radius: 8px; white-space: pre-wrap; word-wrap: break-word; min-height: 120px; }
  #status { margin: 0.5rem 0; font-size: 0.9rem; }
  h3 { margin: 1.1rem 0 0.3rem; font-size: 1rem; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 0.75rem; margin: 0.5rem 0 1rem; }
  .card { border: 1px solid #555; border-radius: 10px; overflow: hidden; background: #fff; color: #111; display: flex; flex-direction: column; }
  .card .thumb { width: 100%; height: 150px; object-fit: contain; background: repeating-conic-gradient(#ececec 0% 25%, #fff 0% 50%) 50% / 14px 14px; display: block; border: 0; }
  .card .thumb.pdf { height: 150px; }
  .card .meta { padding: 0.45rem 0.6rem; font-size: 0.75rem; line-height: 1.5; border-top: 1px solid #ddd; word-break: break-all; }
  .badge { display: inline-block; padding: 1px 8px; border-radius: 10px; background: #3a66c4; color: #fff; font-size: 0.7rem; font-weight: 600; }
  .badge.decorative { background: #8a8a8a; }
  .badge.signature { background: #b04a2e; }
  .badge.logo { background: #2e7d4f; }
  .badge.photo { background: #7a4ab0; }
  .badge.chart, .badge.diagram { background: #b08a2e; }
  .card a.dl { font-size: 0.75rem; }
  details { margin: 0.35rem 0; }
  summary { cursor: pointer; font-weight: 600; }
</style>
</head>
<body>
<h1>pdf2md-core sandbox</h1>
<p>Drop a digital (text-layer) PDF to convert it via the native Rust engine. Cut images (photos, logos, signatures, charts…) appear as thumbnails below, then the layout blocks and Markdown.</p>
<div id="drop">Drop PDF here, or click to choose</div>
<input id="file" type="file" accept="application/pdf,.pdf" hidden>
<label style="display:block; margin:0.6rem 0;"><input type="checkbox" id="vectors"> Also cut pure-vector figures (charts / diagrams) as clipped PDFs</label>
<div id="status"></div>
<section id="mediaSec" hidden>
  <h3>Cut images / media (<span id="mediaCount"></span>)</h3>
  <div id="mediaGrid" class="grid"></div>
</section>
<details id="blocksWrap" hidden>
  <summary>Layout blocks (<span id="blocksCount"></span>)</summary>
  <pre id="blocksPre"></pre>
</details>
<details id="mdWrap">
  <summary>Markdown</summary>
  <pre id="out">(converted Markdown will appear here)</pre>
</details>
<script>
const drop = document.getElementById('drop');
const file = document.getElementById('file');
const out = document.getElementById('out');
const status = document.getElementById('status');
const mediaSec = document.getElementById('mediaSec');
const mediaCount = document.getElementById('mediaCount');
const mediaGrid = document.getElementById('mediaGrid');
const blocksWrap = document.getElementById('blocksWrap');
const blocksCount = document.getElementById('blocksCount');
const blocksPre = document.getElementById('blocksPre');

const vectorsChk = document.getElementById('vectors');

drop.addEventListener('click', () => file.click());
drop.addEventListener('dragover', e => { e.preventDefault(); drop.classList.add('dragover'); });
drop.addEventListener('dragleave', () => drop.classList.remove('dragover'));
drop.addEventListener('drop', e => { e.preventDefault(); drop.classList.remove('dragover'); if (e.dataTransfer.files.length) handle(e.dataTransfer.files[0]); });
file.addEventListener('change', () => { if (file.files.length) handle(file.files[0]); });

function dataUri(m) { return 'data:' + (m.format || 'application/octet-stream') + ';base64,' + m.data_b64; }
function fileExt(m) { return (m.format || 'bin').split('/').pop(); }

function cardFor(m, i) {
  const card = document.createElement('div');
  card.className = 'card';

  const thumb = document.createElement(m.format === 'application/pdf' ? 'iframe' : 'img');
  thumb.className = 'thumb' + (m.format === 'application/pdf' ? ' pdf' : '');
  thumb.title = 'kind=' + (m.kind || '?') + ' page=' + m.page + ' ' + m.width + 'x' + m.height;
  if (m.data_b64) {
    const uri = dataUri(m);
    if (m.format === 'application/pdf') {
      thumb.src = uri; // data: PDFs render in most Chromium/Firefox embeds
    } else {
      thumb.src = uri;
    }
  } else {
    thumb.style.display = 'none';
  }

  const meta = document.createElement('div');
  meta.className = 'meta';

  const badge = document.createElement('span');
  badge.className = 'badge' + (m.decorative ? ' decorative' : '') + (m.kind ? ' ' + m.kind : '');
  badge.textContent = m.kind || m.format;

  meta.appendChild(badge);
  meta.appendChild(document.createElement('br'));
  meta.appendChild(document.createTextNode('page ' + m.page + ' · ' + m.width + '×' + m.height + 'px · ' + (m.format || '?')));
  if (m.decorative) meta.appendChild(document.createTextNode(' · decorative'));
  if (m.repeat > 1) meta.appendChild(document.createTextNode(' · repeated ×' + m.repeat));
  meta.appendChild(document.createElement('br'));

  if (m.data_b64) {
    const dl = document.createElement('a');
    dl.className = 'dl';
    dl.href = dataUri(m);
    dl.download = 'media-p' + m.page + '-' + i + '.' + fileExt(m).replace('jpeg', 'jpg');
    dl.textContent = '⬇ download';
    meta.appendChild(dl);
  } else {
    meta.appendChild(document.createTextNode('no bytes (filtered / skipped)'));
  }

  card.appendChild(thumb);
  card.appendChild(meta);
  return card;
}

function renderMedia(list) {
  mediaGrid.innerHTML = '';
  if (!list || !list.length) { mediaSec.hidden = true; return; }
  mediaCount.textContent = list.length;
  list.forEach((m, i) => mediaGrid.appendChild(cardFor(m, i)));
  mediaSec.hidden = false;
}

function renderBlocks(list) {
  if (!list || !list.length) { blocksWrap.hidden = true; return; }
  blocksCount.textContent = list.length;
  blocksPre.textContent = JSON.stringify(list, null, 2);
  blocksWrap.hidden = false;
}

async function handle(f) {
  status.textContent = 'Converting ' + f.name + ' ...';
  mediaSec.hidden = true;
  blocksWrap.hidden = true;
  out.textContent = '';
  try {
    const resp = await fetch('/convert' + (vectorsChk.checked ? '?vectors=1' : ''), { method: 'POST', body: f });
    const data = await resp.json();
    if (data.ok) {
      out.textContent = data.markdown;
      renderMedia(data.media || []);
      renderBlocks(data.blocks || []);
      const n = (data.media || []).length;
      status.textContent = f.name + ' → ' + (data.pages ?? 0) + ' pages, ' + (data.words ?? 0) + ' words, ' + (data.tables ?? 0) + ' tables, ' + n + ' image(s), ' + (data.duration_us ?? 0) + ' µs (Rust)';
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
