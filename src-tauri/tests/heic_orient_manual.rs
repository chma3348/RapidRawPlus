/// Gated check with a pre-made orientation-6 HEIC (120x60 stored → 60x120
/// displayed). Set RAPIDRAW_HEIC_ORIENT_FIXTURE to the fixture path to run.
#[cfg(target_os = "macos")]
#[test]
fn oriented_heic_is_display_oriented_once() {
    let p = std::env::var("RAPIDRAW_HEIC_ORIENT_FIXTURE").ok();
    let Some(p) = p else { return };
    let bytes = std::fs::read(&p).unwrap();
    let img = rapidraw_lib::image_loader::load_image_with_orientation(&bytes, None).unwrap();
    assert_eq!(
        (img.width(), img.height()),
        (60, 120),
        "should be rotated exactly once"
    );
}
