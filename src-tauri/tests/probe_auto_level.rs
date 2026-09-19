//! Diagnostics for auto level on real photos.
//!
//! `AUTO_LEVEL_DIR=/photos cargo test --test probe_auto_level -- --ignored --nocapture`
//! prints the estimate for every JPEG/PNG in the directory. With
//! `AUTO_LEVEL_TILT=3.5` it additionally rotates each photo by that many
//! degrees (through the app's own `apply_rotation`) and reports how far the
//! change in estimate is from the applied tilt, which measures accuracy
//! without needing ground truth for the originals.
use rapidraw_lib::{auto_level, image_processing::apply_rotation};
use std::time::Instant;

/// `AUTO_LEVEL_FILE=/photo.jpg [AUTO_LEVEL_TILT=3.5] [AUTO_LEVEL_OUT=dir]`:
/// print the competing peaks for one photo, and for its tilted copy.
#[test]
#[ignore = "diagnostic"]
fn single_file_peaks() {
    let path = std::env::var("AUTO_LEVEL_FILE").expect("AUTO_LEVEL_FILE");
    let img = image::open(&path).unwrap();
    let report = |label: &str, im: &image::DynamicImage| {
        let (est, peaks) = auto_level::estimate_level_verbose(im);
        println!("{label}: {est:?}");
        for p in peaks {
            println!(
                "    {:>6.2}°  share {:.3}  {}",
                p.tilt,
                p.mass_share,
                if p.horizontal { "H" } else { "V" }
            );
        }
    };
    report("original", &img);
    if let Ok(t) = std::env::var("AUTO_LEVEL_TILT") {
        let t: f32 = t.parse().unwrap();
        let rotated = apply_rotation(&img, t).into_owned();
        let (w, h) = (rotated.width(), rotated.height());
        let cropped = rotated.crop_imm(w / 6, h / 6, w * 2 / 3, h * 2 / 3);
        report(&format!("tilted {t:+}°"), &cropped);
        if let Ok(out) = std::env::var("AUTO_LEVEL_OUT") {
            cropped.to_rgb8().save(format!("{out}/tilted.png")).unwrap();
        }
    }
}

#[test]
#[ignore = "diagnostic"]
fn directory_report() {
    let dir = std::env::var("AUTO_LEVEL_DIR").expect("AUTO_LEVEL_DIR");
    let tilt: Option<f32> = std::env::var("AUTO_LEVEL_TILT").ok().map(|s| s.parse().unwrap());
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                Some("jpg" | "jpeg" | "png")
            )
        })
        .collect();
    entries.sort();
    let mut errors = Vec::new();
    for path in entries {
        let Ok(img) = image::open(&path) else { continue };
        let t0 = Instant::now();
        let est = auto_level::estimate_level(&img);
        let ms = t0.elapsed().as_millis();
        let name = path.file_name().unwrap().to_string_lossy();
        match est {
            Some(e) => print!(
                "{name:<20} {:>6.2}°  conf {:.3}  {:<10} {ms:>4} ms",
                e.angle, e.confidence, format!("{:?}", e.reference)
            ),
            None => print!("{name:<20}   none                          {ms:>4} ms"),
        }
        if let Some(t) = tilt {
            let rotated = apply_rotation(&img, t).into_owned();
            let (w, h) = (rotated.width(), rotated.height());
            let cropped = rotated.crop_imm(w / 6, h / 6, w * 2 / 3, h * 2 / 3);
            let est2 = auto_level::estimate_level(&cropped);
            match (est, est2) {
                (Some(a), Some(b)) => {
                    let err = (b.angle - a.angle) + t;
                    errors.push(err.abs());
                    print!("   after +{t}°: {:>6.2}° (err {:+.2}°)", b.angle, err);
                }
                (_, Some(b)) => print!("   after +{t}°: {:>6.2}° (no baseline)", b.angle),
                _ => print!("   after +{t}°: none"),
            }
        }
        println!();
    }
    if !errors.is_empty() {
        errors.sort_by(|a, b| a.total_cmp(b));
        let median = errors[errors.len() / 2];
        let max = errors[errors.len() - 1];
        println!("tilt-recovery error over {} photos: median {median:.2}°, max {max:.2}°", errors.len());
    }
}
