//! Fill test set: runs the app's REAL fill command headlessly on a fixed set
//! of photos and selections, and scores every result inside its selection.
//!
//! ```text
//! ORT_DYLIB_PATH=resources/libonnxruntime.dylib cargo run --release --example fill_testset -- \
//!     examples/fill_testset_cases.txt <run dir> [--methods lama,ca,flux] [--only id,id] \
//!     [--photos "<dir>"] [--engine "<engine dir>"]
//! ```
//!
//! Writes into the run dir: `index.html` (every case: sheet + metrics),
//! `metrics.csv`, `metrics.md`, `sheets/<case>.png`, `fill.log` (the fill
//! path's own log lines) and `results/<case>-<method>.png` composites.
//!
//! The fills run through `invoke_generative_replace_with_mask_def`, the
//! command the Retouch panel calls, inside a real (windowless) Tauri app
//! whose data folder is private to the run: it links the installed models
//! read-only and never touches the app's own data. Flux runs on a private
//! ComfyUI started here on port 8499 with its own input/output/temp/user
//! folders, and stopped on exit; the app's engine and port are untouched.
//!
//! Metrics, all INSIDE the selection:
//! * holdout (truth exists): Oklab ΔE (x100) to the truth; SSIM on luma;
//!   texture = mean gradient magnitude fill/truth (1 ideal, <0.5 mush); seam
//!   = mean colour step across the selection edge / the same step one pixel
//!   out (1 = no visible edge), with the truth's own seam alongside.
//! * clipped (no truth): texture = in-mask gradient / gradient in the nearest
//!   unclipped bright surroundings; seam as above; ΔE of the mean colour to
//!   those surroundings; share of the selection still clipped white.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GrayImage, ImageEncoder, Luma, Rgba, RgbaImage};
use serde_json::{Value, json};

const ENGINE_PORT: u16 = 8499;
const FLUX_SEED: &str = "1234";
const PANEL_W: u32 = 560;

struct Case {
    id: String,
    photo: String,
    kind: String,
    params: HashMap<String, String>,
    methods: Vec<String>,
}

fn parse_cases(path: &Path) -> anyhow::Result<Vec<Case>> {
    let mut out = Vec::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('|').map(str::trim).collect();
        anyhow::ensure!(f.len() == 5, "bad case line: {line}");
        let params = f[3]
            .split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        out.push(Case {
            id: f[0].to_string(),
            photo: f[1].to_string(),
            kind: f[2].to_string(),
            params,
            methods: f[4].split(',').map(|m| m.trim().to_string()).collect(),
        });
    }
    Ok(out)
}

fn pf(case: &Case, key: &str) -> f64 {
    case.params
        .get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{}: missing {key}", case.id))
}

// ---------------------------------------------------------------- setup

/// A private app-data folder and engine folder for this run.
fn private_env(run: &Path, real_home: &Path, engine: &Path) -> anyhow::Result<PathBuf> {
    let env = run.join(".env");
    let home = env.join("home");
    let data = home.join("Library/Application Support/io.github.chma3348.DarkroomIndex");
    std::fs::create_dir_all(&data)?;
    let real_data = real_home.join("Library/Application Support/io.github.chma3348.DarkroomIndex");
    // Installed models, read through a link (the app's own data folder
    // links them the same way).
    let models = std::fs::canonicalize(real_data.join("models"))?;
    let link = data.join("models");
    if std::fs::symlink_metadata(&link).is_err() {
        std::os::unix::fs::symlink(&models, &link)?;
    }
    // Engine mirror: the installed venv and model weights, with private
    // input/output/temp/user folders.
    let mirror = env.join("engine");
    let comfy = mirror.join("ComfyUI");
    for d in ["input", "output", "temp", "user"] {
        std::fs::create_dir_all(comfy.join(d))?;
    }
    for (from, to) in [
        (engine.join("venv"), mirror.join("venv")),
        (engine.join("ComfyUI/main.py"), comfy.join("main.py")),
        (engine.join("ComfyUI/models"), comfy.join("models")),
    ] {
        if std::fs::symlink_metadata(&to).is_err() {
            std::os::unix::fs::symlink(&from, &to)?;
        }
    }
    let link = data.join("comfy");
    if std::fs::symlink_metadata(&link).is_err() {
        std::os::unix::fs::symlink(&mirror, &link)?;
    }
    // Settings: Flux Fill is the generative model, as on Chris's machine.
    let mut settings: Value = std::fs::read_to_string(real_data.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    settings["preferredModels"]["inpaint"] = json!("flux-fill");
    std::fs::write(
        data.join("settings.json"),
        serde_json::to_string_pretty(&settings)?,
    )?;
    Ok(home)
}

fn engine_up() -> bool {
    let Ok(mut s) = std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], ENGINE_PORT).into(),
        Duration::from_millis(500),
    ) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = s.write_all(b"GET /system_stats HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    let mut buf = [0u8; 16];
    s.read(&mut buf).is_ok_and(|n| {
        buf[..n].starts_with(b"HTTP/1.1 200") || buf[..n].starts_with(b"HTTP/1.0 200")
    })
}

/// The private engine, stopped when dropped.
struct Engine(Option<std::process::Child>);

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("[testset] private engine stopped");
        }
    }
}

fn start_engine(engine: &Path, env: &Path) -> anyhow::Result<Engine> {
    anyhow::ensure!(
        !engine_up(),
        "something already answers on port {ENGINE_PORT}; not starting a second engine"
    );
    let comfy = env.join("engine/ComfyUI");
    let log = std::fs::File::create(env.join("engine.log"))?;
    let child = std::process::Command::new(engine.join("venv/bin/python"))
        .arg("main.py")
        .args(["--listen", "127.0.0.1", "--port", &ENGINE_PORT.to_string()])
        .arg("--input-directory")
        .arg(comfy.join("input"))
        .arg("--output-directory")
        .arg(comfy.join("output"))
        .arg("--temp-directory")
        .arg(comfy.join("temp"))
        .arg("--user-directory")
        .arg(comfy.join("user"))
        .arg("--database-url")
        .arg(format!(
            "sqlite:///{}",
            comfy.join("user/comfyui.db").display()
        ))
        .arg("--disable-auto-launch")
        .env("PYTORCH_ENABLE_MPS_FALLBACK", "1")
        .current_dir(engine.join("ComfyUI"))
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let guard = Engine(Some(child));
    for _ in 0..90 {
        std::thread::sleep(Duration::from_secs(2));
        if engine_up() {
            eprintln!("[testset] private engine up on {ENGINE_PORT}");
            return Ok(guard);
        }
    }
    anyhow::bail!("private engine did not come up (see .env/engine.log)")
}

// ---------------------------------------------------------------- selections

fn png_data_url(mask: &GrayImage) -> String {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            mask.as_raw(),
            mask.width(),
            mask.height(),
            image::ExtendedColorType::L8,
        )
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(bytes)
    )
}

/// A smooth, irregular blob: an ellipse with a few low harmonics on its
/// radius, like a removed object's outline.
fn blob_mask(w: u32, h: u32, case: &Case) -> GrayImage {
    let (cx, cy) = (pf(case, "cx") * w as f64, pf(case, "cy") * h as f64);
    let area = pf(case, "frac") * w as f64 * h as f64;
    let aspect = pf(case, "aspect");
    let ry = (area / (std::f64::consts::PI * aspect)).sqrt();
    let rx = ry * aspect;
    let mut state = pf(case, "seed") as u64 * 0x9E37_79B9_7F4A_7C15 + 1;
    let mut rnd = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 10_000) as f64 / 10_000.0
    };
    let harmonics: Vec<(f64, f64, f64)> = (2..=5)
        .map(|k| {
            (
                k as f64,
                rnd() * 0.18 / (k as f64 - 1.0),
                rnd() * std::f64::consts::TAU,
            )
        })
        .collect();
    GrayImage::from_fn(w, h, |x, y| {
        let dx = (x as f64 - cx) / rx;
        let dy = (y as f64 - cy) / ry;
        let r = (dx * dx + dy * dy).sqrt();
        if r > 1.6 {
            return Luma([0]);
        }
        let t = dy.atan2(dx);
        let edge = 1.0
            + harmonics
                .iter()
                .map(|(k, a, p)| a * (k * t + p).sin())
                .sum::<f64>();
        Luma([if r <= edge { 255 } else { 0 }])
    })
}

fn roi_mask(w: u32, h: u32, case: &Case) -> GrayImage {
    let v: Vec<f64> = case.params["roi"]
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let (x0, y0, x1, y1) = (
        (v[0] * w as f64) as u32,
        (v[1] * h as f64) as u32,
        (v[2] * w as f64) as u32,
        (v[3] * h as f64) as u32,
    );
    GrayImage::from_fn(w, h, |x, y| {
        Luma([if x >= x0 && x < x1 && y >= y0 && y < y1 {
            255
        } else {
            0
        }])
    })
}

fn raster_submask(id: &str, mask: &GrayImage, mode: &str) -> Value {
    json!({
        "id": id, "type": "ai-subject", "visible": true, "invert": false,
        "opacity": 100.0, "mode": mode,
        "parameters": {"startX": 0.0, "startY": 0.0, "endX": 0.0, "endY": 0.0,
                        "maskDataBase64": png_data_url(mask)}
    })
}

// ---------------------------------------------------------------- metrics

fn srgb_to_lin(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn oklab(p: &Rgba<u8>) -> [f32; 3] {
    let r = srgb_to_lin(p[0] as f32 / 255.0);
    let g = srgb_to_lin(p[1] as f32 / 255.0);
    let b = srgb_to_lin(p[2] as f32 / 255.0);
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn de(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt() * 100.0
}

fn luma(p: &Rgba<u8>) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// Chamfer distance to the nearest pixel where `inside` differs.
fn dist_to(inside: &[bool], w: usize, h: usize, want: bool) -> Vec<f32> {
    let mut d: Vec<f32> = inside
        .iter()
        .map(|&b| if b == want { 1e6 } else { 0.0 })
        .collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if x > 0 {
                d[i] = d[i].min(d[i - 1] + 1.0);
            }
            if y > 0 {
                d[i] = d[i].min(d[i - w] + 1.0);
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if x + 1 < w {
                d[i] = d[i].min(d[i + 1] + 1.0);
            }
            if y + 1 < h {
                d[i] = d[i].min(d[i + w] + 1.0);
            }
        }
    }
    d
}

/// Geometry of a selection, computed once per case on a window around it.
struct Region {
    x0: u32,
    y0: u32,
    w: usize,
    h: usize,
    inside: Vec<bool>,
    d_in: Vec<f32>,
    d_out: Vec<f32>,
    span: u32,
}

fn region(mask: &GrayImage) -> Option<Region> {
    let (mw, mh) = mask.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, v) in mask.enumerate_pixels() {
        if v[0] > 127 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == u32::MAX {
        return None;
    }
    let span = (x1 - x0 + 1).max(y1 - y0 + 1);
    let pad = (span / 3).clamp(40, 300);
    let (rx0, ry0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (rx1, ry1) = ((x1 + pad).min(mw - 1), (y1 + pad).min(mh - 1));
    let (w, h) = ((rx1 - rx0 + 1) as usize, (ry1 - ry0 + 1) as usize);
    let mut inside = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            inside[y * w + x] = mask.get_pixel(rx0 + x as u32, ry0 + y as u32)[0] > 127;
        }
    }
    let d_in = dist_to(&inside, w, h, true);
    let d_out = dist_to(&inside, w, h, false);
    Some(Region {
        x0: rx0,
        y0: ry0,
        w,
        h,
        inside,
        d_in,
        d_out,
        span,
    })
}

impl Region {
    fn px<'a>(&self, img: &'a RgbaImage, x: usize, y: usize) -> &'a Rgba<u8> {
        img.get_pixel(self.x0 + x as u32, self.y0 + y as u32)
    }

    fn grad(&self, img: &RgbaImage, x: usize, y: usize) -> f32 {
        let l = |x: usize, y: usize| luma(self.px(img, x, y));
        let xm = x.saturating_sub(1);
        let xp = (x + 1).min(self.w - 1);
        let ym = y.saturating_sub(1);
        let yp = (y + 1).min(self.h - 1);
        let gx = (l(xp, y) - l(xm, y)) * 0.5;
        let gy = (l(x, yp) - l(x, ym)) * 0.5;
        (gx * gx + gy * gy).sqrt()
    }

    /// Mean gradient inside (2+ px from the edge) or over `pick`.
    fn mean_grad(&self, img: &RgbaImage, pick: impl Fn(usize) -> bool) -> f32 {
        let (mut s, mut n) = (0.0f64, 0u64);
        for y in 0..self.h {
            for x in 0..self.w {
                if pick(y * self.w + x) {
                    s += self.grad(img, x, y) as f64;
                    n += 1;
                }
            }
        }
        if n == 0 { 0.0 } else { (s / n as f64) as f32 }
    }

    fn texture_inside(&self, img: &RgbaImage) -> f32 {
        self.mean_grad(img, |i| self.inside[i] && self.d_in[i] >= 2.0)
    }

    /// Mean colour step across the edge (inside pixel to its outside
    /// 4-neighbour) over the mean step between neighbours 2-6 px outside.
    fn seam(&self, img: &RgbaImage) -> f32 {
        let (mut edge, mut ne) = (0.0f64, 0u64);
        let (mut refs, mut nr) = (0.0f64, 0u64);
        for y in 0..self.h - 1 {
            for x in 0..self.w - 1 {
                let i = y * self.w + x;
                for j in [i + 1, i + self.w] {
                    let (a, b) = (
                        oklab(self.px(img, i % self.w, i / self.w)),
                        oklab(self.px(img, j % self.w, j / self.w)),
                    );
                    if self.inside[i] != self.inside[j] {
                        edge += de(a, b) as f64;
                        ne += 1;
                    } else if !self.inside[i]
                        && (2.0..=6.0).contains(&self.d_out[i])
                        && (2.0..=6.0).contains(&self.d_out[j])
                    {
                        refs += de(a, b) as f64;
                        nr += 1;
                    }
                }
            }
        }
        if ne == 0 || nr == 0 || refs == 0.0 {
            return 0.0;
        }
        ((edge / ne as f64) / (refs / nr as f64)) as f32
    }

    fn mean_de(&self, a: &RgbaImage, b: &RgbaImage) -> f32 {
        let (mut s, mut n) = (0.0f64, 0u64);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.inside[y * self.w + x] {
                    s += de(oklab(self.px(a, x, y)), oklab(self.px(b, x, y))) as f64;
                    n += 1;
                }
            }
        }
        (s / n.max(1) as f64) as f32
    }

    /// SSIM of luma on 7x7 windows, averaged over the selection.
    fn ssim(&self, a: &RgbaImage, b: &RgbaImage) -> f32 {
        let (w, h) = (self.w, self.h);
        let sat = |f: &dyn Fn(usize, usize) -> f64| {
            let mut t = vec![0.0f64; (w + 1) * (h + 1)];
            for y in 0..h {
                let mut row = 0.0;
                for x in 0..w {
                    row += f(x, y);
                    t[(y + 1) * (w + 1) + x + 1] = t[y * (w + 1) + x + 1] + row;
                }
            }
            t
        };
        let la = |x: usize, y: usize| luma(self.px(a, x, y)) as f64;
        let lb = |x: usize, y: usize| luma(self.px(b, x, y)) as f64;
        let sa = sat(&|x, y| la(x, y));
        let sb = sat(&|x, y| lb(x, y));
        let saa = sat(&|x, y| la(x, y) * la(x, y));
        let sbb = sat(&|x, y| lb(x, y) * lb(x, y));
        let sab = sat(&|x, y| la(x, y) * lb(x, y));
        let boxsum = |t: &[f64], x0: usize, y0: usize, x1: usize, y1: usize| {
            t[y1 * (w + 1) + x1] + t[y0 * (w + 1) + x0]
                - t[y0 * (w + 1) + x1]
                - t[y1 * (w + 1) + x0]
        };
        let (c1, c2) = (0.01f64.powi(2), 0.03f64.powi(2));
        let (mut s, mut n) = (0.0f64, 0u64);
        for y in 0..h {
            for x in 0..w {
                if !self.inside[y * w + x] {
                    continue;
                }
                let (x0, y0) = (x.saturating_sub(3), y.saturating_sub(3));
                let (x1, y1) = ((x + 4).min(w), (y + 4).min(h));
                let k = ((x1 - x0) * (y1 - y0)) as f64;
                let ma = boxsum(&sa, x0, y0, x1, y1) / k;
                let mb = boxsum(&sb, x0, y0, x1, y1) / k;
                let va = boxsum(&saa, x0, y0, x1, y1) / k - ma * ma;
                let vb = boxsum(&sbb, x0, y0, x1, y1) / k - mb * mb;
                let cab = boxsum(&sab, x0, y0, x1, y1) / k - ma * mb;
                s += ((2.0 * ma * mb + c1) * (2.0 * cab + c2))
                    / ((ma * ma + mb * mb + c1) * (va + vb + c2));
                n += 1;
            }
        }
        (s / n.max(1) as f64) as f32
    }

    /// Unclipped, bright (sky-like) pixels 8..(8+band) px outside.
    fn reference(&self, img: &RgbaImage) -> Vec<bool> {
        let band = (self.span as f32 / 4.0).clamp(32.0, 256.0);
        (0..self.w * self.h)
            .map(|i| {
                if self.inside[i] || !(8.0..=8.0 + band).contains(&self.d_out[i]) {
                    return false;
                }
                let p = self.px(img, i % self.w, i / self.w);
                p[0].min(p[1]).min(p[2]) < 250 && oklab(p)[0] >= 0.55
            })
            .collect()
    }

    fn mean_lab(&self, img: &RgbaImage, pick: &dyn Fn(usize) -> bool) -> [f32; 3] {
        let mut s = [0.0f64; 3];
        let mut n = 0u64;
        for i in 0..self.w * self.h {
            if pick(i) {
                let c = oklab(self.px(img, i % self.w, i / self.w));
                for k in 0..3 {
                    s[k] += c[k] as f64;
                }
                n += 1;
            }
        }
        let n = n.max(1) as f64;
        [(s[0] / n) as f32, (s[1] / n) as f32, (s[2] / n) as f32]
    }

    fn clipped_share(&self, img: &RgbaImage) -> f32 {
        let (mut c, mut n) = (0u64, 0u64);
        for i in 0..self.w * self.h {
            if self.inside[i] {
                let p = self.px(img, i % self.w, i / self.w);
                c += u64::from(p[0].min(p[1]).min(p[2]) >= 250);
                n += 1;
            }
        }
        c as f32 / n.max(1) as f32
    }
}

// ---------------------------------------------------------------- sheets

fn fit(img: &RgbaImage, w: u32) -> RgbaImage {
    let h = (img.height() as f64 * w as f64 / img.width() as f64)
        .round()
        .max(1.0) as u32;
    image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle)
}

fn overlay(img: &RgbaImage, mask: &GrayImage) -> RgbaImage {
    let mut out = img.clone();
    for (x, y, p) in out.enumerate_pixels_mut() {
        let m = mask.get_pixel(x, y)[0] as f32 / 255.0 * 0.55;
        p[0] = (p[0] as f32 * (1.0 - m) + 255.0 * m) as u8;
        p[1] = (p[1] as f32 * (1.0 - m)) as u8;
        p[2] = (p[2] as f32 * (1.0 - m) + 60.0 * m) as u8;
    }
    out
}

/// Columns of panels stacked into rows, on a dark ground.
fn sheet(rows: &[Vec<RgbaImage>]) -> RgbaImage {
    let gap = 6;
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let row_h: Vec<u32> = rows
        .iter()
        .map(|r| r.iter().map(|p| p.height()).max().unwrap_or(0))
        .collect();
    let w = cols * (PANEL_W + gap) + gap;
    let h = row_h.iter().map(|h| h + gap).sum::<u32>() + gap;
    let mut out = RgbaImage::from_pixel(w, h, Rgba([24, 24, 24, 255]));
    let mut y = gap;
    for (r, row) in rows.iter().enumerate() {
        for (c, p) in row.iter().enumerate() {
            image::imageops::overlay(
                &mut out,
                p,
                (gap + c as u32 * (PANEL_W + gap)) as i64,
                y as i64,
            );
        }
        y += row_h[r] + gap;
    }
    out
}

/// Bounding box of the selection with a margin, for the zoomed row.
fn zoom_box(mask: &GrayImage) -> (u32, u32, u32, u32) {
    let (w, h) = mask.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, v) in mask.enumerate_pixels() {
        if v[0] > 127 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    let m = ((x1 - x0).max(y1 - y0) / 6).max(24);
    let (x0, y0) = (x0.saturating_sub(m), y0.saturating_sub(m));
    let (x1, y1) = ((x1 + m).min(w - 1), (y1 + m).min(h - 1));
    (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
}

/// A 1:1 window straddling the selection's left edge at its middle row.
fn detail_box(mask: &GrayImage) -> (u32, u32, u32, u32) {
    let (w, h) = mask.dimensions();
    let (bx, by, bw, bh) = zoom_box(mask);
    let cy = by + bh / 2;
    let mut ex = bx;
    for x in bx..bx + bw {
        if mask.get_pixel(x, cy)[0] > 127 {
            ex = x;
            break;
        }
    }
    let side = PANEL_W.min(w).min(h);
    let x0 = ex.saturating_sub(side / 3).min(w - side);
    let y0 = cy.saturating_sub(side / 2).min(h - side);
    (x0, y0, side, side)
}

fn crop(img: &RgbaImage, b: (u32, u32, u32, u32)) -> RgbaImage {
    image::imageops::crop_imm(img, b.0, b.1, b.2, b.3).to_image()
}

// ---------------------------------------------------------------- run

struct Row {
    case: String,
    kind: String,
    method: String,
    secs: f32,
    status: String,
    cols: Vec<(String, f32)>,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(
        args.len() >= 3,
        "usage: fill_testset <cases.txt> <run dir> [options]"
    );
    let cases_path = PathBuf::from(&args[1]);
    let run = PathBuf::from(&args[2]);
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let real_home = PathBuf::from(std::env::var("HOME")?);
    let photos = opt("--photos")
        .map(PathBuf::from)
        .unwrap_or_else(|| real_home.join("Desktop/Test Photos"));
    let engine_dir = opt("--engine")
        .map(PathBuf::from)
        .unwrap_or_else(|| real_home.join("Documents/Darkroom Index/Models/Engine"));
    let methods: Vec<String> = opt("--methods")
        .unwrap_or_else(|| "lama,ca,flux".into())
        .split(',')
        .map(str::to_string)
        .collect();
    let only: Option<Vec<String>> =
        opt("--only").map(|s| s.split(',').map(str::to_string).collect());

    std::fs::create_dir_all(run.join("sheets"))?;
    std::fs::create_dir_all(run.join("results"))?;
    let home = private_env(&run, &real_home, &engine_dir)?;
    // SAFETY: still single-threaded; nothing has read the environment.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("DARKROOM_ENGINE_PORT", ENGINE_PORT.to_string());
        std::env::set_var("DARKROOM_FILL_SEED", FLUX_SEED);
    }
    fern::Dispatch::new()
        .level(log::LevelFilter::Info)
        .format(|out, msg, rec| out.finish(format_args!("{} {}", rec.level(), msg)))
        .chain(fern::log_file(run.join("fill.log"))?)
        .apply()?;

    let cases: Vec<Case> = parse_cases(&cases_path)?
        .into_iter()
        .filter(|c| only.as_ref().is_none_or(|o| o.contains(&c.id)))
        .collect();
    let wants_flux = methods.iter().any(|m| m == "flux")
        && cases.iter().any(|c| c.methods.iter().any(|m| m == "flux"));
    let _engine = if wants_flux {
        match start_engine(&engine_dir, &run.join(".env")) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("[testset] Flux skipped: {e}");
                None
            }
        }
    } else {
        None
    };
    let flux_ok = _engine.is_some();

    let app = tauri::Builder::default()
        .manage(darkroom_index::AppState::default())
        .build(darkroom_index::tauri_context())?;
    let handle = app.handle().clone();

    let mut rows: Vec<Row> = Vec::new();
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>Fill test set</title>\
         <style>body{background:#111;color:#ddd;font:13px system-ui;margin:16px}\
         img{max-width:100%}table{border-collapse:collapse;margin:8px 0}\
         td,th{border:1px solid #444;padding:3px 8px;text-align:right}\
         th{background:#222}h2{margin-top:32px}</style><h1>Fill test set</h1>",
    );

    for case in &cases {
        eprintln!("[testset] {} ({})", case.id, case.photo);
        let path = photos.join(&case.photo);
        let bytes = std::fs::read(&path)?;
        let original = darkroom_index::image_loader::load_base_image_from_bytes(
            &bytes,
            &path.to_string_lossy(),
            false,
            &darkroom_index::AppSettings::default(),
            None,
        )?;
        let truth = original.to_rgba8();
        let (w, h) = truth.dimensions();
        let path_str = path.to_string_lossy().to_string();

        // The selection, and the photo the fill sees.
        let (opened, sub_masks, hole) = match case.kind.as_str() {
            "holdout" => {
                let hole = blob_mask(w, h, case);
                // The hole's own pixels are painted out so no method can
                // read the answer, all but a 2 px rim: a selection's edge is
                // composited at part strength, and there a removed object's
                // own edge would show through, not a flat grey line.
                let rim = {
                    let (w, h) = (w as usize, h as usize);
                    let inside: Vec<bool> = hole.as_raw().iter().map(|v| *v > 0).collect();
                    dist_to(&inside, w, h, true)
                };
                let mut damaged = truth.clone();
                for (x, y, v) in hole.enumerate_pixels() {
                    if v[0] > 0 && rim[(y * w + x) as usize] > 2.0 {
                        damaged.put_pixel(x, y, Rgba([128, 128, 128, 255]));
                    }
                }
                (
                    DynamicImage::ImageRgba8(damaged),
                    vec![raster_submask("hole", &hole, "additive")],
                    Some(hole),
                )
            }
            "clipped" => {
                let white = pf(case, "white");
                (
                    original.clone(),
                    vec![
                        json!({
                            "id": "clip", "type": "clipped", "visible": true, "invert": false,
                            "opacity": 100.0, "mode": "additive",
                            "parameters": {"whiteThreshold": white, "blackThreshold": 0.0,
                                           "solidify": 40.0, "clean": 12.0, "grow": 0.0,
                                           "feather": 10.0}
                        }),
                        raster_submask("roi", &roi_mask(w, h, case), "intersect"),
                    ],
                    None,
                )
            }
            k => anyhow::bail!("unknown kind {k}"),
        };
        let opened_rgba = opened.to_rgba8();
        darkroom_index::fill_harness::open_photo(&handle, &path_str, opened.clone());

        let mut results: Vec<(String, RgbaImage)> = Vec::new();
        let mut used_mask: Option<GrayImage> = hole.clone();
        for method in &case.methods {
            if !methods.contains(method) {
                continue;
            }
            if method == "flux" && !flux_ok {
                rows.push(Row {
                    case: case.id.clone(),
                    kind: case.kind.clone(),
                    method: method.clone(),
                    secs: 0.0,
                    status: "skipped (no engine)".into(),
                    cols: vec![],
                });
                continue;
            }
            let patch = json!({
                "id": format!("{}-{method}", case.id),
                "name": "test", "visible": true, "invert": false, "prompt": "",
                // The Retouch panel's default for a Clipped selection runs
                // the engine as one pass; fast/content-aware turn it off.
                "reconstructSinglePath": case.kind == "clipped" && method == "flux",
                "generateMode": false,
                "subMasks": sub_masks,
            });
            let (fast, engine) = match method.as_str() {
                "lama" => (true, None),
                "ca" => (false, Some("content-aware")),
                _ => (false, None),
            };
            log::info!("==== {} / {method} ====", case.id);
            let started = Instant::now();
            let out = tauri::async_runtime::block_on(darkroom_index::fill_harness::run_fill(
                &handle,
                &path_str,
                patch,
                json!({"aiPatches": []}),
                fast,
                engine,
            ));
            let secs = started.elapsed().as_secs_f32();
            let payload: Value = match out {
                Ok(s) => serde_json::from_str(&s)?,
                Err(e) => {
                    eprintln!("[testset]   {method}: error {e}");
                    rows.push(Row {
                        case: case.id.clone(),
                        kind: case.kind.clone(),
                        method: method.clone(),
                        secs,
                        status: format!("error: {e}"),
                        cols: vec![],
                    });
                    continue;
                }
            };
            if payload.get("needsPrompt").and_then(Value::as_bool) == Some(true) {
                let pct = payload["usableFraction"].as_f64().unwrap_or(0.0) * 100.0;
                rows.push(Row {
                    case: case.id.clone(),
                    kind: case.kind.clone(),
                    method: method.clone(),
                    secs,
                    status: format!("needs prompt ({pct:.0}% usable)"),
                    cols: vec![],
                });
                continue;
            }
            // Composite the way the editor does.
            let adjustments = json!({"aiPatches": [{
                "id": "p", "visible": true, "patchData": payload,
            }]});
            let composite =
                darkroom_index::image_loader::composite_patches_on_image(&opened, &adjustments)?
                    .to_rgba8();
            if used_mask.is_none() {
                let m = payload["mask"].as_str().unwrap_or_default();
                let bytes = general_purpose::STANDARD.decode(m)?;
                used_mask = Some(image::load_from_memory(&bytes)?.to_luma8());
            }
            composite.save(run.join(format!("results/{}-{method}.png", case.id)))?;
            eprintln!("[testset]   {method}: {secs:.1}s");
            results.push((method.clone(), composite));
            rows.push(Row {
                case: case.id.clone(),
                kind: case.kind.clone(),
                method: method.clone(),
                secs,
                status: "ok".into(),
                cols: vec![],
            });
        }

        let Some(mask) = used_mask else {
            continue;
        };
        mask.save(run.join(format!("results/{}-selection.png", case.id)))?;
        let Some(reg) = region(&mask) else {
            eprintln!("[testset]   empty selection");
            continue;
        };
        let frac = reg.inside.iter().filter(|b| **b).count() as f32 / (w * h) as f32;
        // Scores, per method row.
        let truth_tex = reg.texture_inside(&truth);
        let truth_seam = reg.seam(&truth);
        let refmask = reg.reference(&opened_rgba);
        let ref_tex = reg.mean_grad(&opened_rgba, |i| refmask[i]);
        let ref_lab = reg.mean_lab(&opened_rgba, &|i| refmask[i]);
        for (method, img) in &results {
            let row = rows
                .iter_mut()
                .rev()
                .find(|r| r.case == case.id && &r.method == method)
                .unwrap();
            row.cols.push(("sel_%".into(), frac * 100.0));
            if case.kind == "holdout" {
                row.cols.push(("dE".into(), reg.mean_de(img, &truth)));
                row.cols.push(("ssim".into(), reg.ssim(img, &truth)));
                row.cols.push((
                    "texture".into(),
                    reg.texture_inside(img) / truth_tex.max(1e-6),
                ));
                row.cols.push(("seam".into(), reg.seam(img)));
                row.cols.push(("seam_truth".into(), truth_seam));
            } else {
                row.cols.push((
                    "texture_vs_ref".into(),
                    reg.texture_inside(img) / ref_tex.max(1e-6),
                ));
                row.cols.push(("seam".into(), reg.seam(img)));
                let inside = |i: usize| reg.inside[i];
                row.cols.push((
                    "dE_mean_vs_ref".into(),
                    de(reg.mean_lab(img, &inside), ref_lab),
                ));
                row.cols
                    .push(("clipped_%".into(), reg.clipped_share(img) * 100.0));
            }
        }

        // Sheet: full frames, the selection's box, a 1:1 window on its edge.
        let zb = zoom_box(&mask);
        let db = detail_box(&mask);
        let first: &RgbaImage = if case.kind == "holdout" {
            &truth
        } else {
            &opened_rgba
        };
        let mut full = vec![
            fit(first, PANEL_W),
            fit(&overlay(&opened_rgba, &mask), PANEL_W),
        ];
        let mut zoom = vec![
            fit(&crop(first, zb), PANEL_W),
            fit(&crop(&overlay(&opened_rgba, &mask), zb), PANEL_W),
        ];
        let mut detail = vec![crop(first, db), crop(&overlay(&opened_rgba, &mask), db)];
        for (_, img) in &results {
            full.push(fit(img, PANEL_W));
            zoom.push(fit(&crop(img, zb), PANEL_W));
            detail.push(crop(img, db));
        }
        let sheet_img = sheet(&[full, zoom, detail]);
        let sheet_name = format!("sheets/{}.png", case.id);
        sheet_img.save(run.join(&sheet_name))?;

        let _ = write!(
            html,
            "<h2>{} — {} ({})</h2><p>Columns: {} | selection | {}. Rows: whole frame; the selection's box; 1:1 at its left edge.</p><img src='{}'>",
            case.id,
            case.photo,
            case.kind,
            if case.kind == "holdout" {
                "truth"
            } else {
                "original"
            },
            results
                .iter()
                .map(|(m, _)| m.as_str())
                .collect::<Vec<_>>()
                .join(" | "),
            sheet_name
        );
        let case_rows: Vec<&Row> = rows.iter().filter(|r| r.case == case.id).collect();
        html.push_str("<table><tr><th>method</th><th>status</th><th>secs</th>");
        if let Some(r) = case_rows.iter().find(|r| !r.cols.is_empty()) {
            for (k, _) in &r.cols {
                let _ = write!(html, "<th>{k}</th>");
            }
        }
        html.push_str("</tr>");
        for r in case_rows {
            let _ = write!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{:.1}</td>",
                r.method, r.status, r.secs
            );
            for (_, v) in &r.cols {
                let _ = write!(html, "<td>{v:.3}</td>");
            }
            html.push_str("</tr>");
        }
        html.push_str("</table>");
        std::fs::write(run.join("index.html"), &html)?;
    }

    // Tables.
    let mut csv = String::from("case,kind,method,status,secs,metric,value\n");
    let mut md = String::from(
        "| case | method | secs | sel % | ΔE | SSIM | texture | seam | seam (truth) |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    let mut md_clip = String::from(
        "| case | method | secs | sel % | texture vs ref | seam | ΔE mean vs ref | clipped % |\n|---|---|---|---|---|---|---|---|\n",
    );
    for r in &rows {
        if r.cols.is_empty() {
            let _ = writeln!(
                csv,
                "{},{},{},{},{:.2},,",
                r.case,
                r.kind,
                r.method,
                r.status.replace(',', ";"),
                r.secs
            );
        }
        for (k, v) in &r.cols {
            let _ = writeln!(
                csv,
                "{},{},{},{},{:.2},{k},{v:.4}",
                r.case, r.kind, r.method, r.status, r.secs
            );
        }
        let vals: Vec<String> = r.cols.iter().map(|(_, v)| format!("{v:.3}")).collect();
        let line = if r.cols.is_empty() {
            format!(
                "| {} | {} | {:.1} | {} |\n",
                r.case, r.method, r.secs, r.status
            )
        } else {
            format!(
                "| {} | {} | {:.1} | {} |\n",
                r.case,
                r.method,
                r.secs,
                vals.join(" | ")
            )
        };
        if r.kind == "holdout" {
            md.push_str(&line);
        } else {
            md_clip.push_str(&line);
        }
    }
    std::fs::write(run.join("metrics.csv"), csv)?;
    std::fs::write(
        run.join("metrics.md"),
        format!("## Hold-out (truth known)\n\n{md}\n## Clipped / blown (no truth)\n\n{md_clip}"),
    )?;
    html.push_str("<h2>All metrics</h2><p>See metrics.md / metrics.csv.</p>");
    std::fs::write(run.join("index.html"), &html)?;
    eprintln!("[testset] wrote {}", run.join("index.html").display());
    // Stop the private engine, then leave without running the Tauri and
    // ONNX runtime teardown (which aborts on exit outside an event loop).
    drop(_engine);
    std::process::exit(0)
}
