// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

package main

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// repeatingByteReader is an endless stream of a single byte. Tests cap it with
// io.LimitReader so an oversized body is never materialized in full.
type repeatingByteReader byte

func (r repeatingByteReader) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = byte(r)
	}
	return len(p), nil
}

const testBoundary = "pdf2md-test-boundary"

// oversizedMultipartBody streams a multipart form whose file part alone pushes
// the encoded body past maxUploadBytes+multipartOverheadBytes.
func oversizedMultipartBody() io.Reader {
	prefix := "--" + testBoundary + "\r\n" +
		`Content-Disposition: form-data; name="file"; filename="big.pdf"` + "\r\n" +
		"Content-Type: application/pdf\r\n\r\n"
	suffix := "\r\n--" + testBoundary + "--\r\n"
	return io.MultiReader(
		strings.NewReader(prefix),
		io.LimitReader(repeatingByteReader('A'), maxUploadBytes+multipartOverheadBytes),
		strings.NewReader(suffix),
	)
}

func multipartRequest(body io.Reader) *http.Request {
	req := httptest.NewRequest(http.MethodPost, "/api/v1/convert", body)
	req.Header.Set("Content-Type", "multipart/form-data; boundary="+testBoundary)
	return req
}

func TestOversizedMultipartReturns413(t *testing.T) {
	rec := httptest.NewRecorder()
	handleConvert(rec, multipartRequest(oversizedMultipartBody()))

	if rec.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("status = %d, want %d; body = %s", rec.Code, http.StatusRequestEntityTooLarge, rec.Body.String())
	}
}

func TestOversizedRawBodyReturns413(t *testing.T) {
	body := io.LimitReader(repeatingByteReader('A'), maxUploadBytes+1)
	req := httptest.NewRequest(http.MethodPost, "/api/v1/convert", body)
	req.Header.Set("Content-Type", "application/pdf")

	rec := httptest.NewRecorder()
	handleConvert(rec, req)

	if rec.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("status = %d, want %d; body = %s", rec.Code, http.StatusRequestEntityTooLarge, rec.Body.String())
	}
}

// TestSmallMultipartNotRejectedByCap sanity-checks that the new whole-body cap
// leaves ordinary small uploads alone: a tiny non-PDF still reaches the normal
// validation path (422) instead of being rejected as oversized (413).
func TestSmallMultipartNotRejectedByCap(t *testing.T) {
	var buf bytes.Buffer
	buf.WriteString("--" + testBoundary + "\r\n")
	buf.WriteString(`Content-Disposition: form-data; name="file"; filename="small.pdf"` + "\r\n")
	buf.WriteString("Content-Type: application/pdf\r\n\r\n")
	buf.WriteString("not a pdf")
	buf.WriteString("\r\n--" + testBoundary + "--\r\n")

	rec := httptest.NewRecorder()
	handleConvert(rec, multipartRequest(&buf))

	if rec.Code == http.StatusRequestEntityTooLarge {
		t.Fatalf("small multipart body was rejected by the size cap: %s", rec.Body.String())
	}
}
