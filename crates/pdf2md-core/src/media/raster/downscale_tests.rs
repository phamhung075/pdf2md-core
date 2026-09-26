// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Raster image extraction from PDF content streams and XObject resources.

    use super::downscale_rgba;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..(width as usize * height as usize) {
            out.extend_from_slice(&rgba);
        }
        out
    }

    #[test]
    fn noop_when_already_within_bounds() {
        let buf = solid(100, 50, [10, 20, 30, 255]);
        let (out, w, h) = downscale_rgba(&buf, 100, 50, 1536);
        assert_eq!((w, h), (100, 50));
        assert_eq!(out, buf);
    }

    #[test]
    fn noop_when_disabled_via_zero() {
        let buf = solid(3000, 3000, [1, 2, 3, 4]);
        let (out, w, h) = downscale_rgba(&buf, 3000, 3000, 0);
        assert_eq!((w, h), (3000, 3000));
        assert_eq!(out.len(), buf.len());
    }

    #[test]
    fn caps_longest_side_and_preserves_aspect_ratio() {
        let buf = solid(3000, 1500, [7, 7, 7, 255]);
        let (out, w, h) = downscale_rgba(&buf, 3000, 1500, 1500);
        assert_eq!((w, h), (1500, 750), "aspect ratio must be preserved");
        assert_eq!(out.len(), (w as usize) * (h as usize) * 4);
    }

    #[test]
    fn averages_a_uniform_color_block_exactly() {
        // A uniform-color source must downscale to the exact same color —
        // any averaging bug would show up as drift here.
        let buf = solid(8, 8, [200, 100, 50, 255]);
        let (out, w, h) = downscale_rgba(&buf, 8, 8, 4);
        assert_eq!((w, h), (4, 4));
        for px in out.chunks_exact(4) {
            assert_eq!(px, [200, 100, 50, 255]);
        }
    }

    #[test]
    fn averages_two_tone_halves_to_the_midpoint() {
        // Left half black, right half white; downscaling to 1 column must
        // average the whole row, landing on the midpoint gray.
        let mut buf = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 0 } else { 255 };
                buf.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let (out, w, h) = downscale_rgba(&buf, 4, 2, 1);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![127, 127, 127, 255]);
    }