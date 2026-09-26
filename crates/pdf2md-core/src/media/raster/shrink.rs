// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Raster image extraction from PDF content streams and XObject resources.

use super::*;

/// Box-downsample an RGBA buffer so neither dimension exceeds `max_dim`,
/// preserving aspect ratio; a no-op (returns the input unchanged) when the
/// image already fits or `max_dim` is 0/disabled. Each output pixel is the
/// average of its source box (not nearest-neighbor), so downscaled scans of
/// text stay legible instead of aliasing into noise.
pub(super) fn downscale_rgba(rgba: &[u8], width: u32, height: u32, max_dim: u32) -> (Vec<u8>, u32, u32) {
    if max_dim == 0 || (width <= max_dim && height <= max_dim) || width == 0 || height == 0 {
        return (rgba.to_vec(), width, height);
    }
    let scale = max_dim as f64 / width.max(height) as f64;
    let new_w = ((width as f64 * scale).round() as u32).max(1);
    let new_h = ((height as f64 * scale).round() as u32).max(1);

    let mut out = vec![0u8; new_w as usize * new_h as usize * 4];
    for oy in 0..new_h {
        let y0 = (oy as u64 * height as u64 / new_h as u64) as u32;
        let y1 = (((oy + 1) as u64 * height as u64).div_ceil(new_h as u64)) as u32;
        let y1 = y1.max(y0 + 1).min(height);
        for ox in 0..new_w {
            let x0 = (ox as u64 * width as u64 / new_w as u64) as u32;
            let x1 = (((ox + 1) as u64 * width as u64).div_ceil(new_w as u64)) as u32;
            let x1 = x1.max(x0 + 1).min(width);

            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                let row = sy as usize * width as usize * 4;
                for sx in x0..x1 {
                    let i = row + sx as usize * 4;
                    r += rgba[i] as u64;
                    g += rgba[i + 1] as u64;
                    b += rgba[i + 2] as u64;
                    a += rgba[i + 3] as u64;
                    n += 1;
                }
            }
            let n = n.max(1);
            let oi = (oy as usize * new_w as usize + ox as usize) * 4;
            out[oi] = (r / n) as u8;
            out[oi + 1] = (g / n) as u8;
            out[oi + 2] = (b / n) as u8;
            out[oi + 3] = (a / n) as u8;
        }
    }
    (out, new_w, new_h)
}

/// Hard cap on the number of encode attempts the adaptive shrink may perform.
/// Keeps the loop deterministic and bounded (a handful of steps, never a
/// fine-grained search) so the Tier A embed path stays low-latency. Budgeted as
/// 3 full-res JPEG quality rungs + 1 downscaled JPEG rung + 5 PNG downscale
/// steps.
pub(super) const SHRINK_MAX_STEPS: usize = 9;

/// Longest-edge floor for adaptive downscaling. Below ~400 px a chart/photo
/// loses its labels and body text stops being legible, at which point the
/// honest "*[omitted]*" placeholder is more useful than an illegible
/// thumbnail.
pub(super) const SHRINK_MIN_LONG_EDGE: u32 = 400;

/// Maximum length of the downscale ladder. 5 steps is enough to walk a
/// 1536 px image (the default extraction cap) down to the 400 px floor.
pub(super) const SHRINK_LADDER_MAX: usize = 5;

/// Successive longest-edge targets for the adaptive downscale, from just under
/// the source size down to [`SHRINK_MIN_LONG_EDGE`] (25% smaller per step). The
/// floor is always the final candidate even when the ladder is truncated, so a
/// large source still tries the smallest size before giving up. Empty when the
/// image is already at or below the floor (nothing to trade).
pub(super) fn downscale_ladder(long_edge: u32) -> Vec<u32> {
    let mut out = Vec::new();
    if long_edge <= SHRINK_MIN_LONG_EDGE {
        return out;
    }
    let mut t = long_edge;
    while t > SHRINK_MIN_LONG_EDGE && out.len() < SHRINK_LADDER_MAX {
        let next = (t * 3 / 4).max(SHRINK_MIN_LONG_EDGE);
        if next >= t {
            break;
        }
        out.push(next);
        if next == SHRINK_MIN_LONG_EDGE {
            break;
        }
        t = next;
    }
    if out.last().copied() != Some(SHRINK_MIN_LONG_EDGE) {
        out.pop();
        out.push(SHRINK_MIN_LONG_EDGE);
    }
    out
}

/// Exact base64 length of `n` raw bytes, without allocating the string.
#[inline]
pub(super) fn b64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// Decode an already-encoded raster to RGBA for the adaptive shrink path.
///
/// PNG goes through the dependency-light [`decode_png_rgba`] (always
/// available). JPEG has no decoder in the default build, so it is only
/// decodable under the optional `vision` feature. Returns `(rgba, w, h,
/// has_alpha)`.
pub(super) fn decode_for_shrink(data: &[u8], format: &str) -> Option<(Vec<u8>, u32, u32, bool)> {
    if format == "image/png" {
        let (rgba, w, h) = decode_png_rgba(data)?;
        let has_alpha = rgba.chunks_exact(4).any(|p| p[3] != 255);
        Some((rgba, w, h, has_alpha))
    } else {
        #[cfg(feature = "vision")]
        {
            // Probe the header before decoding: a DCT/JPEG stream may declare
            // one size in the PDF and carry a much larger frame, and
            // `load_from_memory` would allocate it in full. Reject the pixel
            // count at the same ceiling as the flate path.
            let (w, h) = image::ImageReader::new(std::io::Cursor::new(data))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()?;
            if w as usize * h as usize > crate::media::MAX_IMAGE_PIXELS {
                return None;
            }
            let img = image::load_from_memory(data).ok()?;
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let has_alpha = rgba.as_raw().chunks_exact(4).any(|p| p[3] != 255);
            Some((rgba.into_raw(), w, h, has_alpha))
        }
        #[cfg(not(feature = "vision"))]
        {
            let _ = data;
            None
        }
    }
}

/// `vision`-only: encode an RGBA buffer as baseline JPEG at `quality`. JPEG has
/// no alpha channel; callers must skip alpha-carrying sources.
#[cfg(feature = "vision")]
pub(super) fn encode_jpeg(rgba: &[u8], w: u32, h: u32, quality: u8) -> Option<Vec<u8>> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for p in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    enc.encode(&rgb, w, h, image::ExtendedColorType::Rgb8).ok()?;
    Some(out)
}

/// Try to shrink an already-encoded raster so its base64 payload fits within
/// `budget` bytes, returning `(encoded_bytes, mime)` for the first candidate
/// that fits, or `None` when even the cheapest candidate is still over budget
/// (the caller then falls back to the omission placeholder).
///
/// Both the source and the returned bytes are copies for the inline markdown
/// only; the caller's `MediaItem`/JSON side-channel keeps the full image.
///
/// Ordering (deterministic, bounded by [`SHRINK_MAX_STEPS`]):
/// 1. `vision` only, opaque sources: JPEG at full resolution over a descending
///    quality ladder 80→60→40. JPEG is tried before any downscaling because it
///    shrinks photos/charts far more than a PNG downscale at equivalent
///    perceptual quality, so a full-size recompress often fits where a
///    legible-but-smaller PNG still would not.
/// 2. Then (and as the only path in the default public/WASM build) progressive
///    PNG downscaling over [`downscale_ladder`], reusing the same
///    dependency-light box-downsample + PNG encoder as the decode path.
/// 3. `vision` only: JPEG at the quality floor combined with the floor
///    downscale step, for sources where compression alone was not enough.
pub(crate) fn shrink_encoded_image_to_fit(
    data: &[u8],
    format: &str,
    budget: usize,
) -> Option<(Vec<u8>, &'static str)> {
    if budget == 0 || data.is_empty() {
        return None;
    }
    let (rgba, w, h, has_alpha) = decode_for_shrink(data, format)?;
    if w == 0 || h == 0 {
        return None;
    }
    let targets = downscale_ladder(w.max(h));
    let mut steps = 0usize;
    // `has_alpha` only gates the `vision`-only JPEG path (JPEG has no alpha).
    let _ = has_alpha;

    #[cfg(feature = "vision")]
    {
        if !has_alpha {
            for q in [80u8, 60, 40] {
                if steps >= SHRINK_MAX_STEPS {
                    break;
                }
                steps += 1;
                if let Some(jpeg) = encode_jpeg(&rgba, w, h, q) {
                    if b64_len(jpeg.len()) <= budget {
                        return Some((jpeg, "image/jpeg"));
                    }
                }
            }
            // A single downscale+JPEG rung at the floor: at a fixed quality a
            // smaller pixel count never produces a larger payload, so if the
            // floor does not fit no larger downscale will either. Reserving
            // the rest of the budget for the PNG fallback keeps enabling
            // `vision` from ever making an image worse than the default build.
            if let Some(&floor) = targets.last() {
                if steps < SHRINK_MAX_STEPS {
                    steps += 1;
                    let (small, sw, sh) = downscale_rgba(&rgba, w, h, floor);
                    if let Some(jpeg) = encode_jpeg(&small, sw, sh, 40) {
                        if b64_len(jpeg.len()) <= budget {
                            return Some((jpeg, "image/jpeg"));
                        }
                    }
                }
            }
        }
    }

    for &t in &targets {
        if steps >= SHRINK_MAX_STEPS {
            break;
        }
        steps += 1;
        let (small, sw, sh) = downscale_rgba(&rgba, w, h, t);
        let png = encode_png_rgba(sw, sh, &small);
        if b64_len(png.len()) <= budget {
            return Some((png, "image/png"));
        }
    }
    None
}
