// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).
//
// ATTRIBUTION — Apache-2.0 / BSD-3-Clause NOTICE (required by Apache License 2.0 §4)
// This file is a Rust port/derivative of PDFium's `CPDF_TextPage` analytic
// algorithms (PDFium file `core/fpdftext/cpdf_textpage.cpp`), substantially
// rewritten in safe Rust (renamed structures, adjusted thresholds, new data
// model). The PDFium-derived algorithmic portions remain licensed under the
// Apache License 2.0 and the BSD-3-Clause notice below; the original Rust
// additions are licensed under BSL-1.1. Any modified source must retain this
// attribution and the upstream copyright
// (also reproduced in THIRD_PARTY_LICENSES / NOTICE):
//
// Copyright 2014 The PDFium Authors
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// * Redistributions of source code must retain the above copyright
//   notice, this list of conditions and the following disclaimer.
// * Redistributions in binary form must reproduce the above
//   copyright notice, this list of conditions and the following disclaimer
//   in the documentation and/or other materials provided with the
//   distribution.
// * Neither the name of Google Inc. nor the names of its
//   contributors may be used to endorse or promote products derived from
//   this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
// OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
// LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
// THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
// (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//
// The PDFium-derived portions are available under the Apache License, Version
// 2.0 (http://www.apache.org/licenses/LICENSE-2.0). See THIRD_PARTY_LICENSES
// for the full license text.

//! Standalone Rust implementation of PDFium's `CPDF_TextPage` analytical algorithms.
//!
//! Re-implements the core mathematical transformations (ISO 32000 §8.3.3 & §9.4)
//! and character/word/line spatial clustering heuristics from PDFium
//! (`core/fpdftext/cpdf_textpage.cpp`). This is a derivative work of PDFium and
//! is distributed with the required Apache-2.0 / BSD-3-Clause attribution above.
//!
//! Zero dependencies on external C/C++ runtimes or GUI rasterizers. 100% Pure Safe Rust.

mod geometry;
pub use geometry::*;
mod text_state;
pub use text_state::*;
mod clustering;
pub use clustering::*;

use serde::{Deserialize, Serialize};

// ===========================================================================
// 1. 2D Axis-Aligned Bounding Box (Rect)
// ===========================================================================

#[cfg(test)]
mod tests;
