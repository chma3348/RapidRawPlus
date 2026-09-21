//! Diagnostics for the one-click scene masks (sky / foreground / subject).
//! Needs SUBJECT_MODELS (installed models dir) and ORT_DYLIB_PATH.
//!
//! `SCENE_DIR=/photos SCENE_OUT=/out cargo test --test probe_scene_masks raw_stats -- --ignored --nocapture`
//! prints raw model statistics per photo and saves the raw probability maps.
use rapidraw_lib::{ai_processing, model_registry::ModelRegistry};
use std::path::PathBuf;

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("{k}"))
}

fn photos(dir: &str) -> Vec<PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .as_deref(),
                Some("jpg" | "jpeg" | "png")
            )
        })
        .collect();
    v.sort();
    v
}

fn stats(label: &str, probs: &[f32]) -> String {
    let n = probs.len() as f32;
    let (mut lo, mut hi, mut sum, mut above, mut mid) = (f32::MAX, f32::MIN, 0.0, 0usize, 0usize);
    for &v in probs {
        lo = lo.min(v);
        hi = hi.max(v);
        sum += v;
        if v > 0.5 {
            above += 1;
        }
        if v > 0.2 && v < 0.8 {
            mid += 1;
        }
    }
    format!(
        "{label}: min {lo:.3} max {hi:.3} mean {:.3} >0.5 {:.1}% uncertain(0.2..0.8) {:.1}%",
        sum / n,
        100.0 * above as f32 / n,
        100.0 * mid as f32 / n
    )
}

#[test]
#[ignore = "diagnostic"]
fn raw_stats() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let sky = registry.get_session("u2net-sky", None).expect("sky model");
    let fg = registry
        .get_session("u2net-foreground", None)
        .expect("u2net model");
    let out = PathBuf::from(env("SCENE_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let limit: usize = std::env::var("SCENE_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    for path in photos(&env("SCENE_DIR")).into_iter().take(limit) {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let img = image::open(&path).unwrap();
        let t = std::time::Instant::now();
        let (sp, sw, sh) = ai_processing::run_u2net_probabilities(&img, &sky, 320).unwrap();
        let t_sky = t.elapsed();
        let t = std::time::Instant::now();
        let (fp, fw, fh) = ai_processing::run_u2net_probabilities(&img, &fg, 320).unwrap();
        let t_fg = t.elapsed();
        println!("{name:<14} {}   [{t_sky:.0?}]", stats("sky", &sp));
        println!("{:<14} {}   [{t_fg:.0?}]", "", stats("fg ", &fp));
        let save = |p: &[f32], w: u32, h: u32, suffix: &str| {
            let g = image::GrayImage::from_raw(
                w,
                h,
                p.iter()
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8)
                    .collect(),
            )
            .unwrap();
            g.save(out.join(format!("{name}-{suffix}.png"))).unwrap();
        };
        save(&sp, sw, sh, "sky");
        save(&fp, fw, fh, "fg");
    }
}

/// Can the models run at a larger input than 320? Prints the outcome.
#[test]
#[ignore = "diagnostic"]
fn input_size_flexibility() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let sky = registry.get_session("u2net-sky", None).expect("sky model");
    let img = image::open(env("SCENE_PHOTO")).unwrap();
    for size in [320u32, 480, 640] {
        let t = std::time::Instant::now();
        match ai_processing::run_u2net_probabilities(&img, &sky, size) {
            Ok((p, w, h)) => println!(
                "size {size}: ok {w}x{h} in {:.0?}; {}",
                t.elapsed(),
                stats("sky", &p)
            ),
            Err(e) => println!("size {size}: error {e}"),
        }
    }
}

/// Does orientation matter to the models? Runs sky/fg on the photo and on
/// its 90°-rotated copies. `SCENE_PHOTO`.
#[test]
#[ignore = "diagnostic"]
fn orientation_sensitivity() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let sky = registry.get_session("u2net-sky", None).unwrap();
    let fg = registry.get_session("u2net-foreground", None).unwrap();
    let img = image::open(env("SCENE_PHOTO")).unwrap();
    for (label, im) in [
        ("as stored", img.clone()),
        ("rotate90", img.rotate90()),
        ("rotate270", img.rotate270()),
    ] {
        let (sp, _, _) = ai_processing::run_u2net_probabilities(&im, &sky, 320).unwrap();
        let (fp, _, _) = ai_processing::run_u2net_probabilities(&im, &fg, 320).unwrap();
        println!("{label:<10} {} | {}", stats("sky", &sp), stats("fg", &fp));
    }
}

/// Depth maps for a directory: saves the 0..255 relative depth and prints
/// the Otsu split (as a fraction of the range) and the near-side share.
#[test]
#[ignore = "diagnostic"]
fn depth_maps() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let depth = registry
        .get_session("depth-anything-v2-vits", None)
        .unwrap();
    let out = PathBuf::from(env("SCENE_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let limit: usize = std::env::var("SCENE_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    for path in photos(&env("SCENE_DIR")).into_iter().take(limit) {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let img = image::open(&path).unwrap();
        let t = std::time::Instant::now();
        let d = ai_processing::run_depth_anything_model(&img, &depth).unwrap();
        let ms = t.elapsed();
        let (raw, _, _) = ai_processing::run_depth_anything_raw(&img, &depth).unwrap();
        let (rlo, rhi) = raw
            .iter()
            .fold((f32::MAX, f32::MIN), |a, &v| (a.0.min(v), a.1.max(v)));
        let rmean = raw.iter().sum::<f32>() / raw.len() as f32;
        print!(
            "raw disparity min {rlo:.2} max {rhi:.2} range {:.2} mean {rmean:.2}   ",
            rhi - rlo
        );
        let mut hist = [0usize; 256];
        for p in d.pixels() {
            hist[p[0] as usize] += 1;
        }
        let total: usize = hist.iter().sum();
        // Otsu
        let (mut best_t, mut best_var) = (0usize, 0.0f64);
        let sum_all: f64 = hist
            .iter()
            .enumerate()
            .map(|(i, &c)| i as f64 * c as f64)
            .sum();
        let (mut w0, mut sum0) = (0.0f64, 0.0f64);
        for (t, &count) in hist.iter().enumerate() {
            w0 += count as f64;
            if w0 == 0.0 {
                continue;
            }
            let w1 = total as f64 - w0;
            if w1 == 0.0 {
                break;
            }
            sum0 += t as f64 * count as f64;
            let m0 = sum0 / w0;
            let m1 = (sum_all - sum0) / w1;
            let var = w0 * w1 * (m0 - m1) * (m0 - m1);
            if var > best_var {
                best_var = var;
                best_t = t;
            }
        }
        let near: usize = hist[best_t + 1..].iter().sum();
        println!(
            "{name:<14} depth {}x{} otsu {:.2} near-share {:.1}%  [{ms:.0?}]",
            d.width(),
            d.height(),
            best_t as f32 / 255.0,
            100.0 * near as f32 / total as f32
        );
        d.save(out.join(format!("{name}-depth.png"))).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Evaluation of the shipped pipelines.
// `SCENE_DIR=... SCENE_OUT=... [SCENE_LIMIT=n] [SCENE_SUBJECT=1] cargo test --test probe_scene_masks scene_eval -- --ignored --nocapture`
//
// Per photo: coverage, Otsu separability (foreground), mirror-consistency IoU
// (how much the sky/depth models disagree with themselves on the mirrored
// photo), edge alignment of the coarse vs guided mask, timings. Saves
// full-res masks downscaled to 1024 px for contact sheets.
// ---------------------------------------------------------------------------
use image::{DynamicImage, GrayImage};
use rapidraw_lib::scene_masks::{self, Orientation, ProbabilityMap};

fn small(img: &DynamicImage) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let s = 1024.0 / w.max(h) as f32;
    if s >= 1.0 {
        img.clone()
    } else {
        img.resize(
            (w as f32 * s) as u32,
            (h as f32 * s) as u32,
            image::imageops::FilterType::Triangle,
        )
    }
}

fn small_mask(m: &GrayImage, target: (u32, u32)) -> GrayImage {
    image::imageops::resize(m, target.0, target.1, image::imageops::FilterType::Triangle)
}

/// Share of mask boundary pixels lying within 3 px of a strong luma edge.
fn edge_alignment(mask: &GrayImage, photo: &DynamicImage) -> f32 {
    let luma = photo.to_luma32f();
    let (w, h) = (luma.width() as usize, luma.height() as usize);
    let l = |x: usize, y: usize| luma.get_pixel(x as u32, y as u32)[0];
    let mut strong = vec![false; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = l(x + 1, y) - l(x - 1, y);
            let gy = l(x, y + 1) - l(x, y - 1);
            strong[y * w + x] = (gx * gx + gy * gy).sqrt() > 0.12;
        }
    }
    let m = |x: usize, y: usize| mask.get_pixel(x as u32, y as u32)[0] > 127;
    let (mut boundary, mut aligned) = (0usize, 0usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            if m(x, y) != m(x + 1, y) || m(x, y) != m(x, y + 1) {
                boundary += 1;
                let mut hit = false;
                'n: for dy in -3i64..=3 {
                    for dx in -3i64..=3 {
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx >= 0
                            && ny >= 0
                            && (nx as usize) < w
                            && (ny as usize) < h
                            && strong[ny as usize * w + nx as usize]
                        {
                            hit = true;
                            break 'n;
                        }
                    }
                }
                aligned += hit as usize;
            }
        }
    }
    if boundary == 0 {
        f32::NAN
    } else {
        aligned as f32 / boundary as f32
    }
}

fn iou(a: &GrayImage, b: &GrayImage) -> f32 {
    let (mut i, mut u) = (0usize, 0usize);
    for (p, q) in a.pixels().zip(b.pixels()) {
        let (x, y) = (p[0] > 127, q[0] > 127);
        i += (x && y) as usize;
        u += (x || y) as usize;
    }
    if u == 0 { 1.0 } else { i as f32 / u as f32 }
}

fn coarse_mask(p: &ProbabilityMap, target: (u32, u32)) -> GrayImage {
    let g = GrayImage::from_raw(
        p.width(),
        p.height(),
        p.as_raw()
            .iter()
            .map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8)
            .collect(),
    )
    .unwrap();
    image::imageops::resize(
        &g,
        target.0,
        target.1,
        image::imageops::FilterType::Triangle,
    )
}

#[test]
#[ignore = "diagnostic"]
fn scene_eval() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let sky = registry.get_session("u2net-sky", None).unwrap();
    let fg = registry.get_session("u2net-foreground", None).unwrap();
    let depth = registry
        .get_session("depth-anything-v2-vits", None)
        .unwrap();
    let do_subject = std::env::var("SCENE_SUBJECT").is_ok();
    let (encoder, decoder) = if do_subject {
        (
            Some(registry.get_session("sam-vit-b", None).unwrap()),
            Some(registry.get_session("sam-vit-b", Some("decoder")).unwrap()),
        )
    } else {
        (None, None)
    };
    let out = PathBuf::from(env("SCENE_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let limit: usize = std::env::var("SCENE_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    let o = Orientation::default();
    for path in photos(&env("SCENE_DIR")).into_iter().take(limit) {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let img = image::open(&path).unwrap();
        let sm = small(&img);
        let target = (sm.width(), sm.height());
        // Sky
        let t = std::time::Instant::now();
        let p_sky = scene_masks::probabilities_oriented(&img, &sky, 320, o, false).unwrap();
        let sky_res = scene_masks::sky_mask(&img, &sky, o).unwrap();
        let t_sky = t.elapsed();
        let (sky_cov, sky_mir, sky_ec, sky_eg) = match &sky_res {
            Some(s) => {
                let g = small_mask(&s.mask, target);
                g.save(out.join(format!("{name}-sky-mask.png"))).unwrap();
                let c = coarse_mask(&p_sky, target);
                // mirror consistency: single pass vs single pass on mirrored
                let single = coarse_mask(&p_sky, target);
                let mirrored = coarse_mask(
                    &scene_masks::probabilities_oriented(&img.fliph(), &sky, 320, o, false)
                        .map(|m| image::imageops::flip_horizontal(&m))
                        .unwrap(),
                    target,
                );
                (
                    s.coverage * 100.0,
                    iou(&single, &mirrored),
                    edge_alignment(&c, &sm),
                    edge_alignment(&g, &sm),
                )
            }
            None => (0.0, f32::NAN, f32::NAN, f32::NAN),
        };

        // Subject (needed by Foreground: foreground = in front of the subject)
        let mut subj = (0.0f32, String::from("-"), f32::NAN);
        let mut t_subj = std::time::Duration::ZERO;
        let mut subject_mask: Option<GrayImage> = None;
        if let (Some(enc), Some(dec)) = (&encoder, &decoder) {
            let t = std::time::Instant::now();
            let emb = ai_processing::generate_image_embeddings(&img, enc).unwrap();
            let p_fg = scene_masks::probabilities_oriented(&img, &fg, 320, o, false).unwrap();
            let res = scene_masks::auto_subject_from_saliency(&p_fg, &img, dec, &emb).unwrap();
            t_subj = t.elapsed();
            if let Some(a) = res {
                let g = small_mask(&a.scene.mask, target);
                g.save(out.join(format!("{name}-subject-mask.png")))
                    .unwrap();
                let proposal = coarse_mask(&p_fg, target);
                subj = (
                    a.scene.coverage * 100.0,
                    if a.from_sam {
                        "yes".into()
                    } else {
                        "no".into()
                    },
                    iou(&g, &proposal),
                );
                subject_mask = Some(a.scene.mask);
            }
        }

        // Foreground relative to the subject
        let t = std::time::Instant::now();
        let (fg_cov, fg_note, fg_eg) = match &subject_mask {
            None => (0.0, "no subject".to_string(), f32::NAN),
            Some(subject) => {
                match scene_masks::foreground_mask(&img, &depth, subject, o).unwrap() {
                    Ok(f) => {
                        let g = small_mask(&f.mask, target);
                        g.save(out.join(format!("{name}-fg-mask.png"))).unwrap();
                        let overlap = f
                            .mask
                            .pixels()
                            .zip(subject.pixels())
                            .filter(|(a, b)| a[0] > 127 && b[0] > 127)
                            .count();
                        (
                            f.coverage * 100.0,
                            format!("overlap {overlap}px"),
                            edge_alignment(&g, &sm),
                        )
                    }
                    Err(reason) => (0.0, format!("{reason:?}"), f32::NAN),
                }
            }
        };
        let t_fg = t.elapsed();
        let _ = &fg_eg;

        println!(
            "{name:<14} sky {sky_cov:>5.1}% mir {sky_mir:>5.3} | subject {:>5.1}% sam {:>3} agree {:>5.2} | fg {fg_cov:>5.1}% {fg_note}   [sky {t_sky:.0?} subj {t_subj:.1?} fg {t_fg:.1?}]",
            subj.0, subj.1, subj.2
        );
        let _ = (sky_ec, sky_eg);
    }
}

/// Parity: run the app's BiRefNet and scene-label runners on one photo and
/// save the raw maps as .f32 files for comparison with the Python harness.
/// `SCENE_PHOTO=... SCENE_EXTRA_MODELS=<dir with birefnet_lite.onnx, upernet_*.onnx> SCENE_OUT=...`
#[test]
#[ignore = "diagnostic"]
fn runner_parity() {
    let dir = PathBuf::from(env("SCENE_EXTRA_MODELS"));
    let out = PathBuf::from(env("SCENE_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let _ = ort::init().with_name("probe").commit();
    let open = |f: &str| {
        std::sync::Mutex::new(
            ort::session::Session::builder()
                .unwrap()
                .commit_from_file(dir.join(f))
                .unwrap(),
        )
    };
    let img = image::open(env("SCENE_PHOTO")).unwrap();
    let img = img.resize(1600, 1600, image::imageops::FilterType::Triangle);
    let t = std::time::Instant::now();
    let (b, bw, bh) = ai_processing::run_birefnet(&img, &open("birefnet_lite.onnx")).unwrap();
    println!(
        "birefnet {bw}x{bh} in {:.1?}, mean {:.4}",
        t.elapsed(),
        b.iter().sum::<f32>() / b.len() as f32
    );
    std::fs::write(
        out.join("rust_birefnet.f32"),
        b.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(),
    )
    .unwrap();
    let t = std::time::Instant::now();
    let (p, classes, rw, rh) =
        ai_processing::run_scene_labels(&img, &open("upernet_convnext_small.onnx"), 768).unwrap();
    let n = (rw * rh) as usize;
    println!(
        "scene labels {classes} x {rw}x{rh} in {:.1?}, sky mean {:.4}",
        t.elapsed(),
        p[2 * n..3 * n].iter().sum::<f32>() / n as f32
    );
    std::fs::write(
        out.join("rust_sky.f32"),
        p[2 * n..3 * n]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    std::fs::write(
        out.join("rust_dims.txt"),
        format!("{} {} {} {}", img.width(), img.height(), rw, rh),
    )
    .unwrap();
}

/// End-to-end check of the shipping Sky and Subject pipelines through the
/// app's own registry and runtime.
/// `SCENE_DIR=... SCENE_OUT=... [SCENE_ONLY=a,b,c] SUBJECT_MODELS=<models dir>`
#[test]
#[ignore = "diagnostic"]
fn new_pipeline() {
    let registry = ModelRegistry::new(PathBuf::from(env("SUBJECT_MODELS")));
    let scene = registry
        .get_session("upernet-swin-large-ade", None)
        .expect("scene model");
    let birefnet = registry
        .get_session("birefnet-lite", None)
        .expect("birefnet");
    let out = PathBuf::from(env("SCENE_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let only: Option<Vec<String>> = std::env::var("SCENE_ONLY")
        .ok()
        .map(|s| s.split(',').map(str::to_string).collect());
    let o = Orientation::default();
    for path in photos(&env("SCENE_DIR")) {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if let Some(list) = &only
            && !list.contains(&name)
        {
            continue;
        }
        // Load the way the app does, so EXIF rotation is applied.
        let bytes = std::fs::read(&path).unwrap();
        let img = rapidraw_lib::image_loader::load_image_with_orientation(&bytes, None).unwrap();
        let img = img.resize(2400, 2400, image::imageops::FilterType::Triangle);
        let t = std::time::Instant::now();
        let sky = scene_masks::sky_mask_scene(&img, &scene, o).unwrap();
        let t_sky = t.elapsed();
        let t = std::time::Instant::now();
        let subj = scene_masks::auto_subject_birefnet(&img, &birefnet, o).unwrap();
        let t_subj = t.elapsed();
        let show = |m: &Option<scene_masks::SceneMask>| {
            m.as_ref()
                .map(|s| format!("{:.1}%", s.coverage * 100.0))
                .unwrap_or_else(|| "none".into())
        };
        println!(
            "{name:<16} sky {:>6} [{t_sky:.1?}]   subject {:>6} [{t_subj:.1?}]",
            show(&sky),
            subj.as_ref()
                .map(|a| format!("{:.1}%", a.scene.coverage * 100.0))
                .unwrap_or_else(|| "none".into())
        );
        if let Some(s) = sky {
            s.mask
                .save(out.join(format!("{name}-sky-mask.png")))
                .unwrap();
        }
        if let Some(a) = subj {
            a.scene
                .mask
                .save(out.join(format!("{name}-subject-mask.png")))
                .unwrap();
        }
    }
}
