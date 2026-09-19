//! Optional local model regression. No downloads; run with SUBJECT_MODELS set.
use rapidraw_lib::{ai_processing, model_registry::ModelRegistry, subject_selection};
use std::path::PathBuf;

#[test]
#[ignore = "requires installed SAM weights; set SUBJECT_MODELS"]
fn installed_sam_contract_and_refinement() {
    let models = PathBuf::from(std::env::var("SUBJECT_MODELS").expect("SUBJECT_MODELS"));
    let registry = ModelRegistry::new(models);
    let encoder = registry.get_session("sam-vit-b", None).unwrap();
    let decoder = registry.get_session("sam-vit-b", Some("decoder")).unwrap();
    println!("Encoder {:?}", encoder.lock().unwrap().inputs);
    println!("Decoder inputs {:?}", decoder.lock().unwrap().inputs);
    println!("Decoder outputs {:?}", decoder.lock().unwrap().outputs);
    let image = if let Ok(path) = std::env::var("SUBJECT_PHOTO") {
        image::open(path).unwrap()
    } else {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(240, 360, |x, y| {
            image::Rgb(if (60..180).contains(&x) && (40..320).contains(&y) {
                [210, 50, 40]
            } else {
                [40, 100, 170]
            })
        }))
    };
    let embeddings = ai_processing::generate_image_embeddings(&image, &encoder).unwrap();
    let click: Vec<f64> = std::env::var("SUBJECT_CLICK")
        .ok()
        .map(|v| v.split(',').map(|s| s.parse().unwrap()).collect())
        .unwrap_or_else(|| vec![image.width() as f64 * 0.5, image.height() as f64 * 0.5]);
    let mut points = subject_selection::box_or_point((click[0], click[1]), (click[0], click[1]));
    let first =
        subject_selection::select(&decoder, &embeddings, &points, Some("test"), true).unwrap();
    assert_eq!(first.dimensions(), (image.width(), image.height()));
    assert!(first.pixels().any(|p| p[0] > 127));
    assert!(
        first.get_pixel(click[0] as u32, click[1] as u32)[0] > 127,
        "the clicked subject must be included"
    );
    let exclude: Vec<f64> = std::env::var("SUBJECT_EXCLUDE")
        .ok()
        .map(|v| v.split(',').map(|s| s.parse().unwrap()).collect())
        .unwrap_or_else(|| vec![2.0, 2.0]);
    points.push(subject_selection::SubjectPoint {
        x: exclude[0],
        y: exclude[1],
        label: 0,
    });
    let refined =
        subject_selection::select(&decoder, &embeddings, &points, Some("test"), true).unwrap();
    assert_eq!(refined.dimensions(), first.dimensions());
    assert!(
        refined.get_pixel(exclude[0] as u32, exclude[1] as u32)[0] < 127,
        "negative prompt must exclude background"
    );
    // A correction must not depend on previously cached prediction history.
    let fresh = subject_selection::select(&decoder, &embeddings, &points, None, true).unwrap();
    assert_eq!(refined, fresh, "same prompts must produce the same mask");
    // Returning to the original prompt history must not inherit the refinement.
    let reset =
        subject_selection::select(&decoder, &embeddings, &points[..1], Some("test"), true).unwrap();
    assert_eq!(first, reset);
    if std::env::var("SUBJECT_PHOTO").is_err() {
        // Ambiguous flat-color fixtures can produce uncertain soft coverage.
        // Check semantic inclusion, not an unsupported alpha-calibration bound.
        assert!(first.get_pixel(120, 180)[0] > 127);
        assert!(
            refined.get_pixel(10, 180)[0] < 127,
            "background coverage: first={}, refined={}",
            first.get_pixel(10, 180)[0],
            refined.get_pixel(10, 180)[0]
        );
    }
    if let Ok(dir) = std::env::var("SUBJECT_OUTPUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        first.save(dir.join("subject-first.png")).unwrap();
        refined.save(dir.join("subject-refined.png")).unwrap();
        image.save(dir.join("subject-photo.png")).unwrap();
        let mut overlay = image.to_rgb8();
        for (x, y, p) in overlay.enumerate_pixels_mut() {
            let alpha = refined.get_pixel(x, y)[0] as f32 / 255.0 * 0.6;
            p[0] = (p[0] as f32 * (1.0 - alpha) + 255.0 * alpha) as u8;
            p[1] = (p[1] as f32 * (1.0 - alpha)) as u8;
            p[2] = (p[2] as f32 * (1.0 - alpha)) as u8;
        }
        overlay.save(dir.join("subject-overlay.png")).unwrap();
    }
}
