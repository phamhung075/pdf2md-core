# Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
# SPDX-License-Identifier: BSL-1.1
# Licensed under the Business Source License 1.1 (BSL-1.1).
#
# Standalone, zero-dependency PDF-to-Markdown HTTP server (< 25 MB image).
#
# Build from 'public' directory:
#   docker build -t pdf2md-server -f server/Dockerfile .
#
# Build from repository root:
#   docker build -t pdf2md-server -f public/server/Dockerfile public
#
# Run:
#   docker run -p 8080:8080 pdf2md-server

# ==============================================================================
# Stage 1: Build Rust core static library (libpdf2md_core.a)
# ==============================================================================
FROM rust:1.80-alpine AS rust-builder
RUN apk add --no-cache musl-dev

WORKDIR /src
COPY crates/pdf2md-core crates/pdf2md-core
WORKDIR /src/crates/pdf2md-core
RUN cargo build --release --no-default-features

# ==============================================================================
# Stage 2: Build statically linked Go binary
# ==============================================================================
FROM golang:1.22-alpine AS go-builder
RUN apk add --no-cache build-base

WORKDIR /src
COPY server server
COPY crates/pdf2md-core/include crates/pdf2md-core/include
COPY --from=rust-builder /src/crates/pdf2md-core/target/release/libpdf2md_core.a /src/crates/pdf2md-core/target/release/libpdf2md_core.a

WORKDIR /src/server
ENV CGO_ENABLED=1
RUN go build -ldflags="-s -w -extldflags '-static'" -o /bin/pdf2md-server .

# ==============================================================================
# Stage 3: Minimal runtime image (< 25 MB total size)
# ==============================================================================
FROM alpine:3.20
RUN apk --no-cache add ca-certificates tzdata

COPY --from=go-builder /bin/pdf2md-server /usr/local/bin/pdf2md-server

EXPOSE 8080
ENV PORT=8080

ENTRYPOINT ["/usr/local/bin/pdf2md-server"]
