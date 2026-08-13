use std::io::Cursor;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, GrayImage, ImageFormat, RgbaImage};
use ort::session::Session;
use tauri::Emitter;

use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use crate::formats::is_raw_file;
use crate::model_registry::{TaskType, resolve_and_prepare};

/// Maximum pixels for the expanded canvas — expansion is a compositional
/// tool, not an upscaler, so the fill runs at a bounded resolution.
const MAX_EXPANDED_PIXELS: u64 = 48_000_000;
/// How far the fill blends into the original image at the seam.
const SEAM_OVERLAP: u32 = 24;

/// Builds the expanded canvas (image offset by the new left/top margins)
/// and the mask marking everything the model must fill.
pub fn build_canvas_and_mask(
    image: &DynamicImage,
    add_left: u32,
    add_top: u32,
    add_right: u32,
    add_bottom: u32,
) -> Result<(RgbaImage, GrayImage)> {
    let (w, h) = image.dimensions();
    let new_w = w + add_left + add_right;
    let new_h = h + add_top + add_bottom;
    if new_w as u64 * new_h as u64 > MAX_EXPANDED_PIXELS {
        return Err(anyhow!(
            "The expanded image would be {}x{} — too large. Reduce the expansion.",
            new_w,
            new_h
        ));
    }

    let mut canvas = RgbaImage::new(new_w, new_h);
    // Prefill the new area by replicating the nearest edge pixel: gives the
    // inpainting model a color hint and avoids hard black borders bleeding
    // into the fill.
    let rgba = image.to_rgba8();
    for y in 0..new_h {
        for x in 0..new_w {
            let sx = x.saturating_sub(add_left).min(w - 1);
            let sy = y.saturating_sub(add_top).min(h - 1);
            canvas.put_pixel(x, y, *rgba.get_pixel(sx, sy));
        }
    }

    let mut mask = GrayImage::new(new_w, new_h);
    for y in 0..new_h {
        for x in 0..new_w {
            // Mask = everything outside the original image bounds, plus a
            // small seam band just inside each expanded edge so the model
            // can blend into real content.
            let in_original =
                x >= add_left && x < add_left + w && y >= add_top && y < add_top + h;
            let near_left = add_left > 0 && x < add_left + SEAM_OVERLAP;
            let near_top = add_top > 0 && y < add_top + SEAM_OVERLAP;
            let near_right = add_right > 0 && x + SEAM_OVERLAP >= add_left + w;
            let near_bottom = add_bottom > 0 && y + SEAM_OVERLAP >= add_top + h;
            let masked = !in_original || near_left || near_top || near_right || near_bottom;
            mask.put_pixel(x, y, image::Luma([if masked { 255 } else { 0 }]));
        }
    }
    Ok((canvas, mask))
}

/// Runs one fill pass at a given working resolution and returns the result
/// composited back at canvas resolution. Different working resolutions give
/// the model different context, producing genuinely different fills — the
/// source of the "variants".
pub fn fill_variant(
    canvas: &RgbaImage,
    mask: &GrayImage,
    session: &Mutex<Session>,
    work_dim: u32,
) -> Result<RgbaImage> {
    let (w, h) = canvas.dimensions();
    let scale = (work_dim as f32 / w.max(h) as f32).min(1.0);
    let (ww, wh) = (
        ((w as f32 * scale) as u32).max(64),
        ((h as f32 * scale) as u32).max(64),
    );

    let small_canvas = image::imageops::resize(canvas, ww, wh, FilterType::Lanczos3);
    let small_mask = image::imageops::resize(mask, ww, wh, FilterType::Triangle);

    // 8-bit canvas input → result comes back in the same (non-gamma) space.
    let (filled, _) = crate::ai_processing::run_lama_inpainting(
        &DynamicImage::ImageRgba8(small_canvas),
        &small_mask,
        session,
    )?;

    // Upscale the fill back and composite only where masked, keeping the
    // original pixels untouched at full resolution.
    let filled_full = image::imageops::resize(&filled, w, h, FilterType::Lanczos3);
    // Feather the composite alpha for a seamless hand-off to the original.
    let soft_mask = image::imageops::blur(mask, (w.max(h) as f32 / 300.0).max(3.0));
    let mut result = canvas.clone();
    for (x, y, m) in soft_mask.enumerate_pixels() {
        if m[0] > 0 {
            let alpha = m[0] as f32 / 255.0;
            let src = filled_full.get_pixel(x, y);
            let dst = result.get_pixel_mut(x, y);
            for c in 0..3 {
                dst[c] = (src[c] as f32 * alpha + dst[c] as f32 * (1.0 - alpha)) as u8;
            }
            dst[3] = 255;
        }
    }
    Ok(result)
}

/// Downscales canvas+mask to the fill model's working size (long edge
/// ~1216, multiple of 8) and encodes them as PNGs for the engine.
pub(crate) fn engine_canvas_pngs(canvas: &RgbaImage, mask: &GrayImage) -> Result<(Vec<u8>, Vec<u8>, u32, u32), String> {
    let (w, h) = canvas.dimensions();
    let scale = (1216.0 / w.max(h) as f32).min(1.0);
    let round8 = |v: f32| ((v / 8.0).round() as u32).max(8) * 8;
    let (ww, wh) = (round8(w as f32 * scale), round8(h as f32 * scale));
    let small_canvas = image::imageops::resize(canvas, ww, wh, FilterType::Lanczos3);
    // Feathered mask: soft transitions avoid tone steps at the fill seam.
    let small_mask = image::imageops::blur(
        &image::imageops::resize(mask, ww, wh, FilterType::Triangle),
        4.0,
    );
    let mut cbuf = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(small_canvas)
        .to_rgb8()
        .write_to(&mut cbuf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut mbuf = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(small_mask)
        .to_rgb8()
        .write_to(&mut mbuf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok((cbuf.into_inner(), mbuf.into_inner(), ww, wh))
}

/// Composites an engine fill (at working resolution) back onto the
/// full-resolution canvas, touching only masked pixels.
fn composite_engine_fill(canvas: &RgbaImage, mask: &GrayImage, fill_png: &[u8]) -> Result<RgbaImage, String> {
    let (w, h) = canvas.dimensions();
    let filled = image::load_from_memory(fill_png)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let filled_full = image::imageops::resize(&filled, w, h, FilterType::Lanczos3);
    // Feather the composite alpha for a seamless hand-off to the original.
    let soft_mask = image::imageops::blur(mask, (w.max(h) as f32 / 300.0).max(3.0));
    let mut result = canvas.clone();
    for (x, y, m) in soft_mask.enumerate_pixels() {
        if m[0] > 0 {
            let alpha = m[0] as f32 / 255.0;
            let src = filled_full.get_pixel(x, y);
            let dst = result.get_pixel_mut(x, y);
            for c in 0..3 {
                dst[c] = (src[c] as f32 * alpha + dst[c] as f32 * (1.0 - alpha)) as u8;
            }
            dst[3] = 255;
        }
    }
    Ok(result)
}

fn encode_preview(image: &RgbaImage) -> Result<String, String> {
    const MAX_PREVIEW_DIM: u32 = 1600;
    let dynamic = DynamicImage::ImageRgba8(image.clone());
    let (w, h) = dynamic.dimensions();
    let preview = if w.max(h) > MAX_PREVIEW_DIM {
        dynamic.resize(MAX_PREVIEW_DIM, MAX_PREVIEW_DIM, FilterType::Triangle)
    } else {
        dynamic
    };
    let mut buf = Cursor::new(Vec::new());
    preview
        .to_rgb8()
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("Failed to encode expansion preview: {}", e))?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(buf.get_ref())
    ))
}

/// Expands the photo's canvas by the given fractions of its size (0.25 =
/// 25% of the width/height added on that side) and fills the new area with
/// the registry's inpainting model. Produces several variants; full-res
/// results are held in memory until one is saved.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn apply_expansion(
    path: String,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    js_adjustments: Option<serde_json::Value>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // Working resolutions for the three variants: different context sizes
    // make the (deterministic) model produce different fills.
    const VARIANT_DIMS: [u32; 3] = [768, 576, 1024];

    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Inpaint,
        "inpaint",
        |_| true,
    )
    .await
    .map_err(|e| e.to_string())?;

    let (source_path, _) = parse_virtual_path(&path);
    let path_str = source_path.to_string_lossy().to_string();
    let rgb_loaded =
        crate::enhancement::enhancement_input(&path_str, js_adjustments.as_ref(), &state, &app_handle)?;

    if model.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy") {
        let kind = crate::comfy_engine::FillKind::from_params(&model.manifest.params);
        return apply_expansion_engine(rgb_loaded, left, top, right, bottom, kind, app_handle, state)
            .await;
    }

    let session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;
    let results_handle = state.expansion_results.clone();

    tokio::task::spawn_blocking(move || {
        let run = || -> Result<Vec<RgbaImage>, String> {
            let rgb_input = rgb_loaded;
            let (w, h) = rgb_input.dimensions();
            let clamp_frac = |f: f32| f.clamp(0.0, 1.0);
            let add_left = (clamp_frac(left) * w as f32) as u32;
            let add_right = (clamp_frac(right) * w as f32) as u32;
            let add_top = (clamp_frac(top) * h as f32) as u32;
            let add_bottom = (clamp_frac(bottom) * h as f32) as u32;
            if add_left + add_right + add_top + add_bottom == 0 {
                return Err("Drag the frame outward first — nothing to expand.".to_string());
            }

            let _ = app_handle.emit("expand-progress", "Preparing canvas...");
            let (canvas, mask) = build_canvas_and_mask(
                &DynamicImage::ImageRgb32F(rgb_input),
                add_left,
                add_top,
                add_right,
                add_bottom,
            )
            .map_err(|e| e.to_string())?;

            let mut variants = Vec::new();
            for (i, dim) in VARIANT_DIMS.iter().enumerate() {
                let _ = app_handle.emit(
                    "expand-progress",
                    format!("Generating variant {}/{}...", i + 1, VARIANT_DIMS.len()),
                );
                variants.push(fill_variant(&canvas, &mask, &session, *dim).map_err(|e| e.to_string())?);
            }
            Ok(variants)
        };

        match run() {
            Ok(variants) => {
                let previews: Result<Vec<String>, String> =
                    variants.iter().map(encode_preview).collect();
                match previews {
                    Ok(previews) => {
                        *results_handle.lock().unwrap() = variants;
                        let _ = app_handle
                            .emit("expand-complete", serde_json::json!({ "variants": previews }));
                    }
                    Err(e) => {
                        let _ = app_handle.emit("expand-error", e);
                    }
                }
            }
            Err(e) => {
                let _ = app_handle.emit("expand-error", e);
            }
        }
    })
    .await
    .map_err(|e| format!("Expansion task failed: {}", e))
}

/// Generative expansion through the engine: three different seeds give
/// three genuinely different fills.
#[allow(clippy::too_many_arguments)]
async fn apply_expansion_engine(
    rgb_loaded: image::Rgb32FImage,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    kind: crate::comfy_engine::FillKind,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    const SEEDS: [u64; 3] = [42, 1337, 90210];

    let build = tokio::task::spawn_blocking(move || {
        let (w, h) = rgb_loaded.dimensions();
        let clamp_frac = |f: f32| f.clamp(0.0, 1.0);
        let add_left = (clamp_frac(left) * w as f32) as u32;
        let add_right = (clamp_frac(right) * w as f32) as u32;
        let add_top = (clamp_frac(top) * h as f32) as u32;
        let add_bottom = (clamp_frac(bottom) * h as f32) as u32;
        if add_left + add_right + add_top + add_bottom == 0 {
            return Err("Drag the frame outward first — nothing to expand.".to_string());
        }
        let (canvas, mask) = build_canvas_and_mask(
            &DynamicImage::ImageRgb32F(rgb_loaded),
            add_left,
            add_top,
            add_right,
            add_bottom,
        )
        .map_err(|e| e.to_string())?;
        let (canvas_png, mask_png, _, _) = engine_canvas_pngs(&canvas, &mask)?;
        Ok((canvas, mask, canvas_png, mask_png))
    })
    .await
    .map_err(|e| e.to_string())?;
    let (canvas, mask, canvas_png, mask_png) = match build {
        Ok(v) => v,
        Err(e) => {
            let _ = app_handle.emit("expand-error", e.clone());
            return Ok(());
        }
    };

    let mut variants: Vec<RgbaImage> = Vec::new();
    for (i, seed) in SEEDS.iter().enumerate() {
        let progress_handle = app_handle.clone();
        let label = format!("Generating variant {}/{}", i + 1, SEEDS.len());
        let fill = crate::comfy_engine::run_generative_fill(
            &app_handle,
            &state,
            kind,
            canvas_png.clone(),
            mask_png.clone(),
            "",
            *seed,
            move |msg| {
                let _ = progress_handle.emit("expand-progress", format!("{label} — {msg}"));
            },
        )
        .await;
        match fill {
            Ok(png) => match composite_engine_fill(&canvas, &mask, &png) {
                Ok(v) => variants.push(v),
                Err(e) => {
                    let _ = app_handle.emit("expand-error", e);
                    return Ok(());
                }
            },
            Err(e) => {
                let _ = app_handle.emit("expand-error", e.to_string());
                return Ok(());
            }
        }
    }

    let previews: Result<Vec<String>, String> = variants.iter().map(encode_preview).collect();
    match previews {
        Ok(previews) => {
            *state.expansion_results.lock().unwrap() = variants;
            let _ = app_handle.emit("expand-complete", serde_json::json!({ "variants": previews }));
        }
        Err(e) => {
            let _ = app_handle.emit("expand-error", e);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn save_expanded_image(
    original_path_str: String,
    variant_index: usize,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let variant = {
        let results = state.expansion_results.lock().unwrap();
        results
            .get(variant_index)
            .cloned()
            .ok_or("No expanded image found in memory for that variant.")?
    };

    let is_raw = is_raw_file(&original_path_str);
    let (first_path, source_sidecar_path) = parse_virtual_path(&original_path_str);
    let parent_dir = first_path
        .parent()
        .ok_or_else(|| "Could not determine parent directory.".to_string())?;
    let stem = first_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("expanded");

    let dynamic = DynamicImage::ImageRgba8(variant);
    let (output_filename, image_to_save): (String, DynamicImage) = if is_raw {
        (
            format!("{}_Expanded.tiff", stem),
            DynamicImage::ImageRgb16(dynamic.to_rgb16()),
        )
    } else {
        (
            format!("{}_Expanded.png", stem),
            DynamicImage::ImageRgb8(dynamic.to_rgb8()),
        )
    };

    let output_path = parent_dir.join(output_filename);
    image_to_save
        .save(&output_path)
        .map_err(|e| format!("Failed to save image: {}", e))?;

    let (real_path, _) = parse_virtual_path(&original_path_str);
    let _ =
        crate::exif_processing::write_rrexif_sidecar(&real_path.to_string_lossy(), &output_path);
    // Deliberately NOT copying the source sidecar: edits are already baked
    // into this file, so inheriting them would re-apply crop/rotation/AI
    // patches on top of the baked pixels (tilted/warped display).
    let _ = source_sidecar_path;

    Ok(output_path.to_string_lossy().to_string())
}
