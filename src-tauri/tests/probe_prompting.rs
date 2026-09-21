//! Diagnostics for prompting strategies. Needs SUBJECT_MODELS + ORT_DYLIB_PATH.
use rapidraw_lib::{ai_processing, model_registry::ModelRegistry, subject_selection};
use std::path::PathBuf;

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("{k}"))
}
fn nums(k: &str) -> Vec<f64> {
    env(k).split(',').map(|s| s.parse().unwrap()).collect()
}

/// Box prompt around the subject: does SAM produce the whole object?
#[test]
#[ignore = "diagnostic"]
fn box_prompt() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let encoder = registry.get_session("sam-vit-b", None).unwrap();
    let decoder = registry.get_session("sam-vit-b", Some("decoder")).unwrap();
    let image = image::open(env("SUBJECT_PHOTO")).unwrap();
    let emb = ai_processing::generate_image_embeddings(&image, &encoder).unwrap();
    let b = nums("SUBJECT_BOX");
    let pts = subject_selection::box_or_point((b[0], b[1]), (b[2], b[3]));
    let m = subject_selection::select(&decoder, &emb, &pts, None, true).unwrap();
    let area = m.pixels().filter(|p| p[0] > 127).count() as f64 / (m.width() * m.height()) as f64;
    println!("box mask area {:.2}% of frame", area * 100.0);
    let out = PathBuf::from(env("SUBJECT_OUTPUT"));
    std::fs::create_dir_all(&out).unwrap();
    m.save(out.join("box-matte.png")).unwrap();
}

/// U-2-Net saliency: matte + bbox of the salient component under the click.
#[test]
#[ignore = "diagnostic"]
fn u2net_saliency() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let sess = registry.get_session("u2net-foreground", None).unwrap();
    let image = image::open(env("SUBJECT_PHOTO")).unwrap();
    let t = std::time::Instant::now();
    let m = ai_processing::run_u2netp_model(&image, &sess).unwrap();
    println!(
        "u2net {:?}, matte {}x{}",
        t.elapsed(),
        m.width(),
        m.height()
    );
    let c = nums("SUBJECT_CLICK");
    let (w, h) = (m.width() as usize, m.height() as usize);
    let sx = w as f64 / image.width() as f64;
    let sy = h as f64 / image.height() as f64;
    let (cx, cy) = ((c[0] * sx) as usize, (c[1] * sy) as usize);
    let v = |x: usize, y: usize| m.get_pixel(x as u32, y as u32)[0] > 96;
    println!(
        "click saliency value {}",
        m.get_pixel(cx as u32, cy as u32)[0]
    );
    let mut seen = vec![false; w * h];
    let mut stack = vec![(cx, cy)];
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    let mut n = 0;
    while let Some((x, y)) = stack.pop() {
        if seen[y * w + x] || !v(x, y) {
            continue;
        }
        seen[y * w + x] = true;
        n += 1;
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
        if x > 0 {
            stack.push((x - 1, y));
        }
        if x + 1 < w {
            stack.push((x + 1, y));
        }
        if y > 0 {
            stack.push((x, y - 1));
        }
        if y + 1 < h {
            stack.push((x, y + 1));
        }
    }
    println!(
        "salient component under click: {} px ({:.2}% of frame), bbox in image coords ({:.0},{:.0})-({:.0},{:.0})",
        n,
        100.0 * n as f64 / (w * h) as f64,
        x0 as f64 / sx,
        y0 as f64 / sy,
        x1 as f64 / sx,
        y1 as f64 / sy
    );
    let out = PathBuf::from(env("SUBJECT_OUTPUT"));
    std::fs::create_dir_all(&out).unwrap();
    m.save(out.join("u2net-matte.png")).unwrap();
}
