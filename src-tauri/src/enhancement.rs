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

            let out_tensor = run_window(
                session,
                window_tensor(input, win_x, win_y, win_w, win_h),
                scale,
            )?;

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
    let base =
        crate::image_loader::load_and_composite(&bytes, path_str, adj, false, &settings, None)
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
            let lap = luma(x, y)
                - (luma(x - 1, y) + luma(x + 1, y) + luma(x, y - 1) + luma(x, y + 1)) * 0.25;
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
pub(crate) fn grain_noise(i: u32) -> f32 {
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
    // The compare view overlays both images in one zoom/pan space, so they
    // MUST have identical dimensions: at 2x rebuild the raw original is
    // half the result's size, which skewed and misaligned the wipe overlay
    // (and hid every slider change behind the breakage).
    let display_original = if original.dimensions() == (target_w, target_h) {
        DynamicImage::ImageRgb32F(original.clone())
    } else {
        DynamicImage::ImageRgb32F(image::imageops::resize(
            original,
            target_w,
            target_h,
            image::imageops::FilterType::Lanczos3,
        ))
    };
    let payload = serde_json::json!({
        "enhanced": encode_preview(&out_dynamic)?,
        "original": encode_preview(&display_original)?,
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
pub(crate) fn blend_result(
    raw: &Rgb32FImage,
    original: &Rgb32FImage,
    target_w: u32,
    target_h: u32,
    strength: f32,
    texture: f32,
    grain: f32,
) -> Rgb32FImage {
    let mut enhanced = if raw.dimensions() != (target_w, target_h) {
        image::imageops::resize(
            raw,
            target_w,
            target_h,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        raw.clone()
    };

    let strength = strength.clamp(0.0, 1.0);
    let texture = texture.clamp(0.0, 1.0);
    let grain = grain.clamp(0.0, 1.0);

    // When the result is LARGER than the original (upscales, engine 2x
    // previews), the original's fine texture and noise land at a coarser
    // pixel scale after resizing to target dims. Every fine-detail
    // operation below must widen its band by this ratio, or it looks for
    // texture at a scale where an upscaled image has none — which silently
    // turns the Texture and Match grain sliders into no-ops.
    let (ow, oh) = original.dimensions();
    let scale_ratio = (target_w as f32 / ow.max(1) as f32)
        .max(target_h as f32 / oh.max(1) as f32)
        .max(1.0);

    let reference = if strength < 1.0 || texture > 0.0 {
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
        // Band sized from the ORIGINAL's resolution, widened by the resize
        // ratio so it captures the original's real texture at target scale.
        let sigma = (ow.min(oh) as f32 / 1200.0).clamp(1.2, 3.5) * scale_ratio;
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
        // Measure the original's noise at its NATIVE size — after
        // upscaling, its noise moves to a coarser scale and the fine-noise
        // estimator reads near zero, which zeroed the deficit and made
        // Match grain a silent no-op on upscaled results.
        let sigma_ref = estimate_fine_noise(original);
        let sigma_out = estimate_fine_noise(&enhanced);
        let deficit = (sigma_ref * sigma_ref - sigma_out * sigma_out)
            .max(0.0)
            .sqrt();
        // Matching alone almost never fires in practice: degraded JPEGs
        // carry BLOCK artifacts (8px scale) rather than fine noise, so the
        // measured deficit is ~0 and the slider read as dead. The slider
        // therefore guarantees a floor of film-look grain scaled by its
        // position, and the match only ever raises that.
        const GRAIN_FLOOR: f32 = 0.03;
        let sigma_add = (deficit.max(GRAIN_FLOOR) * grain).min(0.06);
        log::info!(
            "[enhance] grain: sigma_ref={:.5} sigma_out={:.5} adding sigma={:.5} (ratio {:.2})",
            sigma_ref,
            sigma_out,
            sigma_add,
            scale_ratio
        );
        if sigma_add > 1e-4 {
            let amplitude = sigma_add / 0.408;
            let row = (target_w * 3) as usize;
            // Grain cells are at least 2px (and follow the resize ratio
            // beyond that): single-pixel grain averages away the moment
            // the photo is viewed below 100% zoom, which made this slider
            // look dead on full renders. 2px cells read as film grain at
            // 1:1 and stay perceptible at fit-to-screen.
            let inv_ratio = 1.0 / scale_ratio.max(2.0);
            use rayon::prelude::*;
            enhanced
                .par_chunks_mut(row)
                .enumerate()
                .for_each(|(y, e_row)| {
                    let ny = ((y as f32 * inv_ratio) as u32).min(oh.saturating_sub(1));
                    for px in 0..(e_row.len() / 3) {
                        let l = 0.2126 * e_row[px * 3]
                            + 0.7152 * e_row[px * 3 + 1]
                            + 0.0722 * e_row[px * 3 + 2];
                        // Film-like: strongest in midtones, present but
                        // subdued in deep shadows and near white.
                        let weight = 0.35 + 0.65 * (4.0 * l * (1.0 - l)).clamp(0.0, 1.0);
                        let nx = ((px as f32 * inv_ratio) as u32).min(ow.saturating_sub(1));
                        let n =
                            grain_noise(ny.wrapping_mul(ow).wrapping_add(nx)) * amplitude * weight;
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
    reblend_only: Option<bool>,
    depixelate: Option<u32>,
    js_adjustments: Option<serde_json::Value>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let task_type = match TaskType::parse(&task) {
        Some(t @ (TaskType::Upscale | TaskType::Deblur | TaskType::Restore)) => t,
        _ => return Err(format!("'{}' is not an enhancement task", task)),
    };
    let chain_step = chain_step.unwrap_or(0);
    log::info!(
        "[enhance] apply: task={} strength={:?} texture={:?} grain={:?} scale={:?} chain={}",
        task,
        strength,
        texture,
        grain,
        output_scale,
        chain_step
    );

    let (registry, model) =
        resolve_and_prepare(&app_handle, &state.model_registry, task_type, &task, |_| {
            true
        })
        .await
        .map_err(|e| e.to_string())?;

    // Instant retry: if the last run was this exact photo/model/edits, the
    // raw model output is still in memory — a new strength or output size
    // is just a re-blend, not a multi-minute re-run.
    let is_engine_model =
        model.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy");
    // For engine models the output scale changes the RUN itself (the input
    // is pre-upscaled), not just the delivery blend — so it must key the
    // cache, or a 1x→2x retry would silently return the cached 1x result.
    let engine_scale = if is_engine_model {
        output_scale.unwrap_or(1).clamp(1, 2)
    } else {
        1
    };
    let cache_key = format!(
        "{}|c{}|s{}|dp{}",
        enhancement_cache_key(&path, &task, &model.manifest.id, js_adjustments.as_ref()),
        chain_step,
        engine_scale,
        depixelate.map(|c| c.to_string()).unwrap_or_else(|| "off".into())
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
                log::info!("[enhance] retry cache MISS: no cached raw output yet");
                return Ok(false);
            };
            if cached.key != key {
                log::info!(
                    "[enhance] retry cache MISS: inputs changed\n  cached: {}\n  wanted: {}",
                    cached.key, key
                );
                return Ok(false);
            }
            log::info!(
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

    // Live slider updates re-blend ONLY: on a cold cache they must never
    // kick off a multi-minute model run from a slider drag — do nothing
    // and let the user start a real run explicitly.
    if reblend_only.unwrap_or(false) {
        log::info!("[enhance] reblend-only requested but cache is cold — ignoring");
        return Ok(());
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

    // De-pixelate prep: dissolve a hard mosaic into the smooth low-res
    // image it encodes before the model sees it (models sharpen crisp
    // block edges instead of removing them).
    let rgb_input = match depixelate {
        Some(cell) => {
            let _ = app_handle.emit("enhance-progress", "De-pixelating...");
            let (out, used) = apply_depixelate(&rgb_input, cell)?;
            log::info!("[enhance] de-pixelate prep: cell {used}px (requested {cell}, 0=auto)");
            out
        }
        None => rgb_input,
    };

    // Generative-engine models run through the managed ComfyUI process
    // instead of the in-process ONNX pipeline.
    if is_engine_model {
        return run_comfy_enhancement(
            model,
            strength.unwrap_or(1.0),
            texture.unwrap_or(0.0),
            grain.unwrap_or(0.0),
            engine_scale,
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

    // Diagnostic: how much did the model actually change this crop? If
    // this is ~0, every blend setting is mixing two identical images and
    // the sliders CANNOT produce a visible difference — the problem is the
    // model run, not the blending.
    {
        let reference = if original.dimensions() == (rw, rh) {
            original.clone()
        } else {
            image::imageops::resize(original, rw, rh, image::imageops::FilterType::Lanczos3)
        };
        let sum: f64 = raw
            .as_raw()
            .iter()
            .zip(reference.as_raw().iter())
            .map(|(a, b)| (a - b).abs() as f64)
            .sum();
        let mean = sum / raw.as_raw().len().max(1) as f64;
        log::info!(
            "[enhance] crop model delta: mean|raw-original| = {:.5} ({}x{})",
            mean,
            rw,
            rh
        );
    }

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
    output_scale: Option<u32>,
    depixelate: Option<u32>,
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
        resolve_and_prepare(&app_handle, &state.model_registry, task_type, &task, |_| {
            true
        })
        .await
        .map_err(|e| e.to_string())?;

    let strength_v = strength.unwrap_or(1.0);
    let texture_v = texture.unwrap_or(0.0);
    let grain_v = grain.unwrap_or(0.0);
    log::info!(
        "[enhance] preview: task={} strength={:?} texture={:?} grain={:?} center=({:.3},{:.3}) region={:?}",
        task,
        strength,
        texture,
        grain,
        center_x,
        center_y,
        region_size
    );

    // Region-specific cache key: same photo/model/edits/region → the raw
    // crop output is still valid, only the blend settings changed.
    let engine_scale = output_scale.unwrap_or(1).clamp(1, 2);
    let preview_key = format!(
        "{}|{:x}|{:x}|{:x}|s{}|dp{}",
        enhancement_cache_key(&path, &task, &model.manifest.id, js_adjustments.as_ref()),
        center_x.to_bits(),
        center_y.to_bits(),
        region_size.map(|v| v.to_bits()).unwrap_or(0),
        engine_scale,
        depixelate.map(|c| c.to_string()).unwrap_or_else(|| "off".into())
    );
    {
        let cache = state.enhancement_preview_raw.clone();
        let key = preview_key.clone();
        let cached_reply = tokio::task::spawn_blocking(move || {
            let guard = cache.lock().unwrap();
            guard.as_ref().filter(|c| c.key == key).map(|c| {
                log::info!("[enhance] preview cache HIT — re-blending crop");
                preview_payload(&c.raw, &c.original, 0, strength_v, texture_v, grain_v)
            })
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Some(reply) = cached_reply {
            return reply;
        }
    }
    log::info!("[enhance] preview cache MISS — running model on crop");

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
            engine_scale,
            depixelate,
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
        // De-pixelate on the CROP: the user pointed the preview at the
        // pixelated area, which is a far stronger detection target than
        // the whole frame.
        let crop = match depixelate {
            Some(cell) => apply_depixelate(&crop, cell)?.0,
            None => crop,
        };

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

        let reply = preview_payload(
            &enhanced,
            &crop,
            params.scale,
            strength_v,
            texture_v,
            grain_v,
        );
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
    output_scale: u32,
    depixelate: Option<u32>,
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
    let resolution = model
        .manifest
        .params
        .get("resolution")
        .and_then(|v| v.as_u64())
        .unwrap_or(1080) as u32;
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
        // De-pixelate on the CROP: the user pointed the preview at the
        // pixelated area, which is a far stronger detection target than
        // the whole frame.
        let crop = match depixelate {
            Some(cell) => apply_depixelate(&crop, cell)?.0,
            None => crop,
        };
        // Run the crop at the SAME effective scale the full render would
        // use (align_for_engine on the whole photo), so what the preview
        // shows — including how much detail the model invents and how the
        // texture/grain sliders behave on it — matches the full result. A
        // fixed 2x here made previews systematically overpromise.
        let full_short = w.min(h).max(1);
        let full_target =
            (((full_short.clamp(resolution, 1536) * output_scale.clamp(1, 2)).min(2048)) / 16)
                .max(1)
                * 16;
        let full_ratio = full_target as f32 / full_short as f32;
        let target = ((crop.width().min(crop.height()) as f32 * full_ratio) as u32).clamp(64, 1024);
        let aligned = align_for_engine(&crop, target, target);
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb32F(aligned.clone())
            .to_rgb8()
            .write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((
            crop,
            aligned.width().min(aligned.height()),
            buf.into_inner(),
        ))
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
    output_scale: u32,
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
        // At output_scale 2 the input is pre-upscaled so the engine runs
        // in its detail-inventing regime — the reconstruction mode for
        // pixelated/low-res sources (capped for VRAM/runtime sanity).
        let short = rgb.width().min(rgb.height()).max(1);
        let target_short = (short.clamp(resolution, 1536) * output_scale.clamp(1, 2)).min(2048);
        let aligned = align_for_engine(&rgb, target_short, target_short);
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
        let out_dynamic = finish_enhancement(
            &raw,
            &rgb_input,
            rw,
            rh,
            strength,
            texture,
            grain,
            &app_handle,
        )?;
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

// ---------------------------------------------------------------------------
// De-pixelate prep: a hard mosaic is an honest low-res image wearing a crisp
// grid — and restoration models PRESERVE crisp edges, block edges included.
// The prep collapses each grid cell to its true mean and rebuilds the image
// as a smooth interpolation of those means (Catmull-Rom through the cell
// centers, phase-aware), so the model sees a natural soft image it knows how
// to restore instead of a grid it would sharpen.
// ---------------------------------------------------------------------------

pub const DEPIX_MIN_CELL: u32 = 2;
pub const DEPIX_MAX_CELL: u32 = 96;
/// Boundary comb must carry this much more gradient energy than the
/// profile average. Soft (JPEG/resized) mosaics dilute the comb, so this
/// is deliberately gentler than a crisp-grid threshold.
const DEPIX_MIN_COMB_SCORE: f32 = 1.3;
/// Minimum normalized autocorrelation peak to accept as a repeating grid.
const DEPIX_MIN_ACF_PEAK: f32 = 0.18;

/// A detected (or forced) pixelation grid. Pitches are FLOAT: real-world
/// mosaics were usually resized after creation, so the block period is
/// rarely a whole number of pixels. `bound_x`/`bound_y` are the positions
/// of the first cell boundary (the first cell may be partial).
#[derive(Clone, Copy, Debug)]
pub struct MosaicGrid {
    pub pitch_x: f32,
    pub pitch_y: f32,
    pub bound_x: f32,
    pub bound_y: f32,
}

/// Column/row gradient-energy profiles of the luma image. A mosaic
/// concentrates its gradient energy on the grid boundaries, so these
/// profiles spike periodically.
fn mosaic_gradient_profiles(img: &Rgb32FImage) -> (Vec<f32>, Vec<f32>) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut luma = vec![0.0f32; w * h];
    for (i, p) in img.pixels().enumerate() {
        luma[i] = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    }
    let mut gx = vec![0.0f32; w.saturating_sub(1)];
    let mut gy = vec![0.0f32; h.saturating_sub(1)];
    for y in 0..h {
        let row = &luma[y * w..(y + 1) * w];
        for x in 0..w - 1 {
            gx[x] += (row[x + 1] - row[x]).abs();
        }
    }
    for y in 0..h - 1 {
        for x in 0..w {
            gy[y] += (luma[(y + 1) * w + x] - luma[y * w + x]).abs();
        }
    }
    (gx, gy)
}

/// Dominant repetition period of a profile via normalized autocorrelation
/// with parabolic sub-pixel refinement. Returns (pitch, peak strength).
/// A mosaic's boundary comb autocorrelates periodically; natural content
/// decays smoothly and produces no isolated peak.
fn normalized_autocorr(profile: &[f32]) -> Option<Vec<f32>> {
    let n = profile.len();
    if n < 16 {
        return None;
    }
    let mean = profile.iter().sum::<f32>() / n as f32;
    let x: Vec<f32> = profile.iter().map(|v| v - mean).collect();
    let r0: f32 = x.iter().map(|v| v * v).sum();
    if r0 <= 1e-12 {
        return None;
    }
    let max_lag = (n / 3).min((DEPIX_MAX_CELL * 2) as usize);
    if max_lag < 4 {
        return None;
    }
    let mut r = vec![0.0f32; max_lag + 1];
    for (lag, rv) in r.iter_mut().enumerate().take(max_lag + 1).skip(2) {
        let mut s = 0.0f32;
        for i in 0..n - lag {
            s += x[i] * x[i + lag];
        }
        *rv = s / r0;
    }
    Some(r)
}

/// Top autocorrelation local maxima, for failure diagnostics in the log.
fn peak_diagnostics(profile: &[f32]) -> String {
    let Some(r) = normalized_autocorr(profile) else {
        return "no-acf".to_string();
    };
    let mut peaks: Vec<(usize, f32)> = (3..r.len().saturating_sub(1))
        .filter(|&lag| r[lag] >= r[lag - 1] && r[lag] >= r[lag + 1])
        .map(|lag| (lag, r[lag]))
        .collect();
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    peaks
        .iter()
        .take(5)
        .map(|(l, v)| format!("{l}:{v:.2}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn dominant_pitch(profile: &[f32]) -> Option<(f32, f32)> {
    let r = normalized_autocorr(profile)?;
    let max_lag = r.len() - 1;
    // Isolated local maxima only. Every multiple of the true pitch peaks
    // about as high as the pitch itself, so take the SMALLEST peak within
    // 80% of the best one.
    let mut peaks: Vec<(usize, f32)> = Vec::new();
    for lag in 3..max_lag {
        if r[lag] >= DEPIX_MIN_ACF_PEAK && r[lag] >= r[lag - 1] && r[lag] >= r[lag + 1] {
            peaks.push((lag, r[lag]));
        }
    }
    let best = peaks.iter().map(|p| p.1).fold(0.0f32, f32::max);
    let (lag, strength) = *peaks.iter().find(|p| p.1 >= best * 0.8)?;
    let (a, b, c) = (r[lag - 1], r[lag], r[lag + 1]);
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() > 1e-9 {
        (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some((lag as f32 + delta, strength))
}

/// Best comb alignment for a float pitch: samples the profile at
/// phase + k*pitch (rounded) and returns (best phase, boundary mean over
/// profile mean). Phase resolution 0.25px.
fn comb_phase(profile: &[f32], pitch: f32) -> (f32, f32) {
    let n = profile.len();
    if n == 0 || pitch < 2.0 {
        return (0.0, 0.0);
    }
    let mean_all = profile.iter().sum::<f32>() / n as f32;
    let mut best = (0.0f32, 0.0f32);
    let mut phase = 0.0f32;
    while phase < pitch {
        let mut sum = 0.0f32;
        let mut count = 0u32;
        let mut b = phase;
        while b < (n as f32) - 0.5 {
            let i = b.round() as usize;
            if i < n {
                sum += profile[i];
                count += 1;
            }
            b += pitch;
        }
        if count >= 3 {
            let score = (sum / count as f32) / mean_all.max(1e-6);
            if score > best.1 {
                best = (phase, score);
            }
        }
        phase += 0.25;
    }
    best
}

/// Local search around an approximate pitch, maximizing the boundary-comb
/// score (this is what nails FRACTIONAL pitches: integer autocorrelation
/// lags can't, but the comb sampler tracks float boundaries exactly).
fn refine_pitch_range(profile: &[f32], approx: f32, range: f32) -> (f32, f32, f32) {
    let mut best = (approx, 0.0f32, 0.0f32);
    let mut p = approx - range;
    while p <= approx + range {
        if p >= 2.0 {
            let (phase, score) = comb_phase(profile, p);
            if score > best.2 {
                best = (p, phase, score);
            }
        }
        p += 0.05;
    }
    best
}

fn refine_pitch(profile: &[f32], approx: f32) -> (f32, f32, f32) {
    refine_pitch_range(profile, approx, 0.6)
}

/// One axis: autocorrelation peak (which may land on a HARMONIC — with
/// fractional pitch the integer lags re-align best at a multiple), then
/// try sub-multiples of that lag refined against the comb, preferring the
/// smallest pitch that scores comparably. Returns (pitch, phase, score).
fn detect_axis(profile: &[f32]) -> Option<(f32, f32, f32)> {
    let (lag, _) = dominant_pitch(profile)?;
    let mut candidates: Vec<(f32, f32, f32)> = Vec::new();
    for divisor in 1..=6u32 {
        let approx = lag / divisor as f32;
        if approx < 2.5 {
            break;
        }
        candidates.push(refine_pitch(profile, approx));
    }
    let best_score = candidates.iter().map(|c| c.2).fold(0.0f32, f32::max);
    candidates
        .iter()
        .rev() // smallest pitch first
        .find(|c| c.2 >= (best_score * 0.85).max(DEPIX_MIN_COMB_SCORE))
        .copied()
}

/// Both axes from precomputed profiles.
fn detect_from_profiles(gx: &[f32], gy: &[f32]) -> Option<MosaicGrid> {
    let (pitch_x, phase_x, _) = detect_axis(gx)?;
    let (pitch_y, phase_y, _) = detect_axis(gy)?;
    if pitch_x < DEPIX_MIN_CELL as f32 || pitch_y < DEPIX_MIN_CELL as f32 {
        return None;
    }
    Some(MosaicGrid {
        pitch_x,
        pitch_y,
        // Gradient index i is the edge between pixels i and i+1, so the
        // first cell starts at phase+1.
        bound_x: phase_x + 1.0,
        bound_y: phase_y + 1.0,
    })
}

/// Windowed vote: a mosaic that covers only part of the frame (or whose
/// comb is diluted by flat regions) is invisible to global profiles but
/// obvious inside a window that sits on it. Windows detect independently;
/// two or more agreeing on a pitch within 8% carries the vote, then the
/// phase is aligned globally at that pitch.
fn detect_windowed(img: &Rgb32FImage, gx_full: &[f32], gy_full: &[f32]) -> Option<MosaicGrid> {
    let (w, h) = img.dimensions();
    // Windows must be meaningfully smaller than the frame, or they inherit
    // the same dilution that defeated the global profiles.
    let win = (w.min(h) / 2).min(384);
    if win < 96 {
        return None;
    }
    let positions = |len: u32| -> Vec<u32> {
        if len <= win { vec![0] } else { vec![0, (len - win) / 2, len - win] }
    };
    let mut xs_pitches: Vec<f32> = Vec::new();
    let mut ys_pitches: Vec<f32> = Vec::new();
    for &y0 in &positions(h) {
        for &x0 in &positions(w) {
            let sub = image::imageops::crop_imm(img, x0, y0, win, win).to_image();
            let (sgx, sgy) = mosaic_gradient_profiles(&sub);
            if let Some((p, _, _)) = detect_axis(&sgx) {
                xs_pitches.push(p);
            }
            if let Some((p, _, _)) = detect_axis(&sgy) {
                ys_pitches.push(p);
            }
        }
    }
    let vote = |mut v: Vec<f32>| -> Option<f32> {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut best: Option<(usize, f32)> = None;
        let mut i = 0;
        while i < v.len() {
            let mut j = i;
            while j + 1 < v.len() && v[j + 1] <= v[i] * 1.08 {
                j += 1;
            }
            let count = j - i + 1;
            let median = v[i + (count / 2).min(j - i)];
            if best.is_none_or(|(bc, _)| count > bc) {
                best = Some((count, median));
            }
            i = j + 1;
        }
        best.filter(|(c, _)| *c >= 2).map(|(_, m)| m)
    };
    let px = vote(xs_pitches)?;
    let py = vote(ys_pitches)?;
    // The vote is the evidence; global phase alignment needs no threshold.
    let (pitch_x, phase_x, _) = refine_pitch(gx_full, px);
    let (pitch_y, phase_y, _) = refine_pitch(gy_full, py);
    log::info!("[enhance] de-pixelate windowed vote: pitch=({pitch_x:.2},{pitch_y:.2})");
    Some(MosaicGrid {
        pitch_x,
        pitch_y,
        bound_x: phase_x + 1.0,
        bound_y: phase_y + 1.0,
    })
}

/// Detects the pixelation grid, tolerating fractional pitch (resized
/// mosaics), softened edges (JPEG), very large blocks (octave fallback),
/// and mosaics covering only part of the frame (windowed vote).
pub fn detect_mosaic_grid(img: &Rgb32FImage) -> Option<MosaicGrid> {
    let (gx, gy) = mosaic_gradient_profiles(img);

    if let Some(g) = detect_from_profiles(&gx, &gy) {
        log::info!(
            "[enhance] de-pixelate detect: pitch=({:.2},{:.2})",
            g.pitch_x,
            g.pitch_y
        );
        return Some(g);
    }

    // Octaves: blocks larger than the autocorrelation range at native
    // resolution compress into range when the image is downscaled.
    let (w, h) = img.dimensions();
    for scale in [2u32, 4] {
        if w / scale < 64 || h / scale < 64 {
            break;
        }
        let small = image::imageops::resize(
            img,
            w / scale,
            h / scale,
            image::imageops::FilterType::Triangle,
        );
        let (sgx, sgy) = mosaic_gradient_profiles(&small);
        if let Some(sg) = detect_from_profiles(&sgx, &sgy) {
            let range = 0.8 * scale as f32;
            let (pitch_x, phase_x, sx) =
                refine_pitch_range(&gx, sg.pitch_x * scale as f32, range);
            let (pitch_y, phase_y, sy) =
                refine_pitch_range(&gy, sg.pitch_y * scale as f32, range);
            if sx.min(sy) >= DEPIX_MIN_COMB_SCORE * 0.9 {
                log::info!(
                    "[enhance] de-pixelate detect at 1/{scale}: pitch=({pitch_x:.2},{pitch_y:.2})"
                );
                return Some(MosaicGrid {
                    pitch_x,
                    pitch_y,
                    bound_x: phase_x + 1.0,
                    bound_y: phase_y + 1.0,
                });
            }
        }
    }

    if let Some(g) = detect_windowed(img, &gx, &gy) {
        return Some(g);
    }

    log::info!(
        "[enhance] de-pixelate: no grid found. acf peaks x=[{}] y=[{}]",
        peak_diagnostics(&gx),
        peak_diagnostics(&gy)
    );
    None
}

/// Grid for a KNOWN cell size (manual mode): pitch refined near the given
/// size (so "12" works on an 11.6px grid), phase auto-aligned, never fails.
fn grid_for_cell(img: &Rgb32FImage, cell: u32) -> MosaicGrid {
    let (gx, gy) = mosaic_gradient_profiles(img);
    let (pitch_x, phase_x, _) = refine_pitch(&gx, cell as f32);
    let (pitch_y, phase_y, _) = refine_pitch(&gy, cell as f32);
    MosaicGrid {
        pitch_x,
        pitch_y,
        bound_x: phase_x + 1.0,
        bound_y: phase_y + 1.0,
    }
}

/// Collapse each grid cell to its mean, then evaluate a Catmull-Rom
/// surface through the cell-center means at every original pixel. Output
/// has the input's dimensions with the grid dissolved into smooth ramps.
pub fn collapse_mosaic(img: &Rgb32FImage, grid: &MosaicGrid) -> Rgb32FImage {
    use rayon::prelude::*;

    let (w, h) = img.dimensions();
    let bx = grid.bound_x.rem_euclid(grid.pitch_x);
    let by = grid.bound_y.rem_euclid(grid.pitch_y);
    // Cell index for a coordinate: 0 for the partial head cell (before the
    // first boundary), then one per pitch.
    let idx_of = |v: f32, bound: f32, pitch: f32| -> i64 {
        if v < bound {
            0
        } else {
            ((v - bound) / pitch).floor() as i64 + 1
        }
    };
    let col_idx: Vec<usize> = (0..w)
        .map(|x| idx_of(x as f32, bx, grid.pitch_x) as usize)
        .collect();
    let row_idx: Vec<usize> = (0..h)
        .map(|y| idx_of(y as f32, by, grid.pitch_y) as usize)
        .collect();
    let nc = col_idx[w as usize - 1] + 1;
    let nr = row_idx[h as usize - 1] + 1;

    let mut sums = vec![[0.0f64; 3]; nc * nr];
    let mut counts = vec![0u32; nc * nr];
    for y in 0..h {
        let r = row_idx[y as usize];
        for x in 0..w {
            let c = col_idx[x as usize];
            let p = img.get_pixel(x, y);
            let idx = r * nc + c;
            sums[idx][0] += p[0] as f64;
            sums[idx][1] += p[1] as f64;
            sums[idx][2] += p[2] as f64;
            counts[idx] += 1;
        }
    }
    let means: Vec<[f32; 3]> = sums
        .iter()
        .zip(&counts)
        .map(|(s, &n)| {
            let n = n.max(1) as f64;
            [(s[0] / n) as f32, (s[1] / n) as f32, (s[2] / n) as f32]
        })
        .collect();

    // Fractional cell index for a pixel: interior cell k's center maps to
    // index k (head cell centers land slightly off; clamped sampling
    // absorbs the sub-cell edge distortion).
    let (pitch_x, pitch_y) = (grid.pitch_x, grid.pitch_y);
    let index_x = move |x: f32| -> f32 { (x - bx) / pitch_x + 0.5 };
    let index_y = move |y: f32| -> f32 { (y - by) / pitch_y + 0.5 };

    let catmull = |p0: f32, p1: f32, p2: f32, p3: f32, t: f32| -> f32 {
        let t2 = t * t;
        let t3 = t2 * t;
        0.5 * ((2.0 * p1)
            + (-p0 + p2) * t
            + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
            + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3)
    };
    let grid_at = |cx: i64, cy: i64, ch: usize| -> f32 {
        let cx = cx.clamp(0, nc as i64 - 1) as usize;
        let cy = cy.clamp(0, nr as i64 - 1) as usize;
        means[cy * nc + cx][ch]
    };

    let mut out = Rgb32FImage::new(w, h);
    out.as_mut()
        .par_chunks_mut(w as usize * 3)
        .enumerate()
        .for_each(|(y, row)| {
            let fy = index_y(y as f32);
            let cy0 = fy.floor() as i64;
            let ty = fy - fy.floor();
            for x in 0..w as usize {
                let fx = index_x(x as f32);
                let cx0 = fx.floor() as i64;
                let tx = fx - fx.floor();
                for ch in 0..3 {
                    let mut col = [0.0f32; 4];
                    for (j, cv) in col.iter_mut().enumerate() {
                        let cy = cy0 - 1 + j as i64;
                        *cv = catmull(
                            grid_at(cx0 - 1, cy, ch),
                            grid_at(cx0, cy, ch),
                            grid_at(cx0 + 1, cy, ch),
                            grid_at(cx0 + 2, cy, ch),
                            tx,
                        );
                    }
                    row[x * 3 + ch] = catmull(col[0], col[1], col[2], col[3], ty).clamp(0.0, 1.0);
                }
            }
        });
    out
}

/// Applies the de-pixelate prep. `requested_cell` of 0 means auto-detect;
/// a positive value forces that cell size (phase still auto-aligned).
/// Returns the prepared image and the approximate cell size used.
pub fn apply_depixelate(
    img: &Rgb32FImage,
    requested_cell: u32,
) -> Result<(Rgb32FImage, u32), String> {
    let grid = if requested_cell == 0 {
        detect_mosaic_grid(img).ok_or_else(|| {
            "No pixel grid detected — this image looks soft/low-res rather than \
             mosaic-pixelated. For blur and compression, run Restore with \
             De-pixelate OFF. Only set a manual cell size if you can see \
             actual square blocks."
                .to_string()
        })?
    } else {
        grid_for_cell(img, requested_cell.clamp(DEPIX_MIN_CELL, DEPIX_MAX_CELL))
    };
    let cell = grid.pitch_x.round().max(2.0) as u32;
    Ok((collapse_mosaic(img, &grid), cell))
}

#[cfg(test)]
mod depixelate_tests {
    use super::*;

    fn smooth_scene(w: u32, h: u32) -> Rgb32FImage {
        let mut img = Rgb32FImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let u = x as f32 / w as f32;
                let v = y as f32 / h as f32;
                img.put_pixel(
                    x,
                    y,
                    Rgb([
                        0.2 + 0.6 * u,
                        0.3 + 0.4 * v,
                        0.5 + 0.3 * (u * 6.0).sin() * (v * 5.0).cos(),
                    ]),
                );
            }
        }
        img
    }

    /// Mosaic with FLOAT pitch: boundaries at round(off + k*pitch), the
    /// shape of a mosaic that was resized after creation.
    fn mosaic_of(original: &Rgb32FImage, pitch: f32, off_x: f32, off_y: f32) -> Rgb32FImage {
        let (w, h) = original.dimensions();
        let bounds = |len: u32, off: f32| -> Vec<u32> {
            let mut b = vec![0u32];
            let mut v = off;
            while v < len as f32 {
                let r = v.round() as u32;
                if r > *b.last().unwrap() && r < len {
                    b.push(r);
                }
                v += pitch;
            }
            b.push(len);
            b
        };
        let xs = bounds(w, off_x);
        let ys = bounds(h, off_y);
        let mut out = original.clone();
        for yi in 0..ys.len() - 1 {
            for xi in 0..xs.len() - 1 {
                let mut acc = [0.0f32; 3];
                let mut n = 0u32;
                for y in ys[yi]..ys[yi + 1] {
                    for x in xs[xi]..xs[xi + 1] {
                        let p = original.get_pixel(x, y);
                        for c in 0..3 {
                            acc[c] += p[c];
                        }
                        n += 1;
                    }
                }
                for c in acc.iter_mut() {
                    *c /= n as f32;
                }
                for y in ys[yi]..ys[yi + 1] {
                    for x in xs[xi]..xs[xi + 1] {
                        out.put_pixel(x, y, Rgb(acc));
                    }
                }
            }
        }
        out
    }

    fn mae(a: &Rgb32FImage, b: &Rgb32FImage) -> f32 {
        let mut sum = 0.0f64;
        for (pa, pb) in a.pixels().zip(b.pixels()) {
            for c in 0..3 {
                sum += (pa[c] - pb[c]).abs() as f64;
            }
        }
        (sum / (a.width() * a.height() * 3) as f64) as f32
    }

    #[test]
    fn detects_and_dissolves_integer_mosaic() {
        let original = smooth_scene(240, 200);
        let mosaic = mosaic_of(&original, 8.0, 3.0, 5.0);
        let grid = detect_mosaic_grid(&mosaic).expect("integer grid not detected");
        assert!((grid.pitch_x - 8.0).abs() < 0.3, "pitch_x {}", grid.pitch_x);
        assert!((grid.pitch_y - 8.0).abs() < 0.3, "pitch_y {}", grid.pitch_y);
        let (restored, used) = apply_depixelate(&mosaic, 0).unwrap();
        assert_eq!(used, 8);
        assert!(
            mae(&restored, &original) < mae(&mosaic, &original) * 0.5,
            "collapse did not improve on the mosaic"
        );
    }

    /// The real-world case that defeats integer detection: a mosaic that
    /// was resized, so its block period is fractional.
    #[test]
    fn detects_and_dissolves_fractional_pitch_mosaic() {
        let original = smooth_scene(300, 240);
        let mosaic = mosaic_of(&original, 11.4, 4.0, 7.0);
        let grid = detect_mosaic_grid(&mosaic).expect("fractional grid not detected");
        assert!(
            (grid.pitch_x - 11.4).abs() < 0.6,
            "pitch_x {} not near 11.4",
            grid.pitch_x
        );
        let (restored, _) = apply_depixelate(&mosaic, 0).unwrap();
        assert!(
            mae(&restored, &original) < mae(&mosaic, &original) * 0.6,
            "fractional collapse did not improve enough"
        );
    }

    /// JPEG-style softened block edges must still be detectable.
    #[test]
    fn detects_soft_edged_mosaic() {
        let original = smooth_scene(240, 200);
        let mosaic = mosaic_of(&original, 10.0, 2.0, 6.0);
        let soft = image::imageops::blur(&mosaic, 0.8);
        let grid = detect_mosaic_grid(&soft).expect("soft grid not detected");
        assert!((grid.pitch_x - 10.0).abs() < 0.6, "pitch_x {}", grid.pitch_x);
    }

    /// A mosaic that covers only part of the frame: global profiles are
    /// diluted by the flat surround, so the windowed vote must carry it.
    #[test]
    fn detects_partial_coverage_mosaic() {
        let scene = smooth_scene(480, 400);
        let patch = mosaic_of(&scene, 9.0, 4.0, 2.0);
        // Flat frame with the mosaic pasted into the central region only.
        let mut img = Rgb32FImage::from_pixel(480, 400, Rgb([0.82f32, 0.8, 0.78]));
        for y in 100..300u32 {
            for x in 120..360u32 {
                img.put_pixel(x, y, *patch.get_pixel(x, y));
            }
        }
        let grid = detect_mosaic_grid(&img).expect("partial mosaic not detected");
        assert!(
            (grid.pitch_x - 9.0).abs() < 0.6,
            "pitch_x {} not near 9",
            grid.pitch_x
        );
    }

    /// Auto mode must refuse images without a grid instead of mangling them.
    #[test]
    fn auto_refuses_non_mosaic() {
        let mut img = Rgb32FImage::new(160, 160);
        let mut state = 11u32;
        for p in img.pixels_mut() {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let v = (state >> 16) as f32 / 65535.0;
            *p = Rgb([v, 1.0 - v, v * 0.5 + 0.25]);
        }
        assert!(detect_mosaic_grid(&img).is_none(), "noise misread as a grid");
        assert!(apply_depixelate(&img, 0).is_err());
    }
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
        assert_eq!(
            a.as_raw(),
            b.as_raw(),
            "same settings must reproduce exactly"
        );

        let c = blend_result(&raw, &original, 96, 64, 0.7, 0.9, 1.0);
        assert_ne!(
            a.as_raw(),
            c.as_raw(),
            "changing texture must change the result"
        );
        let d = blend_result(&raw, &original, 96, 64, 0.4, 0.5, 1.0);
        assert_ne!(
            a.as_raw(),
            d.as_raw(),
            "changing strength must change the result"
        );
    }

    fn mean_abs_diff(a: &Rgb32FImage, b: &Rgb32FImage) -> f32 {
        let sum: f32 = a
            .as_raw()
            .iter()
            .zip(b.as_raw().iter())
            .map(|(x, y)| (x - y).abs())
            .sum();
        sum / a.as_raw().len() as f32
    }

    /// The bug the user actually hit: when the model output is LARGER than
    /// the original (engine 2x previews, upscales), the original's texture
    /// and noise sit at a coarser pixel scale after resizing — a
    /// fixed-scale fine-detail band reads them as empty, silently turning
    /// Texture and Match grain into no-ops. Both must visibly bite on a 2x
    /// result.
    #[test]
    fn texture_and_grain_bite_on_upscaled_results() {
        let (raw, original) = test_pair();
        // Model output at 2x the original's size, clean (no noise).
        let raw_up = image::imageops::resize(&raw, 192, 128, image::imageops::FilterType::Lanczos3);

        let base = blend_result(&raw_up, &original, 192, 128, 1.0, 0.0, 0.0);
        let textured = blend_result(&raw_up, &original, 192, 128, 1.0, 1.0, 0.0);
        assert!(
            mean_abs_diff(&base, &textured) > 0.003,
            "texture must transfer the original's detail onto a 2x result (diff {})",
            mean_abs_diff(&base, &textured)
        );

        let grained = blend_result(&raw_up, &original, 192, 128, 1.0, 0.0, 1.0);
        assert!(
            mean_abs_diff(&base, &grained) > 0.003,
            "grain match must close the noise gap on a 2x result (diff {})",
            mean_abs_diff(&base, &grained)
        );
    }
}
