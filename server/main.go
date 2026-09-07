// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).
//
// Command pdf2md-server is a minimal, high-performance HTTP wrapper around the
// native Rust pdf2md-core engine. It serves as the standalone public conversion
// server for local tooling, dev sandboxes, and desktop integration without
// Python or external ML dependencies.
//
// Endpoints:
//   GET  /health          — liveness probe ({"status": "ok", "version": "public-core"})
//   POST /api/v1/convert  — converts digital PDF (multipart or binary) to Markdown
//   POST /convert         — legacy alias for /api/v1/convert
//   GET  /                — in-browser test sandbox
package main

import (
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strings"
	"time"
)

const maxUploadBytes = 100 << 20 // 100 MB

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
	writeJSON(w, http.StatusOK, map[string]string{
		"status":  "ok",
		"version": "public-core",
	})
}

func extractPDFBytes(r *http.Request) ([]byte, error) {
	contentType := r.Header.Get("Content-Type")
	if strings.HasPrefix(contentType, "multipart/form-data") {
		if err := r.ParseMultipartForm(maxUploadBytes); err != nil {
			return nil, fmt.Errorf("failed to parse multipart form: %w", err)
		}
		var fileHeaders []*struct {
			Filename string
		}
		_ = fileHeaders

		headers := r.MultipartForm.File["file"]
		if len(headers) == 0 {
			headers = r.MultipartForm.File["pdf"]
		}
		if len(headers) == 0 {
			for _, hList := range r.MultipartForm.File {
				if len(hList) > 0 {
					headers = hList
					break
				}
			}
		}
		if len(headers) == 0 {
			return nil, errors.New("no file uploaded in multipart form (expected field 'file')")
		}
		file, err := headers[0].Open()
		if err != nil {
			return nil, fmt.Errorf("failed to open uploaded file: %w", err)
		}
		defer file.Close()
		return io.ReadAll(file)
	}

	body, err := io.ReadAll(http.MaxBytesReader(nil, r.Body, maxUploadBytes))
	if err != nil {
		return nil, fmt.Errorf("failed to read body: %w", err)
	}
	return body, nil
}

func handleConvert(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeJSON(w, http.StatusMethodNotAllowed, map[string]any{"error": "method not allowed"})
		return
	}

	pdfBytes, err := extractPDFBytes(r)
	if err != nil {
		writeJSON(w, http.StatusBadRequest, map[string]any{"error": err.Error()})
		return
	}
	if len(pdfBytes) == 0 {
		writeJSON(w, http.StatusBadRequest, map[string]any{"error": "empty PDF payload"})
		return
	}

	// Sanity check: Ensure the PDF has a digital text layer
	if !IsDigitalPDF(pdfBytes) {
		writeJSON(w, http.StatusUnprocessableEntity, map[string]any{
			"status": "error",
			"error":  "Scanned or non-digital PDF detected. OCR and vision processing require the commercial engine.",
		})
		return
	}

	detectVectors := r.URL.Query().Get("vectors") == "1" || r.URL.Query().Get("vectors") == "true"
	res, err := ConvertPDF(pdfBytes, detectVectors)
	if err != nil {
		writeJSON(w, http.StatusUnprocessableEntity, map[string]any{
			"status": "error",
			"error":  err.Error(),
		})
		return
	}

	accept := r.Header.Get("Accept")
	format := r.URL.Query().Get("format")
	if format == "raw" || format == "text" || format == "markdown" ||
		strings.Contains(accept, "text/markdown") || strings.Contains(accept, "text/plain") {
		w.Header().Set("Content-Type", "text/markdown; charset=utf-8")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(res.Markdown))
		return
	}

	writeJSON(w, http.StatusOK, res)
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
		port = "8080"
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/health", handleHealth)
	mux.HandleFunc("/api/v1/convert", handleConvert)
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

	log.Printf("pdf2md-core public server listening on http://0.0.0.0:%s (engine v%s)", port, EngineVersion())
	log.Printf("  health:  http://127.0.0.1:%s/health", port)
	log.Printf("  convert: POST http://127.0.0.1:%s/api/v1/convert", port)
	log.Printf("  sandbox: http://127.0.0.1:%s/", port)

	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatalf("server error: %v", err)
	}
}

//go:embed sandbox.html
var sandboxHTML string
