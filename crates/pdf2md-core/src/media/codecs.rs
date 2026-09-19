// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Media codecs: Base64, Flate/Deflate, ASCIIHex, ASCII85, RunLength, and PNG encoder.

use std::io::{Read, Write};

/// Standard base64 encoder (RFC 4648) — no dependency needed.
pub fn b64encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Flate-decode `data`, rejecting output larger than `max_output` bytes.
///
/// The decoder reads at most `max_output + 1` bytes, so a small compressed
/// input that expands without bound (a decompression bomb) is rejected without
/// ever allocating the full output. Callers pass the sample size they are
/// willing to interpret, never `usize::MAX`.
pub(crate) fn inflate(data: &[u8], max_output: usize) -> Option<Vec<u8>> {
    let mut d = flate2::read::ZlibDecoder::new(data).take(max_output as u64 + 1);
    let mut out = Vec::new();
    d.read_to_end(&mut out).ok()?;
    (out.len() <= max_output).then_some(out)
}

pub(crate) fn decode_asciihex(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &c in data {
        if c == b'>' {
            break;
        }
        let l = c.to_ascii_lowercase();
        let v = match l {
            b'0'..=b'9' => l - b'0',
            b'a'..=b'f' => l - b'a' + 10,
            _ => continue,
        };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push((h << 4) | v),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    Some(out)
}

pub(crate) fn decode_ascii85(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut group: u32 = 0;
    let mut count = 0usize;
    for &c in data {
        if c == b'~' {
            break;
        }
        if c == b'z' && count == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&c) {
            continue;
        }
        group = group * 85 + (c - b'!') as u32;
        count += 1;
        if count == 5 {
            out.extend_from_slice(&group.to_be_bytes());
            group = 0;
            count = 0;
        }
    }
    if count == 1 {
        group = group.wrapping_mul(85u32.pow(4));
        out.extend_from_slice(&group.to_be_bytes()[..1]);
    } else if count == 2 {
        group = group.wrapping_mul(85u32.pow(3));
        out.extend_from_slice(&group.to_be_bytes()[..2]);
    } else if count == 3 {
        group = group.wrapping_mul(85u32.pow(2));
        out.extend_from_slice(&group.to_be_bytes()[..3]);
    } else if count == 4 {
        group = group.wrapping_mul(85);
        out.extend_from_slice(&group.to_be_bytes()[..4]);
    }
    Some(out)
}

pub(crate) fn decode_runlength(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let c = data[i] as i8;
        i += 1;
        if c == -128 {
            break;
        }
        if c >= 0 {
            let len = c as usize + 1;
            if i + len > data.len() {
                return None;
            }
            out.extend_from_slice(&data[i..i + len]);
            i += len;
        } else {
            let len = (-(c as i16)) as usize + 1;
            if i >= data.len() {
                return None;
            }
            out.extend(std::iter::repeat(data[i]).take(len));
            i += 1;
        }
    }
    Some(out)
}

fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut c = Vec::with_capacity(kind.len() + data.len());
    c.extend_from_slice(kind);
    c.extend_from_slice(data);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

/// Encode RGBA8 pixels as PNG.
pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6); // RGBA
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    chunk(&mut out, b"IHDR", &ihdr);

    let row_len = width as usize * 4;
    let mut raw = Vec::with_capacity((row_len + 1) * height as usize);
    for row in rgba.chunks_exact(row_len) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = enc.write_all(&raw);
    let compressed = enc.finish().unwrap_or_default();
    chunk(&mut out, b"IDAT", &compressed);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Decode an 8-bit, non-interlaced RGBA PNG (color type 6) into a tightly
/// packed RGBA8 buffer, together with its pixel width/height.
///
/// This is deliberately the minimal inverse of [`encode_png_rgba`] — the exact
/// stream layout this crate emits for reconstructed rasters — so the adaptive
/// embed-resize path can re-downsample an already-encoded image without linking
/// a PNG codec in the default build. Any other bit depth, color type, or
/// interlace mode returns `None` rather than guessing. All five PNG scanline
/// filters are handled so the decoder is not silently tied to the filter-0
/// layout of our own encoder.
pub(crate) fn decode_png_rgba(data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 8 || &data[..8] != SIG {
        return None;
    }
    let mut pos = 8usize;
    let (mut width, mut height) = (0u32, 0u32);
    let (mut bit_depth, mut color_type, mut interlace) = (0u8, 0u8, 0u8);
    let mut seen_ihdr = false;
    let mut idat: Vec<u8> = Vec::new();
    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
            as usize;
        let kind = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        let body_end = body_start.checked_add(len)?;
        if body_end + 4 > data.len() {
            return None;
        }
        match kind {
            b"IHDR" => {
                if len < 13 {
                    return None;
                }
                let b = &data[body_start..body_start + 13];
                width = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                height = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
                bit_depth = b[8];
                color_type = b[9];
                interlace = b[12];
                seen_ihdr = true;
            }
            b"IDAT" => idat.extend_from_slice(&data[body_start..body_end]),
            b"IEND" => break,
            _ => {}
        }
        pos = body_end + 4; // data + 4-byte CRC
    }
    if !seen_ihdr || width == 0 || height == 0 {
        return None;
    }
    if bit_depth != 8 || color_type != 6 || interlace != 0 {
        return None;
    }
    if width as usize * height as usize > super::MAX_IMAGE_PIXELS {
        return None;
    }
    let stride = width as usize * 4;
    let expected = (stride + 1).checked_mul(height as usize)?;
    let raw = inflate(&idat, expected)?;
    if raw.len() < expected {
        return None;
    }
    let mut out = vec![0u8; stride * height as usize];
    let mut prev = vec![0u8; stride];
    for y in 0..height as usize {
        let row = &raw[y * (stride + 1)..];
        let filter = row[0];
        let line = &row[1..1 + stride];
        let cur = &mut out[y * stride..(y + 1) * stride];
        for x in 0..stride {
            let a = if x >= 4 { cur[x - 4] } else { 0 };
            let b = prev[x];
            let c = if x >= 4 { prev[x - 4] } else { 0 };
            cur[x] = match filter {
                0 => line[x],
                1 => line[x].wrapping_add(a),
                2 => line[x].wrapping_add(b),
                3 => line[x].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => {
                    let p = a as i16 + b as i16 - c as i16;
                    let pa = (p - a as i16).abs();
                    let pb = (p - b as i16).abs();
                    let pc = (p - c as i16).abs();
                    let pred = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    line[x].wrapping_add(pred)
                }
                _ => return None,
            };
        }
        prev.copy_from_slice(cur);
    }
    Some((out, width, height))
}
