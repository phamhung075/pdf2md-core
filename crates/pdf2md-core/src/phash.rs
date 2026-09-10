// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! On-device perceptual image hashing (DCT-pHash).
//!
//! This primitive lets the commercial server recognise a repeated form/template
//! by its *perceptual* fingerprint rather than its exact bytes, so a Vision-LLM
//! rescue for an identical form can be satisfied from cache instead of re-hitting
//! the paid API. It is intentionally dependency-light and opt-in (behind the
//! `vision` cargo feature) so the public core / WASM builds are unaffected.
//!
//! Design:
//!   * [`perceptual_hash_64`] is the classic Krawetz DCT-pHash: downscale to
//!     32x32 luminance, take the 8x8 low-frequency DCT block, threshold against
//!     its median, and pack the 64 bits into a `u64`. Two images are "the same
//!     form" when their Hamming distance is small (typically <= 8 of 64 bits),
//!     which tolerates re-render at a different DPI, JPEG re-compression and
//!     minor colour drift — but is stable under meaningful content change.
//!   * [`page_dominant_raster_bytes`] extracts the dominant embedded raster on a
//!     PDF page ([`crate::media`] already strips the bytes of full-page scans as
//!     decorative, so this helper deliberately decodes *every* placement and keeps
//!     the largest one). A scanned form is exactly this case.
//!   * [`vision_signature`] hashes up to `max_pages` pages and returns a
//!     comma-joined, per-page hex signature the caller uses as a cache key.

use lopdf::{Document, ObjectId};

/// Downscale dimension for the DCT-pHash input.
const SIZE: usize = 32;
/// Low-frequency block side (8x8 -> 64-bit hash).
const BLOCK: usize = 8;

/// Bits may differ when two images are still the same visual form.
pub const DEFAULT_HAMMING_THRESHOLD: u32 = 8;

/// Compute a 64-bit DCT perceptual hash from encoded image bytes (PNG/JPEG).
///
/// Returns `None` when the payload cannot be decoded as an image.
pub fn perceptual_hash_64(bytes: &[u8]) -> Option<u64> {
    let img = image::load_from_memory(bytes).ok()?;
    let small = img
        .resize_exact(SIZE as u32, SIZE as u32, image::imageops::FilterType::Triangle)
        .to_luma8();
    let raw = small.as_raw();
    if raw.len() != SIZE * SIZE {
        return None;
    }

    // Float luminance, mean-subtracted so the DC term collapses to ~0 for a flat
    // region (which makes the hash robust to overall brightness).
    let mut f = vec![0f64; SIZE * SIZE];
    for (i, &v) in raw.iter().enumerate() {
        f[i] = v as f64;
    }
    let mean = f.iter().sum::<f64>() / (f.len() as f64);
    for v in f.iter_mut() {
        *v -= mean;
    }

    // Precompute cos((2x+1) * u * PI / (2*SIZE)) for u in [0, BLOCK).
    let cos: Vec<Vec<f64>> = (0..BLOCK)
        .map(|u| {
            (0..SIZE)
                .map(|x| {
                    ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI
                        / (2.0 * SIZE as f64))
                        .cos()
                })
                .collect()
        })
        .collect();

    // 2D DCT-II over the full 32x32 grid; collect the 8x8 top-left block.
    let mut coeffs = vec![0f64; BLOCK * BLOCK];
    for u in 0..BLOCK {
        for v in 0..BLOCK {
            let mut sum = 0f64;
            for x in 0..SIZE {
                let cu = cos[u][x];
                for y in 0..SIZE {
                    sum += f[y * SIZE + x] * cu * cos[v][y];
                }
            }
            // Standard DCT-II normalisation: downweight the DC-ish corner so the
            // median split is driven by structure, not average brightness.
            let alpha = |k: usize| {
                if k == 0 {
                    std::f64::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                }
            };
            coeffs[v * BLOCK + u] = sum * alpha(u) * alpha(v);
        }
    }

    // Threshold each coefficient against the block median -> 64-bit hash.
    let mut sorted = coeffs.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    let mut hash = 0u64;
    for (i, &c) in coeffs.iter().enumerate() {
        if c > median {
            hash |= 1u64 << i;
        }
    }
    Some(hash)
}

/// Extract the dominant (largest on-page area) embedded raster on one page,
/// decoded to PNG/JPEG bytes.
///
/// [`crate::media::raster::extract_page_media`] deliberately discards the byte
/// payload of full-page/background images (classified decorative) to keep the
/// markdown output lean; hashing a scanned form needs exactly those bytes, so we
/// independently walk the content stream and keep the largest decodable
/// placement.
pub fn page_dominant_raster_bytes(doc: &Document, page_id: ObjectId) -> Option<Vec<u8>> {
    use crate::media::raster::{
        collect_page_ops, decode_xobject_bytes, page_xobjects, scan_inline_images, Placement,
    };

    let xobjects = page_xobjects(doc, page_id)?;
    let content = doc.get_and_decode_page_content(page_id).ok()?;
    let (init_ctm, _) = crate::layout::glyph_stream::page_initial_transform(doc, page_id);

    let mut placements: Vec<Placement> = Vec::new();
    collect_page_ops(doc, &xobjects, &content, &mut placements, 0, &init_ctm);

    let raw = doc.get_page_content_with_limit(page_id, 64 << 20).unwrap_or_default();
    if !raw.is_empty() {
        placements.extend(scan_inline_images(&raw, init_ctm));
    }

    let mut best: Option<(f64, Vec<u8>)> = None;
    for p in placements {
        let area = (p.x1 - p.x0).abs() * (p.y1 - p.y0).abs();
        if area <= 1.0 {
            continue; // degenerate sub-point placement
        }
        if let Some((best_area, _)) = &best {
            if area <= *best_area {
                continue;
            }
        }
        if let Some(bytes) = decode_xobject_bytes(doc, &p.obj) {
            if !bytes.is_empty() {
                best = Some((area, bytes));
            }
        }
    }
    best.map(|(_, b)| b)
}

/// Build a whole-document perceptual signature: a comma-joined list of per-page
/// 64-bit hex hashes (up to `max_pages`), or `""` when no page yields a
/// hashable raster. An empty signature signals "no caching for this document".
pub fn vision_signature(doc: &Document, max_pages: usize) -> String {
    let cap = if max_pages == 0 { 8 } else { max_pages };
    let mut hashes: Vec<String> = Vec::new();
    for (idx, (_page_num, page_id)) in doc.get_pages().into_iter().enumerate() {
        if idx >= cap {
            break;
        }
        if let Some(bytes) = page_dominant_raster_bytes(doc, page_id) {
            if let Some(h) = perceptual_hash_64(&bytes) {
                hashes.push(format!("{:016x}", h));
            }
        }
    }
    hashes.join(",")
}

/// Hamming distance between two 64-bit hashes (0..=64).
pub fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Object;

    /// Encode an `RgbImage` to PNG bytes so the hash function gets a real image
    /// buffer without any fixture files.
    fn png_of(width: u32, height: u32, fill: impl Fn(u32, u32) -> (u8, u8, u8)) -> Vec<u8> {
        let mut img = image::RgbImage::new(width, height);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let (r, g, b) = fill(x, y);
            *px = image::Rgb([r, g, b]);
        }
        let dynimg = image::DynamicImage::ImageRgb8(img);
        let mut buf = Vec::new();
        dynimg
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .expect("png encode");
        buf
    }

    #[test]
    fn identical_buffers_hash_identically() {
        let a = png_of(128, 96, |x, y| ((x * 2) as u8, (y * 2) as u8, 128));
        let b = png_of(128, 96, |x, y| ((x * 2) as u8, (y * 2) as u8, 128));
        let ha = perceptual_hash_64(&a).expect("decode a");
        let hb = perceptual_hash_64(&b).expect("decode b");
        assert_eq!(ha, hb, "same pixels must yield identical hash");
        assert_eq!(hamming_distance(ha, hb), 0);
    }

    #[test]
    fn distinct_patterns_hash_differently() {
        // A structured diagonal vs. horizontal-line pattern must differ.
        let a = png_of(128, 128, |x, y| {
            if ((x as i32 + y as i32) / 12) % 2 == 0 {
                (240, 240, 240)
            } else {
                (20, 20, 20)
            }
        });
        let b = png_of(128, 128, |_x, y| {
            if (y / 10) % 2 == 0 {
                (240, 240, 240)
            } else {
                (20, 20, 20)
            }
        });
        let ha = perceptual_hash_64(&a).expect("decode a");
        let hb = perceptual_hash_64(&b).expect("decode b");
        assert!(
            hamming_distance(ha, hb) > 8,
            "patterns with different dominant orientation should be far apart, got {}",
            hamming_distance(ha, hb)
        );
    }

    #[test]
    fn scaling_preserves_hash() {
        // Render the *same* structured form pattern (an 8x8 block grid, the kind
        // of coarse luminance structure a real form has) at two very different
        // resolutions. pHash must be robust to this — re-rendering a repeated
        // form at a different DPI is exactly what the cache has to match.
        let mk = |w: u32, h: u32| {
            png_of(w, h, |x, y| {
                let bi = ((x as f32 / w as f32) * 8.0).floor() as u32;
                let bj = ((y as f32 / h as f32) * 8.0).floor() as u32;
                let v = ((bi * 37 + bj * 17 + bi * bj * 13) % 8) * 32;
                (v as u8, v as u8, v as u8)
            })
        };
        let small = mk(64, 64);
        let big = mk(256, 256);
        let hs = perceptual_hash_64(&small).expect("decode small");
        let hb = perceptual_hash_64(&big).expect("decode big");
        let dist = hamming_distance(hs, hb);
        assert!(
            dist <= DEFAULT_HAMMING_THRESHOLD,
            "resized copy should be within the Hamming threshold, got {dist}"
        );
    }

    #[test]
    fn invalid_bytes_yield_none() {
        assert!(perceptual_hash_64(b"not an image").is_none());
        assert!(perceptual_hash_64(b"").is_none());
    }

    #[test]
    fn vision_signature_hashes_an_embedded_image_pdf() {
        // Build a 1-page PDF with a single embedded JPEG XObject, then confirm
        // the doc-level signature is non-empty, deterministic, and stable under a
        // second load. This exercises `page_dominant_raster_bytes`, the one path
        // not covered by the raw-buffer hash tests.
        let mut img = image::RgbImage::new(64, 48);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let nx = x as f32 / 63.0;
            let ny = y as f32 / 47.0;
            *px = image::Rgb([(nx * 255.0) as u8, (ny * 255.0) as u8, 128]);
        }
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .expect("jpeg encode");

        let mut doc = lopdf::Document::new();

        let mut img_dict = lopdf::Dictionary::new();
        img_dict.set(b"Type", Object::Name(b"XObject".to_vec()));
        img_dict.set(b"Subtype", Object::Name(b"Image".to_vec()));
        img_dict.set(b"Width", Object::Integer(64));
        img_dict.set(b"Height", Object::Integer(48));
        img_dict.set(b"ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        img_dict.set(b"BitsPerComponent", Object::Integer(8));
        img_dict.set(b"Filter", Object::Name(b"DCTDecode".to_vec()));
        let img_id = doc.add_object(Object::Stream(lopdf::Stream::new(img_dict, jpeg)));

        let content = "q 120 0 0 96 0 0 cm /Im0 Do Q\n";
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content.as_bytes().to_vec(),
        )));

        let mut page_dict = lopdf::Dictionary::new();
        page_dict.set(b"Type", Object::Name(b"Page".to_vec()));
        page_dict.set(
            b"MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(200),
                Object::Integer(160),
            ]),
        );
        let mut res_dict = lopdf::Dictionary::new();
        let mut xobj = lopdf::Dictionary::new();
        xobj.set(b"Im0", Object::Reference(img_id));
        res_dict.set(b"XObject", Object::Dictionary(xobj));
        page_dict.set(b"Resources", Object::Dictionary(res_dict));
        page_dict.set(b"Contents", Object::Reference(content_id));
        let page_id = doc.add_object(Object::Dictionary(page_dict));

        let mut pages_dict = lopdf::Dictionary::new();
        pages_dict.set(b"Type", Object::Name(b"Pages".to_vec()));
        pages_dict.set(b"Kids", Object::Array(vec![Object::Reference(page_id)]));
        pages_dict.set(b"Count", Object::Integer(1));
        let pages_id = doc.add_object(Object::Dictionary(pages_dict));
        // lopdf's page iterator resolves the tree from `/Root` -> `/Type /Catalog`
        // -> `/Pages`; a Root that points straight at the page tree yields 0 pages.
        let mut catalog_dict = lopdf::Dictionary::new();
        catalog_dict.set(b"Type", Object::Name(b"Catalog".to_vec()));
        catalog_dict.set(b"Pages", Object::Reference(pages_id));
        let catalog_id = doc.add_object(Object::Dictionary(catalog_dict));
        doc.trailer.set(b"Root", Object::Reference(catalog_id));

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save pdf");

        let sig1 = vision_signature(&Document::load_mem(&bytes).expect("load 1"), 8);
        assert!(!sig1.is_empty(), "embedded-image PDF must produce a signature");
        assert_eq!(
            sig1.split(',').count(),
            1,
            "single-page doc should have exactly one hash, got {sig1}"
        );
        let sig2 = vision_signature(&Document::load_mem(&bytes).expect("load 2"), 8);
        assert_eq!(sig1, sig2, "signature must be deterministic");
    }
}
