// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Media extraction — raster images and vector figures from PDF documents.

pub mod codecs;
pub mod raster;
pub mod vector;

pub use codecs::{b64encode, encode_png_rgba};
pub use raster::extract_page_media;
pub use vector::extract_page_vector_figures;

use serde::{Deserialize, Serialize};

/// Role guess for an extracted placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Logo,
    Chart,
    Diagram,
    Photo,
    Signature,
    Barcode,
    Decorative,
    Other,
}

impl MediaKind {
    /// Stable lowercase name for the wire/markdown formats.
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Logo => "logo",
            MediaKind::Chart => "chart",
            MediaKind::Diagram => "diagram",
            MediaKind::Photo => "photo",
            MediaKind::Signature => "signature",
            MediaKind::Barcode => "barcode",
            MediaKind::Decorative => "decoration",
            MediaKind::Other => "image",
        }
    }
}

/// One extracted object (deduped per page) with decoded bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    /// 1-based page number.
    pub page: usize,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub width: u32,
    pub height: u32,
    /// "image/jpeg" | "image/png" | "application/pdf"
    pub format: String,
    pub kind: MediaKind,
    pub decorative: bool,
    /// Times this object is painted on this page.
    pub repeat: u32,
    /// Raw encoded bytes (JPEG passthrough, PNG re-encode, or clipped PDF).
    #[serde(skip)]
    pub data: Vec<u8>,
    /// Base64 of `data` (wire format).
    #[serde(rename = "data_b64", default, skip_serializing_if = "String::is_empty")]
    pub data_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::codecs::decode_ascii85;

    #[test]
    fn b64_roundtrip_vectors() {
        assert_eq!(b64encode(b""), "");
        assert_eq!(b64encode(b"f"), "Zg==");
        assert_eq!(b64encode(b"fo"), "Zm8=");
        assert_eq!(b64encode(b"foo"), "Zm9v");
        assert_eq!(b64encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn png_writer_produces_valid_signature_and_chunks() {
        let png = encode_png_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(&png[12..16], b"IHDR");
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!((w, h), (2, 1));
        assert!(png.windows(4).rev().any(|x| x == b"IEND"));
        assert!(png.windows(4).any(|x| x == b"IDAT"));
    }

    #[test]
    fn ascii85_roundtrip() {
        let enc = b"9jqo^BlbD-BleB1DJ+*+F(f,q/0JhKF<GL>Cj@.4Gp$d7F!,L7@<6@)/0JDEF<G%<+EV:2F!,O<DJ+*.@<*K0@<6L(Df-\\0Ec5e;DffZ(EZee.Bl.9pF\"AGXBPCsi+DGm>@3BB/F*&OCAfu2/AKYi(DIb:@FD,*)+C]U=@3BN#EcYf8ATD3s@q?d$AftVqCh[NqF<G:8+EV:.+Cf>-FD5W8ARlolDIal(DId<j@<?3r@:F%a+D58'ATD4$Bl@l3De:,-DJs`8ARoFb/0JMK@qB4^F!,R<AKZ&-DfTqBG%G>uD.RTpAKYo'+CT/5+Cei#DII?(E,9)oF*2M7/c~>";
        let dec = decode_ascii85(&enc[..]).expect("decode");
        let text = String::from_utf8_lossy(&dec);
        assert!(text.contains("Man is distinguished"));
    }
}
