// Flat-field correction: cancel a fixed rig's illumination falloff by
// dividing each photo by a "master flat" reference frame in linear light.
//
// Profiles live in ~/Documents/RapidRAW Models/flats/<name>/ as a 16-bit
// linear-encoded flat.png plus profile.json (stats + provenance). The
// divide runs at the head of the geometry-warp stage (see
// image_processing::apply_geometry_warp), before distortion/rotation/crop,
// because the flat was shot through the same optics as the photos.

use crate::app_settings::load_settings;
use crate::formats::is_raw_file;
use crate::image_loader::load_base_image_from_bytes;
use crate::image_processing::apply_cpu_default_raw_processing;
use image::{DynamicImage, GenericImageView, Rgb32FImage, imageops::FilterType};
use once_cell::sync::Lazy;
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Divisor floor: caps the recovery boost at ~5.6 stops so rig edges that
/// sit outside the light cone don't explode into noise.
pub const FLAT_FLOOR: f32 = 0.02;

const FLAT_LONG_EDGE: u32 = 2048;
const FLAT_BLUR_SIGMA: f32 = 3.0;
const NORMALIZE_PERCENTILE: f32 = 0.995;
const FALLOFF_PERCENTILE: f32 = 0.005;

#[inline]
fn srgb_to_linear(x: f32) -> f32 {
    let x = x.max(0.0);
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(x: f32) -> f32 {
    let x = x.max(0.0);
    if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

fn flats_root() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    Ok(PathBuf::from(home).join("Documents/RapidRAW Models/flats"))
}

fn sanitize_profile_name(name: &str) -> Result<String, String> {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_' || *c == '.')
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() || cleaned.starts_with('.') {
        return Err("Profile name must contain letters or numbers".to_string());
    }
    Ok(cleaned)
}

fn percentile(values: &mut [f32], q: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let idx = ((values.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
    let (_, v, _) = values.select_nth_unstable_by(idx, |a, b| a.partial_cmp(b).unwrap());
    *v
}

#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

pub struct MasterFlatStats {
    pub falloff_stops: f32,
    pub clipped_percent: f32,
    pub frames: usize,
}

/// Average linearized flat frames into a normalized master flat.
/// Each frame is exposure-normalized by its own bright percentile before
/// averaging so bracketed references average shape, not brightness. The
/// result is blurred (reference noise must not divide into photos) and
/// per-channel normalized so the brightest region sits at gain 1.0 with
/// no global white-balance shift.
pub fn build_master_flat(
    frames: Vec<Rgb32FImage>,
) -> Result<(Rgb32FImage, MasterFlatStats), String> {
    let first = frames.first().ok_or("No flat frames provided")?;
    let (w, h) = (first.width(), first.height());
    let n_samples = (w * h * 3) as usize;
    let frame_count = frames.len();

    let mut clipped: u64 = 0;
    let mut total_px: u64 = 0;
    let mut sum = vec![0.0f32; n_samples];

    for frame in &frames {
        if frame.width() != w || frame.height() != h {
            return Err("Flat frames have mismatched dimensions".to_string());
        }
        let data = frame.as_raw();
        total_px += (w * h) as u64;
        clipped += data
            .chunks_exact(3)
            .filter(|p| p[0] >= 0.98 || p[1] >= 0.98 || p[2] >= 0.98)
            .count() as u64;

        let mut lumas: Vec<f32> = data
            .chunks_exact(3)
            .map(|p| luma(p[0], p[1], p[2]))
            .collect();
        let norm = percentile(&mut lumas, NORMALIZE_PERCENTILE).max(1e-6);
        let inv = 1.0 / norm;
        sum.iter_mut()
            .zip(data.iter())
            .for_each(|(s, v)| *s += v * inv);
    }

    let inv_n = 1.0 / frame_count as f32;
    sum.iter_mut().for_each(|v| *v *= inv_n);

    let averaged = Rgb32FImage::from_raw(w, h, sum).ok_or("Failed to assemble master flat")?;
    let blurred = image::imageops::blur(&averaged, FLAT_BLUR_SIGMA);

    let mut master = blurred;
    for c in 0..3 {
        let mut channel: Vec<f32> = master.as_raw().iter().skip(c).step_by(3).copied().collect();
        let norm = percentile(&mut channel, NORMALIZE_PERCENTILE).max(1e-6);
        let inv = 1.0 / norm;
        master
            .as_mut()
            .iter_mut()
            .skip(c)
            .step_by(3)
            .for_each(|v| *v = (*v * inv).clamp(1e-4, 1.0));
    }

    let mut lumas: Vec<f32> = master
        .as_raw()
        .chunks_exact(3)
        .map(|p| luma(p[0], p[1], p[2]))
        .collect();
    let dark = percentile(&mut lumas, FALLOFF_PERCENTILE).max(1e-4);
    let stats = MasterFlatStats {
        falloff_stops: -dark.log2(),
        clipped_percent: 100.0 * clipped as f32 / total_px.max(1) as f32,
        frames: frame_count,
    };
    Ok((master, stats))
}

/// Decode one flat frame the same way the photo pipeline prepares images
/// for the warp stage (RAW default processing included), then linearize.
/// Consistency with the apply path matters more than absolute radiometry:
/// both sides use the same sRGB linearization, so the falloff cancels.
fn decode_flat_frame(
    path: &str,
    settings: &crate::app_settings::AppSettings,
) -> Result<Rgb32FImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read {path}: {e}"))?;
    let mut img = load_base_image_from_bytes(&bytes, path, false, settings, None)
        .map_err(|e| format!("Failed to decode {path}: {e}"))?;
    if is_raw_file(path) {
        apply_cpu_default_raw_processing(&mut img);
    }
    let (w, h) = img.dimensions();
    let long_edge = w.max(h);
    let img = if long_edge > FLAT_LONG_EDGE {
        let scale = FLAT_LONG_EDGE as f32 / long_edge as f32;
        img.resize(
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
            FilterType::Triangle,
        )
    } else {
        img
    };
    let mut rgb = img.to_rgb32f();
    rgb.as_mut()
        .par_iter_mut()
        .for_each(|v| *v = srgb_to_linear(*v));
    Ok(rgb)
}

type ResizedFlatCache = HashMap<(String, u32, u32), Arc<Vec<f32>>>;

static MASTER_CACHE: Lazy<Mutex<HashMap<String, Arc<Rgb32FImage>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static RESIZED_CACHE: Lazy<Mutex<ResizedFlatCache>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn invalidate_profile_caches(profile: &str) {
    MASTER_CACHE.lock().unwrap().remove(profile);
    RESIZED_CACHE
        .lock()
        .unwrap()
        .retain(|(p, _, _), _| p != profile);
}

fn load_master(profile: &str) -> Option<Arc<Rgb32FImage>> {
    if let Some(m) = MASTER_CACHE.lock().unwrap().get(profile) {
        return Some(Arc::clone(m));
    }
    let path = flats_root().ok()?.join(profile).join("flat.png");
    let img = image::open(&path)
        .map_err(|e| log::warn!("[flat] failed to load master flat {path:?}: {e}"))
        .ok()?;
    let rgb16 = img.into_rgb16();
    let (w, h) = (rgb16.width(), rgb16.height());
    let data: Vec<f32> = rgb16.as_raw().iter().map(|v| *v as f32 / 65535.0).collect();
    let master = Arc::new(Rgb32FImage::from_raw(w, h, data)?);
    MASTER_CACHE
        .lock()
        .unwrap()
        .insert(profile.to_string(), Arc::clone(&master));
    Some(master)
}

fn resized_flat(profile: &str, w: u32, h: u32) -> Option<Arc<Vec<f32>>> {
    let key = (profile.to_string(), w, h);
    if let Some(f) = RESIZED_CACHE.lock().unwrap().get(&key) {
        return Some(Arc::clone(f));
    }
    let master = load_master(profile)?;
    let resized = image::imageops::resize(master.as_ref(), w, h, FilterType::Triangle);
    let flat = Arc::new(resized.into_raw());
    let mut cache = RESIZED_CACHE.lock().unwrap();
    if cache.len() >= 6 {
        cache.clear();
    }
    cache.insert(key, Arc::clone(&flat));
    Some(flat)
}

/// Core divide: linearize, divide by the strength-blended flat (floored),
/// re-encode. `flat` must hold w*h*3 linear gain-domain samples.
pub fn apply_flat_to_image(image: &mut DynamicImage, flat: &[f32], strength: f32) {
    let inv_strength = 1.0 - strength;
    macro_rules! process {
        ($img:expr, $ch:expr, $to_f:expr, $from_f:expr) => {{
            $img.as_mut()
                .par_chunks_mut($ch)
                .enumerate()
                .for_each(|(i, px)| {
                    let base = i * 3;
                    for c in 0..3 {
                        let f = flat[base + c];
                        let denom = (inv_strength + f * strength).max(FLAT_FLOOR);
                        let v = $to_f(px[c]);
                        let l = srgb_to_linear(v) / denom;
                        px[c] = $from_f(linear_to_srgb(l));
                    }
                });
        }};
    }
    let u8_to_f = |v: u8| v as f32 / 255.0;
    let f_to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let id_to_f = |v: f32| v;
    let f_to_id = |v: f32| v.max(0.0);
    match image {
        DynamicImage::ImageRgb8(img) => process!(img, 3, u8_to_f, f_to_u8),
        DynamicImage::ImageRgba8(img) => process!(img, 4, u8_to_f, f_to_u8),
        DynamicImage::ImageRgb32F(img) => process!(img, 3, id_to_f, f_to_id),
        DynamicImage::ImageRgba32F(img) => process!(img, 4, id_to_f, f_to_id),
        other => {
            let mut rgba = other.to_rgba8();
            rgba.as_mut()
                .par_chunks_mut(4)
                .enumerate()
                .for_each(|(i, px)| {
                    let base = i * 3;
                    for c in 0..3 {
                        let f = flat[base + c];
                        let denom = (inv_strength + f * strength).max(FLAT_FLOOR);
                        let l = srgb_to_linear(px[c] as f32 / 255.0) / denom;
                        px[c] = (linear_to_srgb(l) * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                });
            *other = DynamicImage::ImageRgba8(rgba);
        }
    }
}

/// Entry point for the pipeline: applies the profile named in the
/// adjustments (if any) to the unwarped sensor-frame image. Passthrough
/// (borrow preserved) when no profile is set or strength is 0.
pub fn apply_flat_field<'a>(
    image: Cow<'a, DynamicImage>,
    adjustments: &serde_json::Value,
) -> Cow<'a, DynamicImage> {
    let profile = match adjustments.get("flatFieldProfile").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p,
        _ => return image,
    };
    let strength = (adjustments
        .get("flatFieldStrength")
        .and_then(|v| v.as_f64())
        .unwrap_or(100.0) as f32
        / 100.0)
        .clamp(0.0, 1.0);
    if strength <= 0.0 {
        return image;
    }
    let (w, h) = image.dimensions();
    let Some(flat) = resized_flat(profile, w, h) else {
        log::warn!("[flat] profile '{profile}' missing or unreadable; skipping correction");
        return image;
    };
    let start = std::time::Instant::now();
    let mut out = image.into_owned();
    apply_flat_to_image(&mut out, &flat, strength);
    log::info!(
        "[flat] applied '{profile}' at {:.0}% to {w}x{h} in {:.2?}",
        strength * 100.0,
        start.elapsed()
    );
    Cow::Owned(out)
}

#[tauri::command]
pub async fn create_flat_profile(
    app_handle: tauri::AppHandle,
    name: String,
    source_paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    let name = sanitize_profile_name(&name)?;
    if source_paths.is_empty() {
        return Err("Select at least one flat frame".to_string());
    }
    let settings = load_settings(app_handle).unwrap_or_default();

    tokio::task::spawn_blocking(move || {
        log::info!(
            "[flat] building profile '{name}' from {} frames",
            source_paths.len()
        );
        let mut frames = Vec::with_capacity(source_paths.len());
        let mut target_dims: Option<(u32, u32)> = None;
        for path in &source_paths {
            let frame = decode_flat_frame(path, &settings)?;
            let frame = match target_dims {
                None => {
                    target_dims = Some((frame.width(), frame.height()));
                    frame
                }
                Some((tw, th)) if frame.width() != tw || frame.height() != th => {
                    image::imageops::resize(&frame, tw, th, FilterType::Triangle)
                }
                _ => frame,
            };
            frames.push(frame);
        }

        let (master, stats) = build_master_flat(frames)?;

        let dir = flats_root()?.join(&name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let (w, h) = (master.width(), master.height());
        let data16: Vec<u16> = master
            .as_raw()
            .iter()
            .map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
            .collect();
        let img16 = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_raw(w, h, data16)
            .ok_or("Failed to encode master flat")?;
        DynamicImage::ImageRgb16(img16)
            .save(dir.join("flat.png"))
            .map_err(|e| e.to_string())?;

        let sources: Vec<String> = source_paths
            .iter()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.clone())
            })
            .collect();
        let profile = serde_json::json!({
            "name": name,
            "frames": stats.frames,
            "sources": sources,
            "falloffStops": (stats.falloff_stops * 10.0).round() / 10.0,
            "clippedPercent": (stats.clipped_percent * 100.0).round() / 100.0,
            "createdAt": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            "encoding": "linear16",
        });
        std::fs::write(
            dir.join("profile.json"),
            serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;

        invalidate_profile_caches(&name);
        log::info!(
            "[flat] profile '{name}' saved: {:.1} stops falloff, {:.2}% clipped, {} frames",
            stats.falloff_stops,
            stats.clipped_percent,
            stats.frames
        );
        Ok(profile)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_flat_profiles() -> Result<Vec<serde_json::Value>, String> {
    let root = flats_root()?;
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|d| d.path().is_dir())
        .collect();
    dirs.sort_by_key(|d| d.file_name());
    for dir in dirs {
        let meta_path = dir.path().join("profile.json");
        if !dir.path().join("flat.png").is_file() {
            continue;
        }
        let mut profile = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        profile["name"] = serde_json::json!(dir.file_name().to_string_lossy());
        out.push(profile);
    }
    Ok(out)
}

#[tauri::command]
pub async fn delete_flat_profile(name: String) -> Result<(), String> {
    let name = sanitize_profile_name(&name)?;
    if name.contains('/') || name.contains('\\') {
        return Err("Invalid profile name".to_string());
    }
    let dir = flats_root()?.join(&name);
    if !dir.join("flat.png").is_file() {
        return Err(format!("Profile '{name}' not found"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    invalidate_profile_caches(&name);
    log::info!("[flat] deleted profile '{name}'");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_falloff(w: u32, h: u32) -> Vec<f32> {
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let max_r = (cx * cx + cy * cy).sqrt();
        let mut flat = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let r = (dx * dx + dy * dy).sqrt() / max_r;
                // 1.0 at center falling to 1/16 (4 stops) at the corner.
                let f = 1.0 - r * (1.0 - 1.0 / 16.0);
                flat.extend_from_slice(&[f, f, f]);
            }
        }
        flat
    }

    #[test]
    fn divide_recovers_scene_within_epsilon() {
        let (w, h) = (64u32, 48u32);
        let flat = synthetic_falloff(w, h);
        let scene = 0.5f32;
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for i in 0..(w * h) as usize {
            let f = flat[i * 3];
            let v = (linear_to_srgb(scene * f) * 255.0).round() as u8;
            data.extend_from_slice(&[v, v, v]);
        }
        let mut img = DynamicImage::ImageRgb8(image::ImageBuffer::from_raw(w, h, data).unwrap());
        apply_flat_to_image(&mut img, &flat, 1.0);
        let expected = (linear_to_srgb(scene) * 255.0).round();
        let recovered = img.to_rgb8();
        for p in recovered.pixels() {
            assert!(
                (p[0] as f32 - expected).abs() <= 3.0,
                "corner not recovered: got {} expected {expected}",
                p[0]
            );
        }
    }

    #[test]
    fn strength_zero_is_identity() {
        let (w, h) = (16u32, 16u32);
        let data: Vec<u8> = (0..(w * h * 3)).map(|i| (i % 251) as u8).collect();
        let img = DynamicImage::ImageRgb8(image::ImageBuffer::from_raw(w, h, data).unwrap());
        let adjustments = serde_json::json!({
            "flatFieldProfile": "some-profile",
            "flatFieldStrength": 0
        });
        let out = apply_flat_field(Cow::Borrowed(&img), &adjustments);
        assert!(
            matches!(out, Cow::Borrowed(_)),
            "strength 0 must be a passthrough"
        );
        let adjustments = serde_json::json!({});
        let out = apply_flat_field(Cow::Borrowed(&img), &adjustments);
        assert!(
            matches!(out, Cow::Borrowed(_)),
            "no profile must be a passthrough"
        );
    }

    #[test]
    fn floor_caps_boost_on_dead_black_flat() {
        let (w, h) = (4u32, 4u32);
        let flat = vec![0.001f32; (w * h * 3) as usize];
        let input_linear = 0.0004f32;
        let v = (linear_to_srgb(input_linear) * 255.0).round() as u8;
        let data = vec![v; (w * h * 3) as usize];
        let mut img = DynamicImage::ImageRgb8(image::ImageBuffer::from_raw(w, h, data).unwrap());
        apply_flat_to_image(&mut img, &flat, 1.0);
        // Boost must be capped at 1/FLAT_FLOOR (50x), not the raw 1000x.
        // Expectation starts from the u8-quantized input, like the code does.
        let quantized_linear = srgb_to_linear(v as f32 / 255.0);
        let expected = (linear_to_srgb(quantized_linear / FLAT_FLOOR) * 255.0).round();
        let uncapped = (linear_to_srgb(quantized_linear / 0.001) * 255.0).round();
        let got = img.to_rgb8().get_pixel(0, 0)[0] as f32;
        assert!(
            (got - expected).abs() <= 3.0,
            "got {got}, expected capped {expected}"
        );
        assert!((got - uncapped).abs() > 10.0, "boost was not capped");
    }

    #[test]
    fn master_flat_build_normalizes_and_measures() {
        let (w, h) = (96u32, 64u32);
        let flat_truth = synthetic_falloff(w, h);
        // Two frames of the same falloff at different exposures with noise.
        let mut frames = Vec::new();
        for (exposure, seed) in [(0.6f32, 7u32), (0.9f32, 13u32)] {
            let mut noise_state = seed;
            let data: Vec<f32> = flat_truth
                .iter()
                .map(|f| {
                    noise_state = noise_state.wrapping_mul(1664525).wrapping_add(1013904223);
                    let n = (noise_state >> 16) as f32 / 65535.0 - 0.5;
                    (f * exposure * (1.0 + 0.02 * n)).max(0.0)
                })
                .collect();
            frames.push(Rgb32FImage::from_raw(w, h, data).unwrap());
        }
        let (master, stats) = build_master_flat(frames).unwrap();
        assert_eq!(stats.frames, 2);
        // Normalized: brightest region ~1.0.
        let max = master.as_raw().iter().cloned().fold(0.0f32, f32::max);
        assert!(max > 0.95 && max <= 1.0, "master max {max} not normalized");
        // Falloff measured near the synthetic 4 stops (blur + percentile
        // soften the extreme corner, so accept a broad window).
        assert!(
            stats.falloff_stops > 2.5 && stats.falloff_stops < 4.5,
            "falloff {} not near 4 stops",
            stats.falloff_stops
        );
        assert!(stats.clipped_percent < 1.0);
    }
}
