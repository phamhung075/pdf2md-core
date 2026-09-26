// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! Raster image extraction from PDF content streams and XObject resources.

    use super::*;

    /// Bug 4 regression: an attention-map figure (1536x242 px, aspect ~6.34)
    /// spanning the page width is a diagram, not a barcode.
    #[test]
    fn wide_page_spanning_raster_is_diagram_not_barcode() {
        let page = Some((0.0, 0.0, 612.0, 792.0));
        // 507 x 80 pt -> aspect 6.34, 82.8% of the page width, area 40_560.
        let (kind, decorative) = classify_geometry(50.0, 400.0, 557.0, 480.0, page);
        assert_eq!(
            kind,
            MediaKind::Diagram,
            "wide page-spanning figure must classify as Diagram, got {:?}",
            kind
        );
        assert!(!decorative, "a content figure must not be decorative");
    }

    /// A genuinely small, narrow barcode scan is still a barcode.
    #[test]
    fn small_narrow_scan_is_still_barcode() {
        let page = Some((0.0, 0.0, 612.0, 792.0));
        // 120 x 20 pt -> aspect 6.0, 19.6% of the page width, area 2_400.
        let (kind, _) = classify_geometry(40.0, 700.0, 160.0, 720.0, page);
        assert_eq!(kind, MediaKind::Barcode);
    }