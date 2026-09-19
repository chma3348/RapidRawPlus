//! Opt-in regression on the user's backed-up successful sky, never live edits.
use base64::Engine;
use image::imageops::{self, FilterType};
use rapidraw_lib::replacement_blend::{BlendOptions, blend};

#[test]
#[ignore = "requires local checkpoint fixture"]
fn saved_clouds_blend_without_regeneration() {
    let fixture =
        std::path::PathBuf::from(std::env::var("REPLACEMENT_FIXTURE").expect("fixture directory"));
    let output =
        std::path::PathBuf::from(std::env::var("REPLACEMENT_PREVIEWS").expect("preview directory"));
    std::fs::create_dir_all(&output).unwrap();
    let sidecar: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.join("DSC08212.JPG.rrdata")).unwrap())
            .unwrap();
    let mut adjustments = sidecar["adjustments"].clone();
    let patches = adjustments["aiPatches"].as_array_mut().unwrap();
    let index = patches
        .iter()
        .position(|p| p["id"] == "ba757c0c-7f42-4784-be10-9cae214ca381")
        .unwrap();
    let patch = patches.remove(index);
    let base = image::open(fixture.join("DSC08212.JPG")).unwrap();
    let source = rapidraw_lib::image_loader::composite_patches_on_image(&base, &adjustments)
        .unwrap()
        .to_rgba8();
    let source = imageops::rotate180(&source); // fixture has both flips, no coarse rotation
    let debug = fixture.join("successful-generation");
    let plan: serde_json::Value =
        serde_json::from_slice(&std::fs::read(debug.join("replace-plan.json")).unwrap()).unwrap();
    let c = &plan["crop"];
    let (x, y, w, h) = (
        c["x"].as_u64().unwrap() as u32,
        c["y"].as_u64().unwrap() as u32,
        c["width"].as_u64().unwrap() as u32,
        c["height"].as_u64().unwrap() as u32,
    );
    let crop = imageops::crop_imm(&source, x, y, w, h).to_image();
    let context = image::open(debug.join("replace-context.png"))
        .unwrap()
        .to_rgba8();
    let a = imageops::resize(&crop, 256, 256, FilterType::Triangle);
    let b = imageops::resize(&context, 256, 256, FilterType::Triangle);
    let diff = a
        .pixels()
        .zip(b.pixels())
        .map(|(a, b)| {
            (0..3)
                .map(|c| (a[c] as f64 - b[c] as f64).abs())
                .sum::<f64>()
        })
        .sum::<f64>()
        / (256.0 * 256.0 * 3.0);
    println!("Legacy source/context mean absolute difference: {diff:.4}");
    assert!(diff < 3.0, "legacy migration must verify source alignment");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(patch["patchData"]["mask"].as_str().unwrap())
        .unwrap();
    let mask = imageops::rotate180(&image::load_from_memory(&bytes).unwrap().to_luma8());
    let mask = imageops::crop_imm(&mask, x, y, w, h).to_image();
    let preview_width = 1536;
    let preview_height = h * preview_width / w;
    let photo = imageops::resize(&crop, preview_width, preview_height, FilterType::Lanczos3);
    let mask = imageops::resize(&mask, preview_width, preview_height, FilterType::Nearest);
    let raw = image::open(debug.join("replace-raw-output.png"))
        .unwrap()
        .to_rgba8();
    let raw = imageops::resize(&raw, preview_width, preview_height, FilterType::Lanczos3);
    let sky = image::open(debug.join("replace-sky-recognition.png"))
        .unwrap()
        .to_luma8();
    let sky = imageops::resize(&sky, preview_width, preview_height, FilterType::Triangle);
    for (label, improved, appearance) in [
        ("original", false, 0.0),
        ("transition", true, 0.0),
        ("matched", true, 35.0),
    ] {
        let (color, alpha) = blend(
            &photo,
            &raw,
            &mask,
            Some(&sky),
            true,
            BlendOptions {
                improved,
                transition: 40.0,
                appearance,
            },
        )
        .unwrap();
        let mut composited = photo.clone();
        for ((p, g), m) in composited
            .pixels_mut()
            .zip(color.pixels())
            .zip(alpha.pixels())
        {
            let a = m[0] as f32 / 255.0;
            for c in 0..3 {
                p[c] = (p[c] as f32 * (1.0 - a) + g[c] as f32 * a).round() as u8;
            }
        }
        // Full-image export and preview must leave every zero-alpha pixel exact.
        for ((p, s), m) in composited.pixels().zip(photo.pixels()).zip(alpha.pixels()) {
            if m[0] == 0 {
                assert_eq!(p, s);
            }
        }
        println!(
            "{label}: {} selected pixels",
            alpha.pixels().filter(|p| p[0] > 0).count()
        );
        composited
            .save(output.join(format!("{label}.png")))
            .unwrap();
        alpha
            .save(output.join(format!("{label}-mask.png")))
            .unwrap();
    }
}
