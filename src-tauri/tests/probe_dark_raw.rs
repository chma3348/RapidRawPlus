// Requires the LaMa weights in the app models dir — validates gamma-space inpainting on deep-shadow linear data.
use image::{DynamicImage, GrayImage, Luma, Rgb32FImage};
use std::path::PathBuf;

#[test]
#[ignore]
fn dark_raw_inpaint() {
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

    // Deep-shadow linear image: noisy values around 1e-4..1e-2 (like a
    // night RAW), with a pure-zero oval defect.
    let (w, h) = (500u32, 400u32);
    let mut img = Rgb32FImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let n = ((x * 7919 + y * 104729) % 97) as f32 / 97.0;
        let v = 0.0002 + 0.004 * n;
        p.0 = [v, v * 0.9, v * 1.1];
    }
    let (cx, cy, rx, ry) = (250f32, 200f32, 70f32, 40f32);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let dx = (x as f32 - cx) / rx;
        let dy = (y as f32 - cy) / ry;
        if dx * dx + dy * dy < 1.0 {
            p.0 = [0.0, 0.0, 0.0];
        }
    }
    let mut mask = GrayImage::new(w, h);
    for (x, y, p) in mask.enumerate_pixels_mut() {
        let dx = (x as f32 - cx) / (rx + 14.0);
        let dy = (y as f32 - cy) / (ry + 14.0);
        if dx * dx + dy * dy < 1.0 {
            *p = Luma([255]);
        }
    }

    let dynamic = DynamicImage::ImageRgb32F(img.clone());
    let (result, is_gamma) =
        rapidraw_lib::ai_processing::run_lama_inpainting(&dynamic, &mask, &session).unwrap();
    assert!(is_gamma, "float source must come back gamma-encoded");

    // Decode fill back to linear and compare against ring level.
    const G: f32 = 2.4;
    let mut fill_sum = 0f64;
    let mut fill_n = 0u64;
    for (x, y, m) in mask.enumerate_pixels() {
        if m[0] > 0 {
            let p = result.get_pixel(x, y);
            let lin = (p[0] as f32 / 255.0).powf(G);
            fill_sum += lin as f64;
            fill_n += 1;
        }
    }
    let fill_mean = fill_sum / fill_n as f64;
    // Ring reference level (linear)
    let mut ring_sum = 0f64;
    let mut ring_n = 0u64;
    for (x, y, p) in img.enumerate_pixels() {
        if mask.get_pixel(x, y)[0] == 0 {
            let dx = (x as f32 - cx) / (rx + 40.0);
            let dy = (y as f32 - cy) / (ry + 40.0);
            if dx * dx + dy * dy < 1.0 {
                ring_sum += p[0] as f64;
                ring_n += 1;
            }
        }
    }
    let ring_mean = ring_sum / ring_n as f64;
    println!("fill mean (linear): {fill_mean:.6}, ring mean (linear): {ring_mean:.6}");
    // Before the fix the fill was exactly 0. Success = fill within 3x of
    // the ring's level (i.e. the black hole now matches its surroundings).
    assert!(
        fill_mean > ring_mean / 3.0 && fill_mean < ring_mean * 3.0,
        "fill {fill_mean} not in range of ring {ring_mean}"
    );
}
