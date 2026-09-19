//! Manual diagnosis of sky recognition on a saved replacement input.
use image::{GenericImageView, imageops::FilterType};
use std::path::PathBuf;

#[test]
#[ignore]
fn inspect_sky_recognition() {
    let input = PathBuf::from(std::env::var("REPLACE_PROBE_INPUT").expect("input directory"));
    let output = PathBuf::from(std::env::var("REPLACE_PROBE_OUTPUT").expect("output directory"));
    unsafe {
        std::env::set_var(
            "ORT_DYLIB_PATH",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/libonnxruntime.dylib"),
        );
    }
    rapidraw_lib::register_exit_handler();
    let models = PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/io.github.CyberTimon.RapidRAW/models");
    let registry = rapidraw_lib::model_registry::ModelRegistry::new(models);
    let session = registry.get_session("u2net-sky", None).unwrap();
    {
        let guard = session.lock().unwrap();
        println!(
            "sky model inputs: {:?}, outputs: {:?}",
            guard.inputs, guard.outputs
        );
    }
    let photo = image::open(input.join("replace-context.png")).unwrap();
    let mask = image::open(input.join("replace-model-mask.png"))
        .unwrap()
        .to_luma8();
    std::fs::create_dir_all(&output).unwrap();
    let mut bounds = (photo.width(), photo.height(), 0, 0);
    for (x, y, p) in mask.enumerate_pixels() {
        if p[0] > 127 {
            bounds.0 = bounds.0.min(x);
            bounds.1 = bounds.1.min(y);
            bounds.2 = bounds.2.max(x);
            bounds.3 = bounds.3.max(y);
        }
    }
    for (label, pad) in [
        ("full", u32::MAX),
        ("local", 180),
        ("square", u32::MAX),
        ("local-square", 180),
    ] {
        let (x, y) = (bounds.0.saturating_sub(pad), bounds.1.saturating_sub(pad));
        let right = bounds.2.saturating_add(pad).min(photo.width() - 1);
        let bottom = bounds.3.saturating_add(pad).min(photo.height() - 1);
        let crop = photo.crop_imm(x, y, right - x + 1, bottom - y + 1);
        let model_input = if label.contains("square") {
            crop.resize_exact(320, 320, FilterType::Triangle)
        } else {
            crop.clone()
        };
        let sky = rapidraw_lib::ai_processing::run_sky_seg_model(&model_input, &session).unwrap();
        let sky = image::imageops::resize(&sky, crop.width(), crop.height(), FilterType::Triangle);
        let mut selected = 0u64;
        let mut confident = 0u64;
        let mut membership = 0u64;
        for (cx, cy, p) in sky.enumerate_pixels() {
            if mask.get_pixel(cx + x, cy + y)[0] > 127 {
                selected += 1;
                confident += u64::from(p[0] > 191);
                membership += u64::from(p[0] > 127);
            }
        }
        println!(
            "{label}: confident sky overlap {confident}/{selected} = {:.3}",
            confident as f64 / selected as f64
        );
        println!(
            "{label}: sky membership {membership}/{selected} = {:.3}",
            membership as f64 / selected as f64
        );
        sky.save(output.join(format!("{label}-sky.png"))).unwrap();
        if label == "square" {
            let candidate = image::GrayImage::from_fn(mask.width(), mask.height(), |x, y| {
                image::Luma([
                    if mask.get_pixel(x, y)[0] > 127 || sky.get_pixel(x, y)[0] > 127 {
                        255
                    } else {
                        0
                    },
                ])
            });
            candidate
                .save(output.join("blob-00-engine-mask.png"))
                .unwrap();
            photo.save(output.join("blob-00-engine-input.png")).unwrap();
        }
        if label == "local-square" {
            let candidate = image::GrayImage::from_fn(crop.width(), crop.height(), |cx, cy| {
                image::Luma([
                    if mask.get_pixel(cx + x, cy + y)[0] > 127 || sky.get_pixel(cx, cy)[0] > 32 {
                        255
                    } else {
                        0
                    },
                ])
            });
            let local = output.join("local");
            std::fs::create_dir_all(&local).unwrap();
            let width = (crop.width() / 8) * 8;
            let height = (crop.height() / 8) * 8;
            image::imageops::resize(&candidate, width, height, FilterType::Nearest)
                .save(local.join("blob-00-engine-mask.png"))
                .unwrap();
            crop.resize_exact(width, height, FilterType::Lanczos3)
                .save(local.join("blob-00-engine-input.png"))
                .unwrap();
            if let Ok(generated_path) = std::env::var("REPLACE_PROBE_GENERATED") {
                let generated = image::open(generated_path)
                    .unwrap()
                    .resize_exact(crop.width(), crop.height(), FilterType::Lanczos3)
                    .to_rgba8();
                let mut full = photo.to_rgba8();
                image::imageops::replace(&mut full, &generated, x as i64, y as i64);
                let result =
                    rapidraw_lib::heal_blend::blend_generated(&photo.to_rgba8(), &full, &mask, 8.0);
                result.save(output.join("local-composite.png")).unwrap();
                result
                    .view(
                        bounds.0,
                        bounds.1,
                        bounds.2 - bounds.0 + 1,
                        bounds.3 - bounds.1 + 1,
                    )
                    .to_image()
                    .save(output.join("selected-result.png"))
                    .unwrap();
            }
        }
        crop.resize(1024, 1024, FilterType::Triangle)
            .save(output.join(format!("{label}-photo.png")))
            .unwrap();
    }
}
