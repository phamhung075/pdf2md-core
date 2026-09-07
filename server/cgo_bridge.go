// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

package main

/*
#cgo CFLAGS: -I${SRCDIR}/../crates/pdf2md-core/include
#cgo LDFLAGS: -L${SRCDIR}/../crates/pdf2md-core/target/release -Wl,-rpath,${SRCDIR}/../crates/pdf2md-core/target/release -lpdf2md_core -lm -ldl
#include "pdf2md.h"
#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"time"
	"unsafe"
)

// MediaItem represents an extracted media element (image).
type MediaItem struct {
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

// DocBlock represents a structured layout block in reading order.
type DocBlock struct {
	Page int     `json:"page,omitempty"`
	Kind string  `json:"kind"`
	X0   float64 `json:"x0"`
	Y0   float64 `json:"y0"`
	X1   float64 `json:"x1"`
	Y1   float64 `json:"y1"`
	Text string  `json:"text"`
}

// ConvertResult holds the result of a PDF-to-Markdown conversion.
type ConvertResult struct {
	OK         bool        `json:"ok"`
	Markdown   string      `json:"markdown"`
	Pages      int         `json:"pages"`
	Words      int         `json:"words"`
	Tables     int         `json:"tables"`
	Media      []MediaItem `json:"media,omitempty"`
	Blocks     []DocBlock  `json:"blocks,omitempty"`
	DurationUs uint64      `json:"duration_us"`
	DurationMs int64       `json:"duration_ms"`
	Engine     string      `json:"engine"`
	Version    string      `json:"version"`
	Error      string      `json:"error,omitempty"`
}

// IsDigitalPDF checks whether the given PDF bytes contain a digital text layer.
func IsDigitalPDF(data []byte) bool {
	if len(data) < 32 {
		return false
	}
	res := C.pdf2md_is_digital((*C.uint8_t)(unsafe.Pointer(&data[0])), C.size_t(len(data)))
	return res == 1
}

// ConvertPDF runs the native Rust engine to convert PDF bytes to Markdown.
func ConvertPDF(data []byte, detectVectors bool) (*ConvertResult, error) {
	if len(data) == 0 {
		return nil, errors.New("empty PDF payload")
	}

	start := time.Now()
	var cStr *C.char
	ptr := (*C.uint8_t)(unsafe.Pointer(&data[0]))
	sz := C.size_t(len(data))

	if detectVectors {
		cStr = C.pdf2md_convert_ex(ptr, sz, 1)
	} else {
		cStr = C.pdf2md_convert(ptr, sz)
	}

	if cStr == nil {
		return nil, errors.New("native core returned null pointer")
	}
	defer C.pdf2md_free_string(cStr)

	raw := C.GoString(cStr)

	var rawResp struct {
		OK         bool        `json:"ok"`
		Markdown   string      `json:"markdown"`
		Pages      int         `json:"pages"`
		Words      int         `json:"words"`
		Tables     int         `json:"tables"`
		Media      []MediaItem `json:"media"`
		Blocks     []DocBlock  `json:"blocks"`
		DurationUs uint64      `json:"duration_us"`
		Error      string      `json:"error"`
	}

	if err := json.Unmarshal([]byte(raw), &rawResp); err != nil {
		return nil, fmt.Errorf("failed to parse native engine response: %w", err)
	}

	if !rawResp.OK {
		return nil, errors.New(rawResp.Error)
	}

	durMs := time.Since(start).Milliseconds()

	return &ConvertResult{
		OK:         true,
		Markdown:   rawResp.Markdown,
		Pages:      rawResp.Pages,
		Words:      rawResp.Words,
		Tables:     rawResp.Tables,
		Media:      rawResp.Media,
		Blocks:     rawResp.Blocks,
		DurationUs: rawResp.DurationUs,
		DurationMs: durMs,
		Engine:     "pdf2md-core",
		Version:    EngineVersion(),
	}, nil
}

// EngineVersion returns the underlying Rust engine version string.
func EngineVersion() string {
	cStr := C.pdf2md_version()
	if cStr == nil {
		return "unknown"
	}
	defer C.pdf2md_free_string(cStr)
	return C.GoString(cStr)
}
