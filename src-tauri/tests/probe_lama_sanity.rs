// Requires the LaMa weights in the app models dir — is run_lama_inpainting functional at all, and is mask polarity right?
use image::{DynamicImage, GrayImage, Luma, Rgb, RgbImage};
use std::path::PathBuf;

fn test_image() -> (DynamicImage, GrayImage) {
    // Bright gradient with a black oval in the middle.
    let (w, h) = (400u32, 300u32);
    let mut img = RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = Rgb([(80 + x / 4) as u8, (100 + y / 4) as u8, 160]);
    }
    let (cx, cy, rx, ry) = (200f32, 150f32, 60f32, 35f32);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let dx = (x as f32 - cx) / rx;
        let dy = (y as f32 - cy) / ry;
        if dx * dx + dy * dy < 1.0 {
            *p = Rgb([5, 5, 5]);
        }
    }
    let mut mask = GrayImage::new(w, h);
    for (x, y, p) in mask.enumerate_pixels_mut() {
        let dx = (x as f32 - cx) / (rx + 12.0);
        let dy = (y as f32 - cy) / (ry + 12.0);
        if dx * dx + dy * dy < 1.0 {
            *p = Luma([255]);
        }
    }
    (DynamicImage::ImageRgb8(img), mask)
}

#[test]
#[ignore]
fn lama_sanity() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    unsafe {
        std::env::set_var(
            "ORT_DYLIB_PATH",
            manifest_dir.join("resources/libonnxruntime.dylib"),
        )
    };
    rapidraw_lib::register_exit_handler();
    let models_dir = std::env::var("HOME").unwrap()
        + "/Library/Application Support/io.github.CyberTimon.RapidRAW/models";
    if !PathBuf::from(&models_dir).join("lama_fp16.onnx").is_file() {
        eprintln!("lama_fp16.onnx not present; skipping");
        return;
    }
    let registry = rapidraw_lib::model_registry::ModelRegistry::new(PathBuf::from(models_dir));
    let session = registry.get_session("lama-fp16", None).unwrap();

    let (img, mask) = test_image();
    let (result, _) = rapidraw_lib::ai_processing::run_lama_inpainting(&img, &mask, &session).unwrap();
    // Center of the oval should no longer be black if the fill worked.
    let center = result.get_pixel(200, 150);
    let src = img.to_rgba8();
    let (mut changed, mut total) = (0u64, 0u64);
    for (x, y, m) in mask.enumerate_pixels() {
        if m[0] > 0 {
            total += 1;
            let a = src.get_pixel(x, y);
            let b = result.get_pixel(x, y);
            if (0..3).any(|c| a[c].abs_diff(b[c]) > 10) {
                changed += 1;
            }
        }
    }
    println!("NORMAL mask: center px after = {:?}, changed {changed}/{total}", center);

    // Inverted polarity probe
    let mut inv = mask.clone();
    for p in inv.pixels_mut() {
        p[0] = 255 - p[0];
    }
    let (result_inv, _) = rapidraw_lib::ai_processing::run_lama_inpainting(&img, &inv, &session).unwrap();
    let center_inv = result_inv.get_pixel(200, 150);
    println!("INVERTED mask: center px after = {:?}", center_inv);
}
