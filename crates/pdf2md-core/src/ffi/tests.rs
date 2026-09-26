// Copyright (c) 2026 Dai Hung PHAM. All rights reserved.
// SPDX-License-Identifier: BSL-1.1
// Licensed under the Business Source License 1.1 (BSL-1.1).

//! C ABI (FFI) exports for embedding in Go (cgo), Python, Node, and other native consumers.

    use super::*;
    use std::ffi::CStr;

    /// Build a one-page PDF with a single uncompressed `DeviceRGB` image
    /// XObject (deterministic pseudo-random noise) painted at 250x200 pt on a
    /// 400x400 page, plus a minimal Helvetica text layer so the synthetic page
    /// is not rejected as a scan. Mirrors `lib.rs`'s
    /// `synthetic_noise_image_pdf` so these FFI tests are self-contained and
    /// never depend on external fixtures.
    fn synthetic_noise_image_pdf(width: u32, height: u32) -> Vec<u8> {
        let mut samples = Vec::with_capacity(width as usize * height as usize * 3);
        let mut state: u32 = 0x1234_5678;
        for _ in 0..(width as usize * height as usize * 3) {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            samples.push((state & 0xff) as u8);
        }

        let mut doc = lopdf::Document::new();
        let mut img_dict = lopdf::Dictionary::new();
        img_dict.set(b"Type", lopdf::Object::Name(b"XObject".to_vec()));
        img_dict.set(b"Subtype", lopdf::Object::Name(b"Image".to_vec()));
        img_dict.set(b"Width", lopdf::Object::Integer(width as i64));
        img_dict.set(b"Height", lopdf::Object::Integer(height as i64));
        img_dict.set(b"ColorSpace", lopdf::Object::Name(b"DeviceRGB".to_vec()));
        img_dict.set(b"BitsPerComponent", lopdf::Object::Integer(8));
        let img_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(img_dict, samples)));

        let mut font_dict = lopdf::Dictionary::new();
        font_dict.set(b"Type", lopdf::Object::Name(b"Font".to_vec()));
        font_dict.set(b"Subtype", lopdf::Object::Name(b"Type1".to_vec()));
        font_dict.set(b"BaseFont", lopdf::Object::Name(b"Helvetica".to_vec()));
        font_dict.set(b"Encoding", lopdf::Object::Name(b"WinAnsiEncoding".to_vec()));
        let font_id = doc.add_object(lopdf::Object::Dictionary(font_dict));

        let content = "BT /F1 12 Tf 60 360 Td (Synthetic sample text for the FFI test) Tj ET\n\
                       q 250 0 0 200 75 100 cm /Im0 Do Q\n";
        let content_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            lopdf::Dictionary::new(),
            content.as_bytes().to_vec(),
        )));

        let mut page_dict = lopdf::Dictionary::new();
        page_dict.set(b"Type", lopdf::Object::Name(b"Page".to_vec()));
        page_dict.set(
            b"MediaBox",
            lopdf::Object::Array(vec![
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(0),
                lopdf::Object::Integer(400),
                lopdf::Object::Integer(400),
            ]),
        );
        let mut xobj = lopdf::Dictionary::new();
        xobj.set(b"Im0", lopdf::Object::Reference(img_id));
        let mut font_res = lopdf::Dictionary::new();
        font_res.set(b"F1", lopdf::Object::Reference(font_id));
        let mut res_dict = lopdf::Dictionary::new();
        res_dict.set(b"XObject", lopdf::Object::Dictionary(xobj));
        res_dict.set(b"Font", lopdf::Object::Dictionary(font_res));
        page_dict.set(b"Resources", lopdf::Object::Dictionary(res_dict));
        page_dict.set(b"Contents", lopdf::Object::Reference(content_id));
        let page_id = doc.add_object(lopdf::Object::Dictionary(page_dict));

        let mut pages_dict = lopdf::Dictionary::new();
        pages_dict.set(b"Type", lopdf::Object::Name(b"Pages".to_vec()));
        pages_dict.set(
            b"Kids",
            lopdf::Object::Array(vec![lopdf::Object::Reference(page_id)]),
        );
        pages_dict.set(b"Count", lopdf::Object::Integer(1));
        let pages_id = doc.add_object(lopdf::Object::Dictionary(pages_dict));

        let mut catalog_dict = lopdf::Dictionary::new();
        catalog_dict.set(b"Type", lopdf::Object::Name(b"Catalog".to_vec()));
        catalog_dict.set(b"Pages", lopdf::Object::Reference(pages_id));
        let catalog_id = doc.add_object(lopdf::Object::Dictionary(catalog_dict));
        doc.trailer.set(b"Root", lopdf::Object::Reference(catalog_id));

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("save synthetic image pdf");
        bytes
    }

    /// Call an FFI entry point, copy its JSON, and free the C string.
    fn call_json(bytes: &[u8], f: impl FnOnce(*const u8, usize) -> *mut c_char) -> String {
        let ptr = f(bytes.as_ptr(), bytes.len());
        assert!(!ptr.is_null(), "FFI entry point returned a null pointer");
        let out = unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .expect("FFI output must be UTF-8")
            .to_owned();
        pdf2md_free_string(ptr);
        out
    }

    /// The parsed JSON with the wall-clock `duration_us` removed: it is
    /// inherently nondeterministic between calls, so behavioral equality of
    /// two conversions is asserted over every field except the timer.
    fn comparable(json: &str) -> serde_json::Value {
        let mut v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        if let Some(obj) = v.as_object_mut() {
            obj.remove("duration_us");
        }
        v
    }

    #[test]
    fn media_mode_none_is_default_and_embed_is_opt_in() {
        let bytes = synthetic_noise_image_pdf(300, 240);

        // Sanity: the fixture really does carry embeddable media, so the
        // "no media by default" result below is meaningful.
        let embed: serde_json::Value =
            serde_json::from_str(&call_json(&bytes, |p, l| pdf2md_convert_ex3(p, l, 0, 2)))
                .unwrap();
        assert!(
            !embed["media"].as_array().unwrap().is_empty(),
            "fixture must contain at least one media item; got {embed}"
        );
        assert!(
            embed["markdown"].as_str().unwrap().contains("data:image"),
            "explicit embed must inline a data-URI image; got {embed}"
        );

        // New default: no media extracted and no data-URI inlined.
        let default_call: serde_json::Value =
            serde_json::from_str(&call_json(&bytes, |p, l| pdf2md_convert(p, l))).unwrap();
        assert!(
            default_call["media"].as_array().unwrap().is_empty(),
            "default must not extract media; got {default_call}"
        );
        assert!(
            !default_call["markdown"].as_str().unwrap().contains("data:image"),
            "default must not inline a data:image; got {default_call}"
        );

        // `reference`: extract to the JSON side-channel but never inline.
        let reference: serde_json::Value =
            serde_json::from_str(&call_json(&bytes, |p, l| pdf2md_convert_ex3(p, l, 0, 1)))
                .unwrap();
        assert!(
            !reference["media"].as_array().unwrap().is_empty(),
            "reference mode must extract media; got {reference}"
        );
        assert!(
            !reference["markdown"].as_str().unwrap().contains("data:image"),
            "reference mode must not inline a data:image; got {reference}"
        );

        // `no_media = 1` is the compatibility alias for `none`.
        let no_media: serde_json::Value =
            serde_json::from_str(&call_json(&bytes, |p, l| pdf2md_convert_ex2(p, l, 0, 1))).unwrap();
        assert!(
            no_media["media"].as_array().unwrap().is_empty(),
            "no_media=1 must yield an empty media array; got {no_media}"
        );
        assert!(
            !no_media["markdown"].as_str().unwrap().contains("data:image"),
            "no_media=1 markdown must contain no data:image; got {no_media}"
        );
    }

    #[test]
    fn ex2_default_matches_existing_convert_entry_points() {
        let bytes = synthetic_noise_image_pdf(300, 240);

        let legacy = call_json(&bytes, |p, l| pdf2md_convert(p, l));
        let legacy_ex = call_json(&bytes, |p, l| pdf2md_convert_ex(p, l, 0));
        let new_default = call_json(&bytes, |p, l| pdf2md_convert_ex2(p, l, 0, 0));
        // `ex4` with `page_markers = 0` must stay on the legacy path: this is
        // the same equivalence pattern extended to the newest entry point.
        let ex4_default = call_json(&bytes, |p, l| pdf2md_convert_ex4(p, l, 0, 0, 0));

        assert_eq!(
            comparable(&new_default),
            comparable(&legacy),
            "pdf2md_convert_ex2(..., 0, 0) must match pdf2md_convert on every field except duration_us"
        );
        assert_eq!(
            comparable(&new_default),
            comparable(&legacy_ex),
            "pdf2md_convert_ex2(..., 0, 0) must match pdf2md_convert_ex(..., 0) on every field except duration_us"
        );
        assert_eq!(
            comparable(&legacy),
            comparable(&legacy_ex),
            "pdf2md_convert and pdf2md_convert_ex(..., 0) must agree as before"
        );
        assert_eq!(
            comparable(&ex4_default),
            comparable(&legacy),
            "pdf2md_convert_ex4(..., 0, 0, 0) must match pdf2md_convert on every field except duration_us"
        );
        assert_eq!(
            comparable(&ex4_default),
            comparable(&new_default),
            "pdf2md_convert_ex4(..., 0, 0, 0) must match pdf2md_convert_ex2(..., 0, 0) on every field except duration_us"
        );
    }

    /// `ex4` with `page_markers = 1` opts in: the marker appears exactly once
    /// for the single synthetic page, and the legacy entry points stay marker-
    /// free so the disabled default cannot regress.
    #[test]
    fn ex4_page_markers_opt_in_inserts_exactly_one_marker() {
        let bytes = synthetic_noise_image_pdf(300, 240);

        let on: serde_json::Value =
            serde_json::from_str(&call_json(&bytes, |p, l| pdf2md_convert_ex4(p, l, 0, 0, 1)))
                .unwrap();
        let md = on["markdown"].as_str().unwrap();
        assert_eq!(
            md.matches("<!-- pdf2w:page n=\"").count(),
            1,
            "page_markers=1 must emit exactly one marker for a one-page doc: {md}"
        );
        assert!(
            md.contains("<!-- pdf2w:page n=\"1\" -->"),
            "page_markers=1 must emit the 1-indexed marker: {md}"
        );

        // The frozen contract: `ex4(..., 0)` (and every legacy entry point) is
        // byte-identical to the marker-free conversion.
        let off = call_json(&bytes, |p, l| pdf2md_convert_ex4(p, l, 0, 0, 0));
        assert!(
            !off.contains("pdf2w:page"),
            "page_markers=0 must not emit a marker: {off}"
        );
    }

    #[test]
    fn ex2_no_media_off_is_byte_identical_to_legacy_convert() {
        let bytes = synthetic_noise_image_pdf(300, 240);

        let legacy = call_json(&bytes, |p, l| pdf2md_convert(p, l));
        let new_default = call_json(&bytes, |p, l| pdf2md_convert_ex2(p, l, 0, 0));

        // `duration_us` is a wall-clock timer and cannot be equal run to run,
        // so the byte-identity proof is taken over every other field: the
        // canonical re-serialization of the parsed JSON (with `duration_us`
        // removed) is exactly equal, which is the strongest deterministic
        // equivalence available.
        assert_eq!(
            comparable(&new_default).to_string(),
            comparable(&legacy).to_string(),
            "default-off path must be byte-identical to pdf2md_convert except for duration_us"
        );
    }

    /// `ex3(..., media_mode = 0)` is `MediaMode::None`, so it must agree on
    /// every deterministic field with the `ex2` compatibility alias for
    /// "no media" (`no_media = 1`).
    #[test]
    fn ex3_none_mode_matches_ex2_no_media_alias() {
        let bytes = synthetic_noise_image_pdf(300, 240);

        let ex3_none = call_json(&bytes, |p, l| pdf2md_convert_ex3(p, l, 0, 0));
        let ex2_no_media = call_json(&bytes, |p, l| pdf2md_convert_ex2(p, l, 0, 1));

        assert!(
            serde_json::from_str::<serde_json::Value>(&ex3_none)
                .unwrap()["media"]
                .as_array()
                .unwrap()
                .is_empty(),
            "ex3 none mode must leave the media array empty: {ex3_none}"
        );
        assert_eq!(
            comparable(&ex3_none),
            comparable(&ex2_no_media),
            "pdf2md_convert_ex3(..., 0, 0) must match pdf2md_convert_ex2(..., 0, 1) on every field except duration_us"
        );
    }

    /// `ex3(..., media_mode = 2)` is `MediaMode::Embed`. The crate's own
    /// `ConversionOptions::default()` is now `MediaMode::None` (see
    /// `models.rs`), so the matching in-crate reference path is an explicit
    /// `MediaMode::Embed` conversion, not `pdf2md_convert` / `ex2(..., 0, 0)`.
    /// Assert the deterministic markdown and media side-channel agree with it.
    #[test]
    fn ex3_embed_matches_explicit_embed_options() {
        let bytes = synthetic_noise_image_pdf(300, 240);

        let ffi: serde_json::Value =
            serde_json::from_str(&call_json(&bytes, |p, l| pdf2md_convert_ex3(p, l, 0, 2)))
                .unwrap();

        let mut opts = ConversionOptions::default();
        opts.media_mode = MediaMode::Embed;
        let direct = convert_pdf_bytes_to_markdown(&bytes, &opts).expect("direct embed conversion");

        assert!(!direct.media.is_empty(), "embed fixture must carry media");
        assert_eq!(
            ffi["markdown"].as_str().unwrap(),
            direct.markdown,
            "ex3 embed markdown must match an explicit MediaMode::Embed conversion"
        );
        assert_eq!(
            ffi["media"].as_array().unwrap().len(),
            direct.media.len(),
            "ex3 embed media count must match an explicit MediaMode::Embed conversion"
        );
        assert!(
            ffi["markdown"].as_str().unwrap().contains("data:image"),
            "ex3 embed must inline a data-URI image: {ffi}"
        );
    }

    /// `1 = reference` and any unrecognized value use the safe middle policy:
    /// extract the media side-channel but never inline a `data:` URI.
    #[test]
    fn ex3_reference_and_unknown_modes_never_inline() {
        let bytes = synthetic_noise_image_pdf(300, 240);

        for mode in [1, 3] {
            let v: serde_json::Value =
                serde_json::from_str(&call_json(&bytes, |p, l| pdf2md_convert_ex3(p, l, 0, mode)))
                    .unwrap();
            assert!(
                !v["media"].as_array().unwrap().is_empty(),
                "ex3 mode {mode} must extract the media side-channel; got {v}"
            );
            assert!(
                !v["markdown"].as_str().unwrap().contains("data:image"),
                "ex3 mode {mode} must not inline a data:image; got {v}"
            );
        }
    }