//! Sky Replace through the app's glue: find the sky with the real model,
//! composite a plate, encode it as the panel's patch, then put that patch
//! back through both engines and check what lands is what was composited.
//!
//! `SUBJECT_MODELS=<models dir> ORT_DYLIB_PATH=<runtime> SKY_PHOTO=a.jpg
//!  SKY_PLATE=p.jpg cargo test --release --test probe_sky_patch -- --ignored --nocapture`
use rapidraw_lib::{
    color_engine::{config::*, patches},
    model_registry::ModelRegistry,
    scene_masks, sky_commands, sky_replace,
};
use std::path::PathBuf;

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("{k}"))
}

fn encode_srgb(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

#[test]
#[ignore = "diagnostic: needs the scene model"]
fn sky_patch_round_trip() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let scene = registry.get_session("upernet-swin-large-ade", None).expect("scene model");
    let bytes = std::fs::read(env("SKY_PHOTO")).unwrap();
    let photo = rapidraw_lib::image_loader::load_image_with_orientation(&bytes, None).unwrap();
    let plate = image::open(env("SKY_PLATE")).unwrap();

    let t = std::time::Instant::now();
    let sky = scene_masks::sky_mask_scene(&photo, &scene, scene_masks::Orientation::default())
        .unwrap()
        .expect("this photograph has a sky");
    println!("sky found: {:.1}% of frame in {:.1}s", sky.coverage * 100.0, t.elapsed().as_secs_f64());

    for (name, options) in [
        ("auto match (whole frame)", sky_replace::SkyReplaceOptions::auto_match()),
        ("as shot (sky only)", sky_replace::SkyReplaceOptions::as_shot()),
    ] {
        let base = photo.to_rgb8();
        let t = std::time::Instant::now();
        let result = sky_replace::replace_sky(&photo, &sky.mask, &plate, &options).unwrap().to_rgb8();
        let composite_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = std::time::Instant::now();
        let patch = sky_commands::encode_patch(&base, &result, &sky.mask, &options, false).unwrap();
        let encode_ms = t.elapsed().as_secs_f64() * 1000.0;
        let size_mb = patch.to_string().len() as f64 / 1e6;
        let edits = serde_json::json!({"aiPatches": [{
            "id": "sky", "name": "Sky", "visible": true, "invert": false, "opacity": 100, "feather": 0,
            "subMasks": [], "patchType": "sky", "patchData": patch
        }]});

        // Previous engine: composites onto the display-encoded base.
        let legacy = rapidraw_lib::image_loader::composite_patches_on_image(&photo, &edits).unwrap().to_rgb8();
        // v3: composites onto the linear decoded source.
        let mut frame = rapidraw_lib::color_engine::input::decode_profiled_photo(&bytes).unwrap();
        patches::composite(&mut frame.pixels, &edits, &frame.color, None).unwrap();
        assert_eq!(frame.color.reference, ReferenceDomain::Display);

        let gap = |pick: &dyn Fn(u32, u32) -> [f32; 3]| {
            let mut sum = 0.0f64;
            let mut worst = 0.0f32;
            for (x, y, p) in result.enumerate_pixels() {
                let got = pick(x, y);
                for c in 0..3 {
                    let d = (got[c] - p[c] as f32 / 255.0).abs();
                    sum += d as f64;
                    worst = worst.max(d);
                }
            }
            (sum / (result.pixels().len() * 3) as f64 * 255.0, worst * 255.0)
        };
        let (legacy_mean, legacy_worst) = gap(&|x, y| legacy.get_pixel(x, y).0.map(|v| v as f32 / 255.0));
        let (v3_mean, v3_worst) = gap(&|x, y| frame.pixels.get_pixel(x, y).0[..3].try_into().map(|a: [f32; 3]| a.map(encode_srgb)).unwrap());
        println!(
            "{name:26} composite {composite_ms:5.0} ms, encode {encode_ms:5.0} ms, patch {size_mb:5.1} MB\n  previous engine: mean {legacy_mean:.2}/255 worst {legacy_worst:.0}/255\n  v3:              mean {v3_mean:.2}/255 worst {v3_worst:.0}/255"
        );
        assert!(legacy_mean < 1.5 && v3_mean < 1.5, "the patch does not reproduce the composite");
    }
}
