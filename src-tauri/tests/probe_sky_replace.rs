//! End-to-end sky replacement through the app's own code: detect the sky,
//! then composite a plate into the photo.
//!
//! `SUBJECT_MODELS=<models dir> ORT_DYLIB_PATH=<runtime> SKY_PHOTOS=a.jpg,b.jpg
//!  SKY_PLATES=p1.jpg,p2.jpg SKY_OUT=<dir> cargo test --test probe_sky_replace
//!  -- --ignored --nocapture`
use rapidraw_lib::{model_registry::ModelRegistry, scene_masks, sky_replace};
use std::path::PathBuf;

fn env(k: &str) -> String { std::env::var(k).unwrap_or_else(|_| panic!("{k}")) }

#[test]
#[ignore = "diagnostic"]
fn replace() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let scene = registry.get_session("upernet-swin-large-ade", None).expect("scene model");
    let out = PathBuf::from(env("SKY_OUT")); std::fs::create_dir_all(&out).unwrap();
    let opts = sky_replace::SkyReplaceOptions::default();
    let plates: Vec<PathBuf> = env("SKY_PLATES").split(',').map(PathBuf::from).collect();
    for photo_path in env("SKY_PHOTOS").split(',') {
        let bytes = std::fs::read(photo_path).unwrap();
        let photo = rapidraw_lib::image_loader::load_image_with_orientation(&bytes, None).unwrap();
        let photo = photo.resize(2400, 2400, image::imageops::FilterType::Triangle);
        let t = std::time::Instant::now();
        let Some(sky) = scene_masks::sky_mask_scene(&photo, &scene, scene_masks::Orientation::default()).unwrap() else {
            println!("{photo_path}: no sky found");
            continue;
        };
        let t_mask = t.elapsed();
        let name = PathBuf::from(photo_path).file_stem().unwrap().to_string_lossy().to_string();
        for plate_path in &plates {
            let plate = image::open(plate_path).unwrap();
            let t = std::time::Instant::now();
            let done = sky_replace::replace_sky(&photo, &sky.mask, &plate, &opts).unwrap();
            let plate_name = plate_path.file_stem().unwrap().to_string_lossy();
            let stem: String = plate_name.chars().take(28).collect();
            done.save(out.join(format!("{name}__{stem}.jpg"))).unwrap();
            println!("{name} + {stem}: sky {:.0}% [mask {t_mask:.1?}, composite {:.1?}]",
                sky.coverage * 100.0, t.elapsed());
        }
    }
}
