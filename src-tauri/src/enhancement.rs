use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, Rgb32FImage};
use ndarray::{Array, IxDyn};
use ort::session::Session;
use ort::value::Tensor;
use tauri::{AppHandle, Emitter};

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use crate::formats::is_raw_file;
use crate::image_loader::load_base_image_from_bytes;
use crate::image_processing::apply_cpu_default_raw_processing;
use crate::model_registry::{RegisteredModel, TaskType, resolve_and_prepare};

const DEFAULT_TILE_SIZE: u32 = 512;
const DEFAULT_TILE_OVERLAP: u32 = 16;

/// Builds the NCHW input tensor for a window, replicating the image border
/// where the window extends past it (a no-op for interior windows).
fn window_tensor(
    input: &Rgb32FImage,
    win_x: u32,
    win_y: u32,
    win_w: u32,
    win_h: u32,
) -> Array<f32, ndarray::Ix4> {
    let (w, h) = input.dimensions();
    let mut tensor_data = Array::zeros((1, 3, win_h as usize, win_w as usize));
    for y in 0..win_h {
        for x in 0..win_w {
            let p = input.get_pixel((win_x + x).min(w - 1), (win_y + y).min(h - 1));
            tensor_data[[0, 0, y as usize, x as usize]] = p[0];
            tensor_data[[0, 1, y as usize, x as usize]] = p[1];
            tensor_data[[0, 2, y as usize, x as usize]] = p[2];
        }
    }
    tensor_data
}

fn run_window(
    session: &Mutex<Session>,
    tensor_data: Array<f32, ndarray::Ix4>,
    scale: u32,
) -> Result<Array<f32, IxDyn>> {
    let (win_h, win_w) = (tensor_data.shape()[2], tensor_data.shape()[3]);
    let tensor = Tensor::from_array(tensor_data.into_dyn().as_standard_layout().into_owned())?;
    let out_tensor = {
        let mut session = session.lock().unwrap();
        let outputs = session.run(ort::inputs![tensor])?;
        outputs[0].try_extract_array::<f32>()?.to_owned()
    };
    let shape = out_tensor.shape().to_vec();
    let expected = [win_h * scale as usize, win_w * scale as usize];
    if shape.len() != 4 || shape[2] != expected[0] || shape[3] != expected[1] {
        return Err(anyhow!(
            "Model output shape {:?} does not match a {}x scale of the {}x{} input. \
             Check the manifest's scale_factor.",
            shape,
            scale,
            win_w,
            win_h
        ));
    }
    Ok(out_tensor)
}

/// Runs an image-to-image model (upscaler, deblurrer, restorer) over the
/// input in overlapping tiles and reassembles the result with a linear
/// cross-fade in the overlap zones, which hides both seam lines and
/// per-tile disagreement (e.g. slightly different color corrections).
///
/// The model must take one NCHW f32 RGB tensor in [0, 1] and return the
/// same layout scaled by `scale`.
///
/// `fixed_size` (height, width) is for exports that only accept certain
/// input dimensions (e.g. NAFNet's channel attention bakes in a minimum):
/// every tile is fed at exactly that size, with edge-replication padding
/// where the window extends past the image.
pub fn run_tiled_enhancement(
    input: &Rgb32FImage,
    session: &Mutex<Session>,
    scale: u32,
    tile_size: u32,
    tile_overlap: u32,
    fixed_size: Option<(u32, u32)>,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<Rgb32FImage> {
    if scale == 0 {
        return Err(anyhow!("scale_factor must be at least 1"));
    }
    let (w, h) = input.dimensions();
    if w == 0 || h == 0 {
        return Err(anyhow!("Cannot enhance an empty image"));
    }

    let (tile_w, tile_h) = match fixed_size {
        Some((fh, fw)) => (fw.max(64), fh.max(64)),
        None => (tile_size.max(64), tile_size.max(64)),
    };
    let overlap_x = tile_overlap.min(tile_w / 4);
    let overlap_y = tile_overlap.min(tile_h / 4);
    let core_w_step = tile_w - 2 * overlap_x;
    let core_h_step = tile_h - 2 * overlap_y;

    let tiles_x = w.div_ceil(core_w_step);
    let tiles_y = h.div_ceil(core_h_step);
    let total = (tiles_x * tiles_y) as usize;
    let mut done = 0usize;

    let (out_w, out_h) = (w * scale, h * scale);
    let mut num = vec![0f32; out_w as usize * out_h as usize * 3];
    let mut den = vec![0f32; out_w as usize * out_h as usize];

    // Adjacent windows overlap by 2*overlap, so the cross-fade ramp spans
    // that full width for weights that sum to ~1 across the seam.
    let ramp_x = (2 * overlap_x * scale).max(1) as f32;
    let ramp_y = (2 * overlap_y * scale).max(1) as f32;

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let core_x = tx * core_w_step;
            let core_y = ty * core_h_step;
            let core_w = core_w_step.min(w - core_x);
            let core_h = core_h_step.min(h - core_y);

            let win_x = core_x.saturating_sub(overlap_x);
            let win_y = core_y.saturating_sub(overlap_y);
            let (win_w, win_h) = if fixed_size.is_some() {
                (tile_w, tile_h)
            } else {
                (
                    (core_x + core_w + overlap_x).min(w) - win_x,
                    (core_y + core_h + overlap_y).min(h) - win_y,
                )
            };

            let out_tensor = run_window(session, window_tensor(input, win_x, win_y, win_w, win_h), scale)?;

            // Only the part of the window inside the image contributes
            // (fixed-size windows may extend past it via padding).
            let valid_w = (win_w.min(w - win_x) * scale) as usize;
            let valid_h = (win_h.min(h - win_y) * scale) as usize;
            let has_left = win_x > 0;
            let has_top = win_y > 0;
            let has_right = win_x + (valid_w as u32 / scale) < w;
            let has_bottom = win_y + (valid_h as u32 / scale) < h;

            let weight_1d = |i: usize, len: usize, ramp: f32, lo: bool, hi: bool| -> f32 {
                let f = i as f32 + 0.5;
                let mut wgt = 1.0f32;
                if lo {
                    wgt = wgt.min(f / ramp);
                }
                if hi {
                    wgt = wgt.min((len as f32 - f) / ramp);
                }
                wgt.max(1e-4)
            };

            for y in 0..valid_h {
                let wy = weight_1d(y, valid_h, ramp_y, has_top, has_bottom);
                let gy = win_y as usize * scale as usize + y;
                for x in 0..valid_w {
                    let wx = weight_1d(x, valid_w, ramp_x, has_left, has_right);
                    let gx = win_x as usize * scale as usize + x;
                    let weight = wx * wy;
                    let px = gy * out_w as usize + gx;
                    num[px * 3] += out_tensor[[0, 0, y, x]].clamp(0.0, 1.0) * weight;
                    num[px * 3 + 1] += out_tensor[[0, 1, y, x]].clamp(0.0, 1.0) * weight;
                    num[px * 3 + 2] += out_tensor[[0, 2, y, x]].clamp(0.0, 1.0) * weight;
                    den[px] += weight;
                }
            }

            done += 1;
            on_progress(done, total);
        }
    }

    let mut output = Rgb32FImage::new(out_w, out_h);
    for (i, p) in output.pixels_mut().enumerate() {
        let d = den[i].max(1e-8);
        *p = Rgb([num[i * 3] / d, num[i * 3 + 1] / d, num[i * 3 + 2] / d]);
    }
    Ok(output)
}

/// Runs the whole image through the model in one pass, padding dimensions
/// up to a multiple of `pad_multiple` with border replication and cropping
/// afterwards. Global corrections (e.g. demoiréing) need this — per-tile
/// runs disagree with each other and show as visible boxes.
pub fn run_single_pass_enhancement(
    input: &Rgb32FImage,
    session: &Mutex<Session>,
    scale: u32,
    pad_multiple: u32,
) -> Result<Rgb32FImage> {
    if scale == 0 {
        return Err(anyhow!("scale_factor must be at least 1"));
    }
    let (w, h) = input.dimensions();
    if w == 0 || h == 0 {
        return Err(anyhow!("Cannot enhance an empty image"));
    }
    let m = pad_multiple.max(1);
    let pad_w = w.div_ceil(m) * m;
    let pad_h = h.div_ceil(m) * m;

    let out_tensor = run_window(session, window_tensor(input, 0, 0, pad_w, pad_h), scale)?;

    let mut output = Rgb32FImage::new(w * scale, h * scale);
    for (x, y, p) in output.enumerate_pixels_mut() {
        *p = Rgb([
            out_tensor[[0, 0, y as usize, x as usize]].clamp(0.0, 1.0),
            out_tensor[[0, 1, y as usize, x as usize]].clamp(0.0, 1.0),
            out_tensor[[0, 2, y as usize, x as usize]].clamp(0.0, 1.0),
        ]);
    }
    Ok(output)
}

#[derive(Clone, Copy)]
pub(crate) struct EnhanceModelParams {
    pub(crate) scale: u32,
    pub(crate) tile_size: u32,
    pub(crate) tile_overlap: u32,
    pub(crate) fixed_size: Option<(u32, u32)>,
    pub(crate) single_pass: bool,
    pub(crate) pad_multiple: u32,
    pub(crate) single_pass_max_pixels: u64,
}

pub(crate) fn model_params(model: &RegisteredModel) -> EnhanceModelParams {
    let get = |key: &str| {
        model
            .manifest
            .params
            .get(key)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
    };
    let fixed_size = match (get("input_height"), get("input_width")) {
        (Some(h), Some(w)) => Some((h, w)),
        _ => None,
    };
    EnhanceModelParams {
        scale: get("scale_factor").unwrap_or(1),
        tile_size: get("tile_size").unwrap_or(DEFAULT_TILE_SIZE),
        tile_overlap: get("tile_overlap").unwrap_or(DEFAULT_TILE_OVERLAP),
        fixed_size,
        single_pass: model
            .manifest
            .params
            .get("single_pass")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        pad_multiple: get("pad_multiple").unwrap_or(32),
        single_pass_max_pixels: model
            .manifest
            .params
            .get("single_pass_max_pixels")
            .and_then(|v| v.as_u64())
            .unwrap_or(33_000_000),
    }
}

fn encode_preview(image: &DynamicImage) -> Result<String, String> {
    const MAX_PREVIEW_DIM: u32 = 4000;
    let (w, h) = image.dimensions();
    let preview = if w.max(h) > MAX_PREVIEW_DIM {
        image.resize(
            MAX_PREVIEW_DIM,
            MAX_PREVIEW_DIM,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image.clone()
    };
    let mut buf = Cursor::new(Vec::new());
    preview
        .to_rgb8()
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode preview: {}", e))?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(buf.get_ref())
    ))
}

pub(crate) fn load_image_for_enhancement(
    path_str: &str,
    app_handle: &AppHandle,
) -> Result<Rgb32FImage, String> {
    let path = Path::new(path_str);
    if !path.exists() {
        return Err("File not found".to_string());
    }

    let is_raw = is_raw_file(path_str);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    let _ = app_handle.emit("enhance-progress", "Loading image...");

    let file_bytes = fs::read(path).map_err(|e| e.to_string())?;
    let mut dynamic_img = load_base_image_from_bytes(&file_bytes, path_str, false, &settings, None)
        .map_err(|e| e.to_string())?;

    if is_raw {
        let _ = app_handle.emit("enhance-progress", "Preparing RAW data...");
        apply_cpu_default_raw_processing(&mut dynamic_img);
    }

    Ok(dynamic_img.to_rgb32f())
}

/// The image every enhancement feature works on. With adjustments present
/// this is the photo exactly as the user sees it — rendered through the
/// same pipeline as exports (edits, masks, crop, rotation all applied).
/// Without adjustments it falls back to the neutrally processed original.
pub(crate) fn enhancement_input(
    path_str: &str,
    adjustments: Option<&serde_json::Value>,
    state: &tauri::State<'_, AppState>,
    app_handle: &tauri::AppHandle,
) -> Result<Rgb32FImage, String> {
    let Some(adj) = adjustments else {
        return load_image_for_enhancement(path_str, app_handle);
    };
    let _ = app_handle.emit("enhance-progress", "Rendering edited photo...");
    let context = crate::gpu_processing::get_or_init_gpu_context(state, app_handle)?;
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let is_raw = is_raw_file(path_str);
    let bytes = fs::read(Path::new(path_str)).map_err(|e| e.to_string())?;
    let base = crate::image_loader::load_and_composite(&bytes, path_str, adj, false, &settings, None)
        .map_err(|e| format!("Failed to load image: {}", e))?;
    let rendered = crate::export_processing::process_image_for_export_pipeline(
        path_str,
        &base,
        adj,
        &context,
        state,
        is_raw,
        "enhancement_render",
        app_handle,
    )?;
    Ok(rendered.to_rgb32f())
}

/// Upper bound for holding a raw model output in memory for instant
/// retries (500MP of f32 RGB ~= 6GB — fine transiently on this machine's
/// 48GB, and single-slot so it never accumulates).
const MAX_CACHED_RAW_PIXELS: u64 = 500_000_000;

fn enhancement_cache_key(
    path: &str,
    task: &str,
    model_id: &str,
    js_adjustments: Option<&serde_json::Value>,
) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(v) = js_adjustments {
        v.to_string().hash(&mut h);
    }
    format!("{}|{}|{}|{:x}", path, task, model_id, h.finish())
}

/// Robust estimate of an image's fine-grained noise level: the median
/// absolute 4-neighbor Laplacian of luma over a sample grid. The median
/// ignores edges (sparse outliers), so this tracks noise/grain amplitude
/// rather than image content.
fn estimate_fine_noise(img: &Rgb32FImage) -> f32 {
    let (w, h) = img.dimensions();
    if w < 8 || h < 8 {
        return 0.0;
    }
    let luma = |x: u32, y: u32| {
        let p = img.get_pixel(x, y);
        0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
    };
    // Cap the sample count so huge upscales don't pay a full-image pass.
    let step = (((w as u64 * h as u64) as f32 / 250_000.0).sqrt().ceil() as u32).max(1);
    let mut vals: Vec<f32> = Vec::with_capacity(260_000);
    let mut y = 1;
    while y < h - 1 {
        let mut x = 1;
        while x < w - 1 {
            let lap = luma(x, y) - (luma(x - 1, y) + luma(x + 1, y) + luma(x, y - 1) + luma(x, y + 1)) * 0.25;
            vals.push(lap.abs());
            x += step;
        }
        y += step;
    }
    if vals.is_empty() {
        return 0.0;
    }
    let mid = vals.len() / 2;
    vals.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    // MAD → σ (×1.4826), then unwind the Laplacian kernel's gain on white
    // noise (√1.25 ≈ 1.118) so both measurements are in pixel-value units.
    vals[mid] * 1.4826 / 1.118
}

/// Deterministic per-pixel noise in (-1, 1) with ~0.408 std (triangular):
/// hash-based so retries are reproducible (no RNG state).
fn grain_noise(i: u32) -> f32 {
    let hash = |mut x: u32| -> u32 {
        x = (x ^ 61) ^ (x >> 16);
        x = x.wrapping_mul(9);
        x ^= x >> 4;
        x = x.wrapping_mul(0x27d4_eb2d);
        x ^ (x >> 15)
    };
    let u1 = hash(i) as f32 / u32::MAX as f32;
    let u2 = hash(i ^ 0x9E37_79B9) as f32 / u32::MAX as f32;
    u1 + u2 - 1.0
}

/// Produces the delivered image from a raw model output: resize to the
/// target dims, swap the original's fine-detail layer back in at
/// `texture`, blend with the original at `strength`, close any remaining
/// grain deficit at `grain`, emit the result payload. Shared by fresh
/// runs and instant retries.
#[allow(clippy::too_many_arguments)]
fn finish_enhancement(
    raw: &Rgb32FImage,
    original: &Rgb32FImage,
    target_w: u32,
    target_h: u32,
    strength: f32,
    texture: f32,
    grain: f32,
    app_handle: &AppHandle,
) -> Result<DynamicImage, String> {
    let _ = app_handle.emit("enhance-progress", "Applying settings...");
    let enhanced = blend_result(raw, original, target_w, target_h, strength, texture, grain);

    let _ = app_handle.emit("enhance-progress", "Generating previews...");
    let out_dynamic = DynamicImage::ImageRgb32F(enhanced);
    let payload = serde_json::json!({
        "enhanced": encode_preview(&out_dynamic)?,
        "original": encode_preview(&DynamicImage::ImageRgb32F(original.clone()))?,
        "width": target_w,
        "height": target_h,
    });
    let _ = app_handle.emit("enhance-complete", &payload);
    Ok(out_dynamic)
}

/// The settings core shared by full runs and crop previews: resize the raw
/// model output to the target dims, swap in the original's fine-detail
/// layer at `texture`, crossfade with the original at `strength`, close
/// any remaining grain deficit at `grain`. Pure — no events.
fn blend_result(
    raw: &Rgb32FImage,
    original: &Rgb32FImage,
    target_w: u32,
    target_h: u32,
    strength: f32,
    texture: f32,
    grain: f32,
) -> Rgb32FImage {
    let mut enhanced = if raw.dimensions() != (target_w, target_h) {
        image::imageops::resize(raw, target_w, target_h, image::imageops::FilterType::Lanczos3)
    } else {
        raw.clone()
    };

    let strength = strength.clamp(0.0, 1.0);
    let texture = texture.clamp(0.0, 1.0);
    let grain = grain.clamp(0.0, 1.0);

    let reference = if strength < 1.0 || texture > 0.0 || grain > 0.0 {
        Some(if original.dimensions() == (target_w, target_h) {
            original.clone()
        } else {
            image::imageops::resize(
                original,
                target_w,
                target_h,
                image::imageops::FilterType::Lanczos3,
            )
        })
    } else {
        None
    };

    // Authentic texture: keep the model output's structure (low/mid
    // frequencies — the actual repair) but blend the ORIGINAL's
    // fine-detail layer back on top. Restoration models smooth away real
    // micro-texture (pores, hair, grain) along with the noise; this puts
    // the real texture back without undoing the repair.
    if texture > 0.0 {
        let reference = reference.as_ref().unwrap();
        let sigma = (target_w.min(target_h) as f32 / 1200.0).clamp(1.2, 3.5);
        let enhanced_low = image::imageops::fast_blur(&enhanced, sigma);
        let reference_low = image::imageops::fast_blur(reference, sigma);
        let row = (target_w * 3) as usize;
        use rayon::prelude::*;
        enhanced
            .par_chunks_mut(row)
            .zip(enhanced_low.par_chunks(row))
            .zip(reference.par_chunks(row).zip(reference_low.par_chunks(row)))
            .for_each(|((e_row, el_row), (r_row, rl_row))| {
                for i in 0..e_row.len() {
                    let own_high = e_row[i] - el_row[i];
                    let ref_high = r_row[i] - rl_row[i];
                    e_row[i] = el_row[i] + own_high * (1.0 - texture) + ref_high * texture;
                }
            });
    }

    if strength < 1.0 {
        let reference = reference.as_ref().unwrap();
        for (out_p, ref_p) in enhanced.pixels_mut().zip(reference.pixels()) {
            for c in 0..3 {
                out_p[c] = out_p[c] * strength + ref_p[c] * (1.0 - strength);
            }
        }
    }

    // Grain match: if the result is smoother than the original, add
    // neutral luma grain to close the measured gap (variances add, so the
    // needed σ is the quadrature difference). Never adds grain when the
    // result is already at least as grainy — this only restores the
    // photo's noise signature, it doesn't stylize.
    if grain > 0.0 {
        let reference = reference.as_ref().unwrap();
        let sigma_ref = estimate_fine_noise(reference);
        let sigma_out = estimate_fine_noise(&enhanced);
        let deficit = (sigma_ref * sigma_ref - sigma_out * sigma_out).max(0.0).sqrt();
        let sigma_add = (deficit * grain).min(0.06);
        if sigma_add > 1e-4 {
            let amplitude = sigma_add / 0.408;
            let row = (target_w * 3) as usize;
            use rayon::prelude::*;
            enhanced
                .par_chunks_mut(row)
                .enumerate()
                .for_each(|(y, e_row)| {
                    for px in 0..(e_row.len() / 3) {
                        let l = 0.2126 * e_row[px * 3]
                            + 0.7152 * e_row[px * 3 + 1]
                            + 0.0722 * e_row[px * 3 + 2];
                        // Film-like: strongest in midtones, present but
                        // subdued in deep shadows and near white.
                        let weight = 0.35 + 0.65 * (4.0 * l * (1.0 - l)).clamp(0.0, 1.0);
                        let n = grain_noise((y as u32).wrapping_mul(target_w).wrapping_add(px as u32))
                            * amplitude
                            * weight;
                        for c in 0..3 {
                            e_row[px * 3 + c] = (e_row[px * 3 + c] + n).max(0.0);
                        }
                    }
                });
        }
    }

    enhanced
}

#[allow(clippy::too_many_arguments)]
fn enhance_image(
    rgb_input: Rgb32FImage,
    session: std::sync::Arc<Mutex<Session>>,
    params: EnhanceModelParams,
    strength: f32,
    texture: f32,
    grain: f32,
    output_scale: u32,
    app_handle: AppHandle,
) -> Result<(DynamicImage, Rgb32FImage, Rgb32FImage), String> {
    let (in_w, in_h) = rgb_input.dimensions();

    let use_single_pass =
        params.single_pass && (in_w as u64 * in_h as u64) <= params.single_pass_max_pixels;

    let enhanced = if use_single_pass {
        let _ = app_handle.emit("enhance-progress", "Processing full image...");
        run_single_pass_enhancement(&rgb_input, &session, params.scale, params.pad_multiple)
            .map_err(|e| e.to_string())?
    } else {
        run_tiled_enhancement(
            &rgb_input,
            &session,
            params.scale,
            params.tile_size,
            params.tile_overlap,
            params.fixed_size,
            |done, total| {
                let pct = (done as f32 / total as f32) * 100.0;
                let _ = app_handle.emit(
                    "enhance-progress",
                    format!("Processing tile {}/{} ({:.0}%)", done, total, pct),
                );
            },
        )
        .map_err(|e| e.to_string())?
    };

    // The model's native-scale output ("raw") is what retries re-blend
    // from; the delivered image is derived from it here.
    let target_scale = output_scale.clamp(1, params.scale);
    let out_dynamic = finish_enhancement(
        &enhanced,
        &rgb_input,
        in_w * target_scale,
        in_h * target_scale,
        strength,
        texture,
        grain,
        &app_handle,
    )?;
    Ok((out_dynamic, enhanced, rgb_input))
}

/// Runs the preferred (or default) model for an enhancement task
/// ("upscale" or "deblur") on one image and holds the result in memory
/// until `save_enhanced_image` is called.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn apply_enhancement(
    path: String,
    task: String,
    strength: Option<f32>,
    texture: Option<f32>,
    grain: Option<f32>,
    output_scale: Option<u32>,
    chain_step: Option<u32>,
    js_adjustments: Option<serde_json::Value>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let task_type = match TaskType::parse(&task) {
        Some(t @ (TaskType::Upscale | TaskType::Deblur | TaskType::Restore)) => t,
        _ => return Err(format!("'{}' is not an enhancement task", task)),
    };
    let chain_step = chain_step.unwrap_or(0);

    let (registry, model) =
        resolve_and_prepare(&app_handle, &state.model_registry, task_type, &task, |_| true)
            .await
            .map_err(|e| e.to_string())?;

    // Instant retry: if the last run was this exact photo/model/edits, the
    // raw model output is still in memory — a new strength or output size
    // is just a re-blend, not a multi-minute re-run.
    let cache_key = format!(
        "{}|c{}",
        enhancement_cache_key(&path, &task, &model.manifest.id, js_adjustments.as_ref()),
        chain_step
    );
    {
        let raw_handle = state.enhancement_raw.clone();
        let result_handle = state.enhancement_result.clone();
        let key = cache_key.clone();
        let strength_v = strength.unwrap_or(1.0);
        let texture_v = texture.unwrap_or(0.0);
        let grain_v = grain.unwrap_or(0.0);
        let output_scale_v = output_scale;
        let handle = app_handle.clone();
        let hit = tokio::task::spawn_blocking(move || -> Result<bool, String> {
            let guard = raw_handle.lock().unwrap();
            // Diagnostics on stderr: retries MUST hit this cache — a miss
            // here means a multi-minute re-run, so log exactly why.
            let Some(cached) = guard.as_ref() else {
                eprintln!("[enhance] retry cache MISS: no cached raw output yet");
                return Ok(false);
            };
            if cached.key != key {
                eprintln!(
                    "[enhance] retry cache MISS: inputs changed\n  cached: {}\n  wanted: {}",
                    cached.key, key
                );
                return Ok(false);
            }
            eprintln!(
                "[enhance] retry cache HIT — re-blending (strength {strength_v}, texture {texture_v}, grain {grain_v})"
            );
            let _ = handle.emit("enhance-progress", "Applying new settings...");
            let (w, h) = cached.original.dimensions();
            let target_scale = output_scale_v
                .unwrap_or(cached.native_scale)
                .clamp(1, cached.native_scale.max(1));
            let (tw, th) = if cached.native_scale == 0 {
                // Engine results have no integer scale; deliver raw dims.
                cached.raw.dimensions()
            } else {
                (w * target_scale, h * target_scale)
            };
            let out = finish_enhancement(
                &cached.raw,
                &cached.original,
                tw,
                th,
                strength_v,
                texture_v,
                grain_v,
                &handle,
            )?;
            *result_handle.lock().unwrap() = Some(out);
            Ok(true)
        })
        .await
        .map_err(|e| e.to_string())??;
        if hit {
            return Ok(());
        }
    }

    // Chained passes feed on the previous step's result (so restore →
    // upscale composes without saving intermediates); fresh runs render
    // from the source file. The input is pinned per step so retries
    // within a step don't feed on the step's own output.
    let rgb_input = if chain_step > 0 {
        let mut pinned = state.enhancement_chain_input.lock().unwrap();
        match pinned.as_ref() {
            Some((step, img)) if *step == chain_step => img.clone(),
            _ => {
                let img = state
                    .enhancement_result
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|img| img.to_rgb32f())
                    .ok_or_else(|| {
                        "No previous result to continue from — run an enhancement first."
                            .to_string()
                    })?;
                *pinned = Some((chain_step, img.clone()));
                img
            }
        }
    } else {
        *state.enhancement_chain_input.lock().unwrap() = None;
        let (source_path, _) = parse_virtual_path(&path);
        let path_str = source_path.to_string_lossy().to_string();
        enhancement_input(&path_str, js_adjustments.as_ref(), &state, &app_handle)?
    };

    // Generative-engine models run through the managed ComfyUI process
    // instead of the in-process ONNX pipeline.
    if model.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy") {
        return run_comfy_enhancement(
            model,
            strength.unwrap_or(1.0),
            texture.unwrap_or(0.0),
            grain.unwrap_or(0.0),
            rgb_input,
            cache_key,
            app_handle,
            state,
        )
        .await;
    }

    let session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;
    let params = model_params(&model);
    let strength = strength.unwrap_or(1.0);
    let output_scale = output_scale.unwrap_or(params.scale);
    let result_handle = state.enhancement_result.clone();
    let raw_handle = state.enhancement_raw.clone();

    let model_id = model.manifest.id.clone();
    let native_scale = params.scale;
    tokio::task::spawn_blocking(move || {
        match enhance_image(
            rgb_input,
            session,
            params,
            strength,
            texture.unwrap_or(0.0),
            grain.unwrap_or(0.0),
            output_scale,
            app_handle.clone(),
        ) {
            Ok((image, raw, original)) => {
                *result_handle.lock().unwrap() = Some(image);
                let px = raw.width() as u64 * raw.height() as u64;
                *raw_handle.lock().unwrap() = if px <= MAX_CACHED_RAW_PIXELS {
                    Some(crate::app_state::EnhancementRaw {
                        key: cache_key,
                        raw,
                        original,
                        native_scale,
                    })
                } else {
                    None
                };
            }
            Err(e) => {
                let _ = app_handle.emit("enhance-error", e);
            }
        }
        // Enhancement models are the heaviest and used sporadically; free
        // their session right away instead of keeping it resident like the
        // frequently-used mask/inpaint models.
        registry.unload(&model_id);
    })
    .await
    .map_err(|e| format!("Enhancement task failed: {}", e))
}

fn encode_crop_png(image: &Rgb32FImage) -> Result<String, String> {
    let mut buf = Cursor::new(Vec::new());
    DynamicImage::ImageRgb32F(image.clone())
        .to_rgb8()
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode preview crop: {}", e))?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(buf.get_ref())
    ))
}

/// Returns a small overview of the exact image the enhancement engine will
/// process. The preview dialog uses this — not the app's edited thumbnail —
/// as its click/drag map, so region coordinates always line up even when
/// the photo has crops or other edits applied.
#[tauri::command]
pub async fn get_enhancement_overview(
    path: String,
    js_adjustments: Option<serde_json::Value>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    const OVERVIEW_MAX: u32 = 720;
    let (source_path, _) = parse_virtual_path(&path);
    let path_str = source_path.to_string_lossy().to_string();
    let rgb_input = enhancement_input(&path_str, js_adjustments.as_ref(), &state, &app_handle)?;
    tokio::task::spawn_blocking(move || {
        let (w, h) = rgb_input.dimensions();
        let overview = DynamicImage::ImageRgb32F(rgb_input).resize(
            OVERVIEW_MAX,
            OVERVIEW_MAX,
            image::imageops::FilterType::Triangle,
        );
        let mut buf = Cursor::new(Vec::new());
        overview
            .to_rgb8()
            .write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| format!("Failed to encode overview: {}", e))?;
        Ok(serde_json::json!({
            "overview": format!(
                "data:image/png;base64,{}",
                general_purpose::STANDARD.encode(buf.get_ref())
            ),
            "width": w,
            "height": h,
        }))
    })
    .await
    .map_err(|e| format!("Overview task failed: {}", e))?
}

/// Blends a preview crop at the requested settings and packages the reply.
fn preview_payload(
    raw: &Rgb32FImage,
    original: &Rgb32FImage,
    scale: u32,
    strength: f32,
    texture: f32,
    grain: f32,
) -> Result<serde_json::Value, String> {
    let (rw, rh) = raw.dimensions();
    let blended = blend_result(raw, original, rw, rh, strength, texture, grain);
    Ok(serde_json::json!({
        "original": encode_crop_png(original)?,
        "enhanced": encode_crop_png(&blended)?,
        "scale": scale,
    }))
}

/// Runs the selected model on a crop around (`center_x`, `center_y`) with a
/// selectable size (`region_size`, normalized to the image's short side) so
/// the user can judge a model before committing to a full run. The crop's
/// raw model output is cached, so re-calling with only different
/// strength/texture/grain re-blends in milliseconds — this is what lets
/// the preview box track the sliders live.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn preview_enhancement(
    path: String,
    task: String,
    center_x: f32,
    center_y: f32,
    region_size: Option<f32>,
    strength: Option<f32>,
    texture: Option<f32>,
    grain: Option<f32>,
    js_adjustments: Option<serde_json::Value>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    const MIN_CROP: u32 = 64;
    const MAX_CROP: u32 = 512;
    const DEFAULT_CROP: u32 = 256;

    let task_type = match TaskType::parse(&task) {
        Some(t @ (TaskType::Upscale | TaskType::Deblur | TaskType::Restore)) => t,
        _ => return Err(format!("'{}' is not an enhancement task", task)),
    };
    let (registry, model) =
        resolve_and_prepare(&app_handle, &state.model_registry, task_type, &task, |_| true)
            .await
            .map_err(|e| e.to_string())?;

    let strength_v = strength.unwrap_or(1.0);
    let texture_v = texture.unwrap_or(0.0);
    let grain_v = grain.unwrap_or(0.0);

    // Region-specific cache key: same photo/model/edits/region → the raw
    // crop output is still valid, only the blend settings changed.
    let preview_key = format!(
        "{}|{:x}|{:x}|{:x}",
        enhancement_cache_key(&path, &task, &model.manifest.id, js_adjustments.as_ref()),
        center_x.to_bits(),
        center_y.to_bits(),
        region_size.map(|v| v.to_bits()).unwrap_or(0)
    );
    {
        let cache = state.enhancement_preview_raw.clone();
        let key = preview_key.clone();
        let cached_reply = tokio::task::spawn_blocking(move || {
            let guard = cache.lock().unwrap();
            guard.as_ref().filter(|c| c.key == key).map(|c| {
                eprintln!("[enhance] preview cache HIT — re-blending crop");
                preview_payload(&c.raw, &c.original, 0, strength_v, texture_v, grain_v)
            })
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Some(reply) = cached_reply {
            return reply;
        }
    }
    eprintln!("[enhance] preview cache MISS — running model on crop");

    // Generative-engine models: preview the crop through the engine.
    if model.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy") {
        return preview_comfy_enhancement(
            model,
            path,
            center_x,
            center_y,
            region_size,
            strength_v,
            texture_v,
            grain_v,
            preview_key,
            js_adjustments,
            app_handle,
            state,
        )
        .await;
    }

    let session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;
    let params = model_params(&model);

    let (source_path, _) = parse_virtual_path(&path);
    let path_str = source_path.to_string_lossy().to_string();
    let rgb_input = enhancement_input(&path_str, js_adjustments.as_ref(), &state, &app_handle)?;

    let preview_cache = state.enhancement_preview_raw.clone();
    tokio::task::spawn_blocking(move || {
        let (w, h) = rgb_input.dimensions();

        let crop_size = match region_size {
            Some(s) => ((s.clamp(0.01, 1.0) * w.min(h) as f32) as u32).clamp(MIN_CROP, MAX_CROP),
            None => DEFAULT_CROP,
        };
        let crop_w = crop_size.min(w);
        let crop_h = crop_size.min(h);
        let cx = (center_x.clamp(0.0, 1.0) * w as f32) as u32;
        let cy = (center_y.clamp(0.0, 1.0) * h as f32) as u32;
        let x0 = cx.saturating_sub(crop_w / 2).min(w - crop_w);
        let y0 = cy.saturating_sub(crop_h / 2).min(h - crop_h);

        let crop = image::imageops::crop_imm(&rgb_input, x0, y0, crop_w, crop_h).to_image();

        let enhanced = run_tiled_enhancement(
            &crop,
            &session,
            params.scale,
            params.tile_size,
            params.tile_overlap,
            params.fixed_size,
            |_, _| {},
        )
        .map_err(|e| e.to_string())?;

        let reply = preview_payload(&enhanced, &crop, params.scale, strength_v, texture_v, grain_v);
        *preview_cache.lock().unwrap() = Some(crate::app_state::PreviewRaw {
            key: preview_key,
            raw: enhanced,
            original: crop,
        });
        reply
    })
    .await
    .map_err(|e| format!("Preview task failed: {}", e))?
}

/// Crop preview for engine models: run the small crop through SeedVR2 at a
/// modest target so it returns in seconds.
#[allow(clippy::too_many_arguments)]
async fn preview_comfy_enhancement(
    model: crate::model_registry::RegisteredModel,
    path: String,
    center_x: f32,
    center_y: f32,
    region_size: Option<f32>,
    strength: f32,
    texture: f32,
    grain: f32,
    preview_key: String,
    js_adjustments: Option<serde_json::Value>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    const MIN_CROP: u32 = 64;
    const MAX_CROP: u32 = 512;
    const DEFAULT_CROP: u32 = 256;

    let (source_path, _) = parse_virtual_path(&path);
    let path_str = source_path.to_string_lossy().to_string();
    let rgb_input = enhancement_input(&path_str, js_adjustments.as_ref(), &state, &app_handle)?;
    let (crop, engine_short_edge, crop_png) = tokio::task::spawn_blocking(move || {
        let (w, h) = rgb_input.dimensions();
        let crop_size = match region_size {
            Some(s) => ((s.clamp(0.01, 1.0) * w.min(h) as f32) as u32).clamp(MIN_CROP, MAX_CROP),
            None => DEFAULT_CROP,
        };
        let crop_w = crop_size.min(w);
        let crop_h = crop_size.min(h);
        let cx = (center_x.clamp(0.0, 1.0) * w as f32) as u32;
        let cy = (center_y.clamp(0.0, 1.0) * h as f32) as u32;
        let x0 = cx.saturating_sub(crop_w / 2).min(w - crop_w);
        let y0 = cy.saturating_sub(crop_h / 2).min(h - crop_h);
        let crop = image::imageops::crop_imm(&rgb_input, x0, y0, crop_w, crop_h).to_image();
        // 2x the crop, on a 16-aligned grid the engine tolerates.
        let target = (crop.width().min(crop.height()) * 2).clamp(128, 1024);
        let aligned = align_for_engine(&crop, target, target);
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb32F(aligned.clone())
            .to_rgb8()
            .write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((crop, aligned.width().min(aligned.height()), buf.into_inner()))
    })
    .await
    .map_err(|e| e.to_string())??;

    let model_file = std::path::Path::new(&model.manifest.file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Invalid engine model path".to_string())?
        .to_string();
    let result_png = crate::comfy_engine::run_seedvr2(
        &app_handle,
        &state,
        &model_file,
        crop_png,
        engine_short_edge,
        42,
        |_| {},
    )
    .await
    .map_err(|e| e.to_string())?;

    let enhanced = image::load_from_memory(&result_png)
        .map_err(|e| e.to_string())?
        .to_rgb32f();
    let reply = preview_payload(&enhanced, &crop, 2, strength, texture, grain);
    *state.enhancement_preview_raw.lock().unwrap() = Some(crate::app_state::PreviewRaw {
        key: preview_key,
        raw: enhanced,
        original: crop,
    });
    reply
}


/// Resizes an image for the generative engine so both dimensions are
/// multiples of 16 — latent-space models shear or corrupt output on
/// unaligned sizes (arbitrary crops produce exactly such sizes). The short
/// edge lands in [min_short, max_short]; doing the resize here (instead of
/// inside the engine) keeps full control of the final grid.
pub(crate) fn align_for_engine(img: &Rgb32FImage, min_short: u32, max_short: u32) -> Rgb32FImage {
    let (w, h) = img.dimensions();
    let short = w.min(h).max(1);
    let target_short = (short.clamp(min_short, max_short) / 16).max(1) * 16;
    let scale = target_short as f32 / short as f32;
    let round16 = |v: f32| (((v / 16.0).round() as u32).max(1)) * 16;
    let (ow, oh) = if w <= h {
        (target_short, round16(h as f32 * scale))
    } else {
        (round16(w as f32 * scale), target_short)
    };
    if (ow, oh) == (w, h) {
        return img.clone();
    }
    image::imageops::resize(img, ow, oh, image::imageops::FilterType::Lanczos3)
}

#[allow(clippy::too_many_arguments)]
async fn run_comfy_enhancement(
    model: crate::model_registry::RegisteredModel,
    strength: f32,
    texture: f32,
    grain: f32,
    rgb: Rgb32FImage,
    cache_key: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let resolution = model
        .manifest
        .params
        .get("resolution")
        .and_then(|v| v.as_u64())
        .unwrap_or(1080) as u32;

    let (rgb_input, input_png, engine_short_edge) = tokio::task::spawn_blocking(move || {
        // The engine's short-edge target is a *minimum*: photos larger than
        // it are processed at (capped) native size rather than downscaled.
        let aligned = align_for_engine(&rgb, resolution, 1536);
        let short_edge = aligned.width().min(aligned.height());
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb32F(aligned)
            .to_rgb8()
            .write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((rgb, buf.into_inner(), short_edge))
    })
    .await
    .map_err(|e| e.to_string())??;

    // The DiT weights filename comes from the manifest so 3B/7B variants
    // share this path.
    let model_file = std::path::Path::new(&model.manifest.file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "Invalid engine model path".to_string())?
        .to_string();
    let progress_handle = app_handle.clone();
    let result_png = crate::comfy_engine::run_seedvr2(
        &app_handle,
        &state,
        &model_file,
        input_png,
        engine_short_edge,
        42,
        move |msg| {
            let _ = progress_handle.emit("enhance-progress", msg);
        },
    )
    .await
    .map_err(|e| e.to_string())?;

    let result_handle = state.enhancement_result.clone();
    let raw_handle = state.enhancement_raw.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let raw = image::load_from_memory(&result_png)
            .map_err(|e| e.to_string())?
            .to_rgb32f();
        let (rw, rh) = raw.dimensions();
        let out_dynamic =
            finish_enhancement(&raw, &rgb_input, rw, rh, strength, texture, grain, &app_handle)?;
        *result_handle.lock().unwrap() = Some(out_dynamic);
        // native_scale 0 = engine result: retries deliver at raw dims.
        *raw_handle.lock().unwrap() = Some(crate::app_state::EnhancementRaw {
            key: cache_key,
            raw,
            original: rgb_input,
            native_scale: 0,
        });
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;

    Ok(())
}

#[tauri::command]
pub async fn save_enhanced_image(
    original_path_str: String,
    task: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    // Borrow rather than take: a failed write (or a second Save click)
    // must not strand the user with an un-saveable result. The entry is
    // replaced by the next run and dropped when the app exits.
    let enhanced_image = {
        let guard = state.enhancement_result.lock().unwrap();
        guard.as_ref().cloned().ok_or_else(|| {
            "No enhanced image is in memory to save — run the enhancement again, then Save."
                .to_string()
        })?
    };

    let suffix = match task.as_str() {
        "deblur" => "Deblurred",
        "restore" => "Restored",
        _ => "Upscaled",
    };
    let is_raw = is_raw_file(&original_path_str);

    let (first_path, source_sidecar_path) = parse_virtual_path(&original_path_str);
    let parent_dir = first_path
        .parent()
        .ok_or_else(|| "Could not determine parent directory.".to_string())?;
    let stem = first_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("enhanced");

    let (output_filename, image_to_save): (String, DynamicImage) = if is_raw {
        (
            format!("{}_{}.tiff", stem, suffix),
            DynamicImage::ImageRgb16(enhanced_image.to_rgb16()),
        )
    } else {
        (
            format!("{}_{}.png", stem, suffix),
            DynamicImage::ImageRgb8(enhanced_image.to_rgb8()),
        )
    };

    let output_path = parent_dir.join(output_filename);

    let (out_w, out_h) = image_to_save.dimensions();
    log::info!(
        "save_enhanced_image: task={} {}x{} -> {:?}",
        task,
        out_w,
        out_h,
        output_path
    );
    image_to_save.save(&output_path).map_err(|e| {
        log::error!("save_enhanced_image failed for {:?}: {}", output_path, e);
        format!("Failed to save image: {}", e)
    })?;

    let (real_path, _) = parse_virtual_path(&original_path_str);
    let _ =
        crate::exif_processing::write_rrexif_sidecar(&real_path.to_string_lossy(), &output_path);

    // Deliberately NOT copying the source sidecar: edits are already baked
    // into this file, so inheriting them would re-apply crop/rotation/AI
    // patches on top of the baked pixels (tilted/warped display).
    let _ = source_sidecar_path;

    Ok(output_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod authentic_texture_tests {
    use super::*;

    /// The grain-match loop relies on the estimator reading the σ it will
    /// later inject: synthesize noise with the same generator and check
    /// the round trip, plus zero on a flat image.
    #[test]
    fn noise_estimator_tracks_known_sigma() {
        let mut img = Rgb32FImage::from_pixel(256, 256, Rgb([0.5f32, 0.5, 0.5]));
        let target = 0.02f32;
        let amplitude = target / 0.408;
        for (i, p) in img.pixels_mut().enumerate() {
            let n = grain_noise(i as u32) * amplitude;
            for c in 0..3 {
                p[c] += n;
            }
        }
        let est = estimate_fine_noise(&img);
        assert!(
            (est - target).abs() < target * 0.35,
            "estimated σ {} too far from injected σ {}",
            est,
            target
        );

        let flat = Rgb32FImage::from_pixel(64, 64, Rgb([0.5f32, 0.5, 0.5]));
        assert!(estimate_fine_noise(&flat) < 1e-4);
    }

    fn test_pair() -> (Rgb32FImage, Rgb32FImage) {
        // "Raw" = smooth gradient (a model's over-clean output); "original"
        // = the same gradient with noise, so texture and grain both engage.
        let mut raw = Rgb32FImage::new(96, 64);
        for (x, y, p) in raw.enumerate_pixels_mut() {
            let v = (x as f32 / 95.0) * 0.8 + (y as f32 / 63.0) * 0.1;
            *p = Rgb([v, v * 0.9, v * 0.8]);
        }
        let mut original = raw.clone();
        for (i, p) in original.pixels_mut().enumerate() {
            let n = grain_noise(i as u32) * 0.05;
            for c in 0..3 {
                p[c] = (p[c] + n).max(0.0);
            }
        }
        (raw, original)
    }

    /// Strength 1 + texture 0 + grain 0 must be byte-identical to the raw
    /// model output — the new sliders can't perturb old behavior at rest.
    #[test]
    fn neutral_settings_are_identity() {
        let (raw, original) = test_pair();
        let out = blend_result(&raw, &original, 96, 64, 1.0, 0.0, 0.0);
        assert_eq!(out.as_raw(), raw.as_raw());
    }

    /// Same inputs + same settings must give the identical result — this is
    /// what makes "preview crop" and "retry" trustworthy: what you saw is
    /// what you get, every time.
    #[test]
    fn blend_is_deterministic_and_settings_change_output() {
        let (raw, original) = test_pair();
        let a = blend_result(&raw, &original, 96, 64, 0.7, 0.5, 1.0);
        let b = blend_result(&raw, &original, 96, 64, 0.7, 0.5, 1.0);
        assert_eq!(a.as_raw(), b.as_raw(), "same settings must reproduce exactly");

        let c = blend_result(&raw, &original, 96, 64, 0.7, 0.9, 1.0);
        assert_ne!(a.as_raw(), c.as_raw(), "changing texture must change the result");
        let d = blend_result(&raw, &original, 96, 64, 0.4, 0.5, 1.0);
        assert_ne!(a.as_raw(), d.as_raw(), "changing strength must change the result");
    }
}
