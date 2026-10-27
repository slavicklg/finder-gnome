// SPDX-FileCopyrightText: 2026 Slavick
// SPDX-License-Identifier: MIT
use super::*;

#[test]
#[ignore = "internal bounded SVG renderer subprocess; invoked by SVG tests"]
fn helper_process() {
    if std::env::var("FINDER_GNOME_SVG_HELPER").as_deref() == Ok("1") {
        std::process::exit(helper_main());
    }
}
fn image(attributes: &str) -> Vec<u8> {
    format!(r##"<svg xmlns="http://www.w3.org/2000/svg" {attributes}><rect width="100%" height="100%" fill="#ff0000" fill-opacity="0.5"/></svg>"##).into_bytes()
}
fn size(attributes: &str) -> (i32, i32) {
    let raster = render(image(attributes), 0, &gio::Cancellable::new()).unwrap();
    assert_eq!(
        raster.pixels.len(),
        raster.width as usize * raster.height as usize * 4
    );
    assert!(
        raster.pixels.chunks_exact(4).any(|pixel| pixel[0] >= 250
            && pixel[1] == 0
            && pixel[2] == 0
            && (120..=136).contains(&pixel[3])),
        "Correct straight-alpha RGBA output"
    );
    (raster.width, raster.height)
}
#[test]
fn normal_svg_units_viewbox_and_relative_sizes() {
    assert_eq!(size(r#"width="80" height="40""#), (80, 40));
    assert_eq!(size(r#"width="1in" height="0.5in""#), (96, 48));
    assert_eq!(size(r#"width="2.54cm" height="12.7mm""#), (96, 48));
    assert_eq!(size(r#"width="72pt" height="3pc""#), (96, 48));
    assert_eq!(
        size(r#"style="font-size: 10px" width="2em" height="1em""#),
        (20, 10)
    );
    assert_eq!(size(r#"width="3.1px" height="2.1px""#), (4, 3));
    let (width, height) = size(r#"style="font-size: 20px" width="2ex" height="1ex""#);
    assert!(width > 0 && height > 0 && (width - 2 * height).abs() <= 1);
    assert_eq!(size(r#"viewBox="0 0 80 40""#), (80, 40));
    assert_eq!(
        size(r#"width="100%" height="100%" viewBox="-10, -20, 80, 40""#),
        (80, 40)
    );
    assert_eq!(
        size(r#"width="50%" height="75%" viewBox="0 0 80 40""#),
        (80, 40)
    );
    assert_eq!(size(r#"width="200" viewBox="0 0 80 40""#), (200, 100));
    assert_eq!(size(r#"height="100" viewBox="0 0 80 40""#), (200, 100));
    assert_eq!(
        size(r#"style="font-size: 10px" width="2em" viewBox="0 0 80 40""#),
        (20, 10)
    );
    assert_eq!(size(""), (300, 150));
    assert_eq!(size(r#"width="100%" height="100%""#), (300, 150));
}
#[test]
fn oversized_svg_is_rejected_even_for_small_thumbnails() {
    for attributes in [
        r#"width="100000" height="100000""#,
        r#"width="1000in" height="1000in""#,
        r#"viewBox="0 0 100000 100000""#,
        r#"width="100%" height="100%" viewBox="0 0 100000 100000""#,
        r#"style="width:100000px;height:100000px""#,
        r#"width="8000.01" height="5000""#,
        r#"width="1e308" height="1e308""#,
    ] {
        let result = render(image(attributes), 16, &gio::Cancellable::new());
        assert!(
            result.is_err(),
            "Expected oversized SVG rejection: {attributes}"
        );
        let error = result.unwrap_err();
        assert!(error.contains("40 megapixel"), "{attributes}: {error}");
    }
    // Exactly 40 MP is allowed; a large coordinate viewBox does not enlarge a
    // document whose absolute intrinsic size is small.
    assert!(
        render(
            image(r#"width="8000" height="5000""#),
            16,
            &gio::Cancellable::new()
        )
        .is_ok()
    );
    assert_eq!(
        size(r#"width="80" height="40" viewBox="0 0 100000 100000""#),
        (80, 40)
    );
}
#[test]
fn malformed_svg_is_rejected_without_decoding() {
    for bytes in [
        b"not XML".as_slice(),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><g></svg>",
        b"<html/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"NaN\" height=\"10\"/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"-1\" height=\"10\"/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"0\" height=\"10\"/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 -10 20\"/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 NaN 20\"/>",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"1 2 3\"/>",
        b"<!DOCTYPE svg [<!ENTITY attack SYSTEM 'file:///etc/passwd'>]><svg xmlns=\"http://www.w3.org/2000/svg\">&attack;</svg>",
    ] {
        assert!(render(bytes.to_vec(), 32, &gio::Cancellable::new()).is_err());
    }
    let nested = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\">{}{}</svg>",
        "<g>".repeat(256),
        "</g>".repeat(256)
    );
    assert!(render(nested.into_bytes(), 32, &gio::Cancellable::new()).is_err());
}
#[test]
fn svg_input_limit_cancellation_and_disguised_svg() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("misnamed.png");
    std::fs::write(&path, image(r#"width="100000" height="100000""#)).unwrap();
    let file = gio::File::for_path(&path);
    let cancel = gio::Cancellable::new();
    assert!(is_svg(&file, "image/png", &cancel).unwrap());
    assert!(
        load(&file, 32, &cancel)
            .unwrap_err()
            .contains("40 megapixel")
    );
    std::fs::write(&path, vec![b' '; INPUT_LIMIT + 1]).unwrap();
    assert!(load(&file, 32, &cancel).unwrap_err().contains("4 MiB"));
    cancel.cancel();
    assert!(render(image(r#"width="10" height="10""#), 32, &cancel).is_err());
}

#[test]
fn stuck_renderer_process_group_is_terminated_on_timeout_and_cancel() {
    let command = || {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60"]);
        command
    };
    let started = Instant::now();
    assert!(
        run_worker(
            command(),
            Vec::new(),
            16,
            &gio::Cancellable::new(),
            Duration::from_millis(100)
        )
        .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let cancel = gio::Cancellable::new();
    let token = cancel.clone();
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        token.cancel();
    });
    let started = Instant::now();
    let error = run_worker(command(), Vec::new(), 16, &cancel, DEADLINE).unwrap_err();
    trigger.join().unwrap();
    assert!(error.contains("cancelled"));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn external_images_are_not_loaded_and_xml_complexity_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("red.ppm");
    std::fs::write(&image, b"P3\n1 1\n255\n255 0 0\n").unwrap();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><image href="{}" width="16" height="16"/></svg>"#,
        gio::File::for_path(image).uri()
    );
    let raster = render(svg.into_bytes(), 16, &gio::Cancellable::new()).unwrap();
    assert!(raster.pixels.chunks_exact(4).all(|pixel| pixel[3] == 0));
    let repeated = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">{}</svg>"#,
        "<g/>".repeat(100_001)
    );
    assert!(render(repeated.into_bytes(), 16, &gio::Cancellable::new()).is_err());
}

#[test]
fn utf16_svg_dimensions_are_validated_before_rendering() {
    for oversized in [false, true] {
        let svg = String::from_utf8(image(if oversized {
            r#"width="100000" height="100000""#
        } else {
            r#"width="16" height="8""#
        }))
        .unwrap();
        for little in [false, true] {
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in svg.encode_utf16() {
                bytes.extend_from_slice(&if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            let result = render(bytes, 16, &gio::Cancellable::new());
            if oversized {
                assert!(result.unwrap_err().contains("40 megapixel"));
            } else {
                assert_eq!(result.unwrap().width, 16);
            }
        }
    }
}
