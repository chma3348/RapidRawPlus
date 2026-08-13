//! HEIC decode through the real loader path (macOS-only: uses sips).

#[cfg(target_os = "macos")]
#[test]
fn heic_decodes_through_loader() {
    use image::{Rgb, RgbImage};

    let dir = std::env::temp_dir();
    let png = dir.join("rapidraw_heic_test_src.png");
    let heic = dir.join("rapidraw_heic_test.heic");

    let mut img = RgbImage::new(120, 60);
    for p in img.pixels_mut() {
        *p = Rgb([200, 40, 40]);
    }
    img.save(&png).unwrap();

    let status = std::process::Command::new("sips")
        .args(["-s", "format", "heic"])
        .arg(&png)
        .arg("--out")
        .arg(&heic)
        .output()
        .expect("sips should exist on macOS");
    assert!(status.status.success(), "sips heic encode failed");

    let bytes = std::fs::read(&heic).unwrap();
    let decoded = rapidraw_lib::image_loader::load_image_with_orientation(&bytes, None)
        .expect("HEIC should decode through the loader");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (120, 60),
        "decoded dimensions should match the source"
    );

    // The decoded pixels should be (approximately) the source color.
    let rgb = decoded.to_rgb8();
    let p = rgb.get_pixel(60, 30);
    assert!(
        (p[0] as i32 - 200).abs() < 20 && (p[1] as i32 - 40).abs() < 20,
        "decoded color should approximate the source, got {:?}",
        p
    );

    let _ = std::fs::remove_file(&png);
    let _ = std::fs::remove_file(&heic);
}
