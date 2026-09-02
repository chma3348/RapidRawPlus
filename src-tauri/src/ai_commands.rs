use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose};
use image::{
    DynamicImage, GenericImageView, GrayImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage,
};
use serde_json::Value;
use tauri::Manager;

use crate::ai_connector;
use crate::ai_processing::{
    self, AiDepthMaskParameters, AiForegroundMaskParameters, AiSkyMaskParameters,
    AiSubjectMaskParameters, CachedDepthMap, ensure_ai_state, generate_image_embeddings,
    run_depth_anything_model, run_sam_decoder, run_sky_seg_model, run_u2netp_model,
};
use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::cache_utils::GEOMETRY_KEYS;
use crate::image_loader::composite_patches_on_image;
use crate::image_processing::{apply_flip, apply_unwarp_geometry};
use crate::mask_generation::{
    AiPatchDefinition, MaskDefinition, SubMask, SubMaskMode, generate_mask_bitmap,
};
use crate::model_registry::{TaskType, mask_subtype_filter, resolve_and_prepare};
use crate::{
    get_cached_full_warped_image, get_full_image_for_processing, resolve_warped_image_for_masks,
};

fn encode_to_base64_png(image: &GrayImage) -> Result<String, String> {
    let mut buf = Cursor::new(Vec::new());
    image
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let base64_str = general_purpose::STANDARD.encode(buf.get_ref());
    Ok(format!("data:image/png;base64,{}", base64_str))
}

#[tauri::command]
pub async fn generate_ai_foreground_mask(
    js_adjustments: serde_json::Value,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiForegroundMaskParameters, String> {
    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Mask,
        "mask_foreground",
        mask_subtype_filter("foreground"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;

    let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;

    let full_mask_image =
        run_u2netp_model(warped_image.as_ref(), &session).map_err(|e| e.to_string())?;
    let base64_data = encode_to_base64_png(&full_mask_image)?;

    Ok(AiForegroundMaskParameters {
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
    })
}

#[tauri::command]
pub async fn generate_ai_sky_mask(
    js_adjustments: serde_json::Value,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSkyMaskParameters, String> {
    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Mask,
        "mask_sky",
        mask_subtype_filter("sky"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;

    let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;

    let full_mask_image =
        run_sky_seg_model(warped_image.as_ref(), &session).map_err(|e| e.to_string())?;
    let base64_data = encode_to_base64_png(&full_mask_image)?;

    Ok(AiSkyMaskParameters {
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_depth_mask(
    js_adjustments: serde_json::Value,
    path: String,
    min_depth: f32,
    max_depth: f32,
    min_fade: f32,
    max_fade: f32,
    feather: f32,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiDepthMaskParameters, String> {
    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Mask,
        "mask_depth",
        mask_subtype_filter("depth"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;

    let path_hash = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_bytes());
        hasher.update(model.manifest.id.as_bytes());
        let mut geo_hasher = DefaultHasher::new();
        for key in GEOMETRY_KEYS {
            if let Some(val) = js_adjustments.get(key) {
                key.hash(&mut geo_hasher);
                val.to_string().hash(&mut geo_hasher);
            }
        }
        hasher.update(&geo_hasher.finish().to_le_bytes());
        hasher.finalize().to_hex().to_string()
    };

    ensure_ai_state(&state.ai_state);
    let cached_depth = {
        let mut ai_state_lock = state.ai_state.lock().unwrap();
        let ai_state = ai_state_lock.as_mut().unwrap();

        if let Some(cached) = &ai_state.depth_map {
            if cached.path_hash == path_hash {
                cached.clone()
            } else {
                let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;
                let depth_img = run_depth_anything_model(warped_image.as_ref(), &session)
                    .map_err(|e| e.to_string())?;
                let new_cache = CachedDepthMap {
                    path_hash: path_hash.clone(),
                    depth_image: depth_img,
                    original_size: (warped_image.width(), warped_image.height()),
                };
                ai_state.depth_map = Some(new_cache.clone());
                new_cache
            }
        } else {
            let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;
            let depth_img = run_depth_anything_model(warped_image.as_ref(), &session)
                .map_err(|e| e.to_string())?;
            let new_cache = CachedDepthMap {
                path_hash: path_hash.clone(),
                depth_image: depth_img,
                original_size: (warped_image.width(), warped_image.height()),
            };
            ai_state.depth_map = Some(new_cache.clone());
            new_cache
        }
    };

    let raw_depth_fullres = image::imageops::resize(
        &cached_depth.depth_image,
        cached_depth.original_size.0,
        cached_depth.original_size.1,
        image::imageops::FilterType::Triangle,
    );

    let base64_data = encode_to_base64_png(&raw_depth_fullres)?;

    Ok(AiDepthMaskParameters {
        min_depth,
        max_depth,
        min_fade,
        max_fade,
        feather,
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_subject_mask(
    js_adjustments: serde_json::Value,
    path: String,
    start_point: (f64, f64),
    end_point: (f64, f64),
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSubjectMaskParameters, String> {
    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Mask,
        "mask_subject",
        mask_subtype_filter("subject"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let encoder_session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;
    let decoder_session = registry
        .get_session(&model.manifest.id, Some("decoder"))
        .map_err(|e| e.to_string())?;

    let path_hash = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_bytes());
        hasher.update(model.manifest.id.as_bytes());
        let mut geo_hasher = DefaultHasher::new();
        for key in GEOMETRY_KEYS {
            if let Some(val) = js_adjustments.get(key) {
                key.hash(&mut geo_hasher);
                val.to_string().hash(&mut geo_hasher);
            }
        }
        hasher.update(&geo_hasher.finish().to_le_bytes());
        hasher.finalize().to_hex().to_string()
    };

    ensure_ai_state(&state.ai_state);
    let embeddings = {
        let mut ai_state_lock = state.ai_state.lock().unwrap();
        let ai_state = ai_state_lock.as_mut().unwrap();

        if let Some(cached_embeddings) = &ai_state.embeddings {
            if cached_embeddings.path_hash == path_hash {
                cached_embeddings.clone()
            } else {
                let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;
                let mut new_embeddings =
                    generate_image_embeddings(warped_image.as_ref(), &encoder_session)
                        .map_err(|e| e.to_string())?;
                new_embeddings.path_hash = path_hash.clone();
                ai_state.embeddings = Some(new_embeddings.clone());
                new_embeddings
            }
        } else {
            let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;
            let mut new_embeddings =
                generate_image_embeddings(warped_image.as_ref(), &encoder_session)
                    .map_err(|e| e.to_string())?;
            new_embeddings.path_hash = path_hash.clone();
            ai_state.embeddings = Some(new_embeddings.clone());
            new_embeddings
        }
    };

    let (img_w, img_h) = embeddings.original_size;

    let (coarse_rotated_w, coarse_rotated_h) = if orientation_steps % 2 == 1 {
        (img_h as f64, img_w as f64)
    } else {
        (img_w as f64, img_h as f64)
    };

    let center = (coarse_rotated_w / 2.0, coarse_rotated_h / 2.0);

    let p1 = start_point;
    let p2 = (start_point.0, end_point.1);
    let p3 = end_point;
    let p4 = (end_point.0, start_point.1);

    let angle_rad = (rotation as f64).to_radians();
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();

    let unrotate = |p: (f64, f64)| {
        let px = p.0 - center.0;
        let py = p.1 - center.1;
        let new_px = px * cos_a + py * sin_a + center.0;
        let new_py = -px * sin_a + py * cos_a + center.1;
        (new_px, new_py)
    };

    let up1 = unrotate(p1);
    let up2 = unrotate(p2);
    let up3 = unrotate(p3);
    let up4 = unrotate(p4);

    let unflip = |p: (f64, f64)| {
        let mut new_px = p.0;
        let mut new_py = p.1;
        if flip_horizontal {
            new_px = coarse_rotated_w - p.0;
        }
        if flip_vertical {
            new_py = coarse_rotated_h - p.1;
        }
        (new_px, new_py)
    };

    let ufp1 = unflip(up1);
    let ufp2 = unflip(up2);
    let ufp3 = unflip(up3);
    let ufp4 = unflip(up4);

    let un_coarse_rotate = |p: (f64, f64)| -> (f64, f64) {
        match orientation_steps {
            0 => p,
            1 => (p.1, img_h as f64 - p.0),
            2 => (img_w as f64 - p.0, img_h as f64 - p.1),
            3 => (img_w as f64 - p.1, p.0),
            _ => p,
        }
    };

    let ucrp1 = un_coarse_rotate(ufp1);
    let ucrp2 = un_coarse_rotate(ufp2);
    let ucrp3 = un_coarse_rotate(ufp3);
    let ucrp4 = un_coarse_rotate(ufp4);

    let min_x = ucrp1.0.min(ucrp2.0).min(ucrp3.0).min(ucrp4.0);
    let min_y = ucrp1.1.min(ucrp2.1).min(ucrp3.1).min(ucrp4.1);
    let max_x = ucrp1.0.max(ucrp2.0).max(ucrp3.0).max(ucrp4.0);
    let max_y = ucrp1.1.max(ucrp2.1).max(ucrp3.1).max(ucrp4.1);

    let unrotated_start_point = (min_x, min_y);
    let unrotated_end_point = (max_x, max_y);

    let mask_bitmap = run_sam_decoder(
        &decoder_session,
        &embeddings,
        unrotated_start_point,
        unrotated_end_point,
    )
    .map_err(|e| e.to_string())?;
    let base64_data = encode_to_base64_png(&mask_bitmap)?;

    Ok(AiSubjectMaskParameters {
        start_x: start_point.0,
        start_y: start_point.1,
        end_x: end_point.0,
        end_y: end_point.1,
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
    })
}

/// Paint-to-select: the user's rough brush strokes (display space) are
/// un-transformed to image space, rasterized, and handed to SAM as a
/// multi-point + mask-prior prompt.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn generate_ai_paint_mask(
    js_adjustments: serde_json::Value,
    path: String,
    lines: serde_json::Value,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSubjectMaskParameters, String> {
    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Mask,
        "mask_subject",
        mask_subtype_filter("subject"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let encoder_session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;
    let decoder_session = registry
        .get_session(&model.manifest.id, Some("decoder"))
        .map_err(|e| e.to_string())?;

    let path_hash = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_bytes());
        hasher.update(model.manifest.id.as_bytes());
        let mut geo_hasher = DefaultHasher::new();
        for key in GEOMETRY_KEYS {
            if let Some(val) = js_adjustments.get(key) {
                key.hash(&mut geo_hasher);
                val.to_string().hash(&mut geo_hasher);
            }
        }
        hasher.update(&geo_hasher.finish().to_le_bytes());
        hasher.finalize().to_hex().to_string()
    };

    ensure_ai_state(&state.ai_state);
    let embeddings = {
        let mut ai_state_lock = state.ai_state.lock().unwrap();
        let ai_state = ai_state_lock.as_mut().unwrap();
        if let Some(cached) = &ai_state.embeddings
            && cached.path_hash == path_hash
        {
            cached.clone()
        } else {
            let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;
            let mut new_embeddings =
                generate_image_embeddings(warped_image.as_ref(), &encoder_session)
                    .map_err(|e| e.to_string())?;
            new_embeddings.path_hash = path_hash.clone();
            ai_state.embeddings = Some(new_embeddings.clone());
            new_embeddings
        }
    };

    let (img_w, img_h) = embeddings.original_size;
    let (coarse_rotated_w, coarse_rotated_h) = if orientation_steps % 2 == 1 {
        (img_h as f64, img_w as f64)
    } else {
        (img_w as f64, img_h as f64)
    };
    let center = (coarse_rotated_w / 2.0, coarse_rotated_h / 2.0);
    let angle_rad = (rotation as f64).to_radians();
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();

    // Same un-transform chain the click prompt uses, applied per point.
    let to_image_space = |p: (f64, f64)| -> (f64, f64) {
        let px = p.0 - center.0;
        let py = p.1 - center.1;
        let mut x = px * cos_a + py * sin_a + center.0;
        let mut y = -px * sin_a + py * cos_a + center.1;
        if flip_horizontal {
            x = coarse_rotated_w - x;
        }
        if flip_vertical {
            y = coarse_rotated_h - y;
        }
        match orientation_steps {
            1 => (y, img_h as f64 - x),
            2 => (img_w as f64 - x, img_h as f64 - y),
            3 => (img_w as f64 - y, x),
            _ => (x, y),
        }
    };

    // Rasterize the strokes as filled discs along each segment.
    let mut paint = GrayImage::new(img_w, img_h);
    let empty = Vec::new();
    let lines_arr = lines.as_array().unwrap_or(&empty);
    let mut stamped = 0u32;
    for line in lines_arr {
        let radius = (line["brushSize"].as_f64().unwrap_or(50.0) / 2.0).max(4.0);
        let pts: Vec<(f64, f64)> = line["points"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| Some((p["x"].as_f64()?, p["y"].as_f64()?)))
                    .map(&to_image_space)
                    .collect()
            })
            .unwrap_or_default();
        let stamp = |paint: &mut GrayImage, cx: f64, cy: f64| {
            let r = radius as i64;
            for dy in -r..=r {
                for dx in -r..=r {
                    if (dx * dx + dy * dy) as f64 <= radius * radius {
                        let x = cx as i64 + dx;
                        let y = cy as i64 + dy;
                        if x >= 0 && y >= 0 && (x as u32) < img_w && (y as u32) < img_h {
                            paint.put_pixel(x as u32, y as u32, image::Luma([255]));
                        }
                    }
                }
            }
        };
        if pts.len() == 1 {
            stamp(&mut paint, pts[0].0, pts[0].1);
            stamped += 1;
        }
        for pair in pts.windows(2) {
            let (x1, y1) = pair[0];
            let (x2, y2) = pair[1];
            let dist = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
            let steps = ((dist / (radius / 2.0)).ceil() as usize).max(1);
            for s in 0..=steps {
                let t = s as f64 / steps as f64;
                stamp(&mut paint, x1 + (x2 - x1) * t, y1 + (y2 - y1) * t);
            }
            stamped += 1;
        }
    }
    if stamped == 0 {
        return Err("Paint over the subject first, then release.".to_string());
    }

    let mask_bitmap =
        ai_processing::run_sam_decoder_with_paint(&decoder_session, &embeddings, &paint)
            .map_err(|e| e.to_string())?;
    let base64_data = encode_to_base64_png(&mask_bitmap)?;

    Ok(AiSubjectMaskParameters {
        start_x: 0.0,
        start_y: 0.0,
        end_x: img_w as f64,
        end_y: img_h as f64,
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
    })
}

#[tauri::command]
pub async fn precompute_ai_subject_mask(
    js_adjustments: serde_json::Value,
    path: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Mask,
        "mask_subject",
        mask_subtype_filter("subject"),
    )
    .await
    .map_err(|e| e.to_string())?;
    let encoder_session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;

    let path_hash = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_bytes());
        hasher.update(model.manifest.id.as_bytes());
        let mut geo_hasher = DefaultHasher::new();
        for key in GEOMETRY_KEYS {
            if let Some(val) = js_adjustments.get(key) {
                key.hash(&mut geo_hasher);
                val.to_string().hash(&mut geo_hasher);
            }
        }
        hasher.update(&geo_hasher.finish().to_le_bytes());
        hasher.finalize().to_hex().to_string()
    };

    ensure_ai_state(&state.ai_state);
    let mut ai_state_lock = state.ai_state.lock().unwrap();
    let ai_state = ai_state_lock.as_mut().unwrap();

    if let Some(cached_embeddings) = &ai_state.embeddings
        && cached_embeddings.path_hash == path_hash
    {
        return Ok(());
    }

    let warped_image = get_cached_full_warped_image(&state, &js_adjustments)?;
    let mut new_embeddings = generate_image_embeddings(warped_image.as_ref(), &encoder_session)
        .map_err(|e| e.to_string())?;

    new_embeddings.path_hash = path_hash.clone();
    ai_state.embeddings = Some(new_embeddings);

    Ok(())
}

#[tauri::command]
pub async fn check_ai_connector_status(app_handle: tauri::AppHandle) {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let is_connected = if let Some(address) = settings.ai_connector_address {
        ai_connector::check_status(&address).await.unwrap_or(false)
    } else {
        false
    };
    use tauri::Emitter;
    let _ = app_handle.emit(
        "ai-connector-status-update",
        serde_json::json!({ "connected": is_connected }),
    );
}

#[tauri::command]
pub async fn test_ai_connector_connection(address: String) -> Result<(), String> {
    match ai_connector::check_status(&address).await {
        Ok(true) => Ok(()),
        Ok(false) => Err("Server reachable but returned bad health status".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Morphological dilation (separable box max) — grows the removal mask so
/// near-miss brush edges swallow the object's outline.
fn dilate_mask(mask: &GrayImage, radius: u32) -> GrayImage {
    let (w, h) = mask.dimensions();
    let r = radius as i64;
    let mut tmp = GrayImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for dx in -r..=r {
                let xx = x as i64 + dx;
                if xx >= 0 && xx < w as i64 {
                    m = m.max(mask.get_pixel(xx as u32, y)[0]);
                }
            }
            tmp.put_pixel(x, y, image::Luma([m]));
        }
    }
    let mut out = GrayImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut m = 0u8;
            for dy in -r..=r {
                let yy = y as i64 + dy;
                if yy >= 0 && yy < h as i64 {
                    m = m.max(tmp.get_pixel(x, yy as u32)[0]);
                }
            }
            out.put_pixel(x, y, image::Luma([m]));
        }
    }
    out
}

/// Lists .cube LUTs from the managed folder
/// (~/Documents/RapidRAW Models/luts/**), grouped by pack. Input space is
/// inferred from the filename: Fujifilm's official film-sim cubes are
/// named `FLog2C_to_*` and expect F-Log2C-encoded input (see each pack's
/// SOURCES.md for provenance).
#[tauri::command]
pub async fn list_managed_luts() -> Result<Vec<serde_json::Value>, String> {
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let root = std::path::PathBuf::from(home).join("Documents/RapidRAW Models/luts");
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    let packs = std::fs::read_dir(&root).map_err(|e| e.to_string())?;
    for pack in packs.flatten() {
        let pack_path = pack.path();
        if !pack_path.is_dir() {
            continue;
        }
        let pack_name = pack.file_name().to_string_lossy().to_string();
        let Ok(files) = std::fs::read_dir(&pack_path) else {
            continue;
        };
        let mut entries: Vec<_> = files
            .flatten()
            .filter(|f| {
                f.path()
                    .extension()
                    .map(|e| e.eq_ignore_ascii_case("cube"))
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_by_key(|f| f.file_name());
        for f in entries {
            let file_name = f.file_name().to_string_lossy().to_string();
            let is_flog2c = file_name.to_ascii_lowercase().starts_with("flog2c_to_");
            // Display name: strip the transform prefix and grid suffix.
            let name = file_name
                .trim_end_matches(".cube")
                .trim_start_matches("FLog2C_to_")
                .split("_65grid")
                .next()
                .unwrap_or(&file_name)
                .replace(['-', '_'], " ");
            out.push(serde_json::json!({
                "name": name,
                "path": f.path().to_string_lossy(),
                "pack": pack_name,
                "inputSpace": if is_flog2c { "flog2c" } else { "display" },
            }));
        }
    }
    Ok(out)
}

/// Samples the displayed photo's color at image coordinates (5x5 mean),
/// backend-side — the frontend preview is stale or empty under the WGPU
/// renderer, which made preview-based eyedroppers silently sample garbage.
/// Returns (hue_deg, saturation, value).
#[tauri::command]
pub async fn sample_image_color(
    x: f64,
    y: f64,
    js_adjustments: serde_json::Value,
    state: tauri::State<'_, AppState>,
) -> Result<(f32, f32, f32), String> {
    let img = get_cached_full_warped_image(&state, &js_adjustments)?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("No image loaded".into());
    }
    let cx = (x.round() as i64).clamp(0, w as i64 - 1) as u32;
    let cy = (y.round() as i64).clamp(0, h as i64 - 1) as u32;
    let (mut r, mut g, mut b, mut n) = (0f64, 0f64, 0f64, 0f64);
    for dy in -2i64..=2 {
        for dx in -2i64..=2 {
            let sx = (cx as i64 + dx).clamp(0, w as i64 - 1) as u32;
            let sy = (cy as i64 + dy).clamp(0, h as i64 - 1) as u32;
            let p = img.get_pixel(sx, sy);
            r += p[0] as f64;
            g += p[1] as f64;
            b += p[2] as f64;
            n += 1.0;
        }
    }
    let (hh, ss, vv) = crate::mask_generation::rgb_to_hsv_f(
        (r / n / 255.0) as f32,
        (g / n / 255.0) as f32,
        (b / n / 255.0) as f32,
    );
    Ok((hh, ss, vv))
}

/// A connected blob of mask pixels with its bounding box.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MaskComponent {
    pub id: u32,
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
    pub area: u32,
}

impl MaskComponent {
    pub fn span(&self) -> u32 {
        (self.max_x - self.min_x + 1).max(self.max_y - self.min_y + 1)
    }
}

/// Labels 8-connected components of mask pixels above `threshold`.
/// Returns the label map (0 = background, component ids start at 1) and
/// the component list.
pub(crate) fn mask_components(mask: &GrayImage, threshold: u8) -> (Vec<u32>, Vec<MaskComponent>) {
    let (w, h) = mask.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    let mut labels = vec![0u32; wu * hu];
    let mut comps: Vec<MaskComponent> = Vec::new();
    let mut stack: Vec<(u32, u32)> = Vec::new();
    let raw = mask.as_raw();

    for sy in 0..h {
        for sx in 0..w {
            let idx = sy as usize * wu + sx as usize;
            if raw[idx] <= threshold || labels[idx] != 0 {
                continue;
            }
            let id = comps.len() as u32 + 1;
            let mut comp = MaskComponent {
                id,
                min_x: sx,
                min_y: sy,
                max_x: sx,
                max_y: sy,
                area: 0,
            };
            labels[idx] = id;
            stack.push((sx, sy));
            while let Some((x, y)) = stack.pop() {
                comp.area += 1;
                comp.min_x = comp.min_x.min(x);
                comp.min_y = comp.min_y.min(y);
                comp.max_x = comp.max_x.max(x);
                comp.max_y = comp.max_y.max(y);
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                            continue;
                        }
                        let nidx = ny as usize * wu + nx as usize;
                        if raw[nidx] > threshold && labels[nidx] == 0 {
                            labels[nidx] = id;
                            stack.push((nx as u32, ny as u32));
                        }
                    }
                }
            }
            comps.push(comp);
        }
    }
    (labels, comps)
}

const ENGINE_SPOT_SPAN: u32 = 96;
const MIN_SOLID_DENSITY: f32 = 0.35;
const MAX_DIFFUSION_BLOBS: usize = 6;
const MAX_RECONSTRUCT_DIFFUSION_BLOBS: usize = 10;
const RECONSTRUCT_MIN_DIFFUSION_AREA: u32 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconstructAutoHint {
    HighlightSky,
    Highlight,
    Shadow,
    Generic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconstructStrategy {
    HighlightSky,
    HighlightSurface,
    ShadowTexture,
    GenericContext,
    PromptedSemantic,
}

impl ReconstructStrategy {
    fn from_prompt_and_stats(
        prompt: &str,
        promptless_reconstruct: bool,
        original_crop: &RgbaImage,
        mask: &GrayImage,
    ) -> Self {
        if !promptless_reconstruct && !prompt.trim().is_empty() {
            return ReconstructStrategy::PromptedSemantic;
        }

        let prompt_lower = prompt.to_ascii_lowercase();
        if prompt_lower.contains("sky") || prompt_lower.contains("cloud") {
            return ReconstructStrategy::HighlightSky;
        }
        if prompt_lower.contains("shadow") || prompt_lower.contains("dark") {
            return ReconstructStrategy::ShadowTexture;
        }
        if prompt_lower.contains("highlight") || prompt_lower.contains("overexposed") {
            return ReconstructStrategy::HighlightSurface;
        }

        let Some(stats) = reconstruct_region_stats(original_crop, mask) else {
            return ReconstructStrategy::GenericContext;
        };
        if stats.mean_luma <= 48.0 {
            ReconstructStrategy::ShadowTexture
        } else if stats.mean_luma >= 210.0 {
            ReconstructStrategy::HighlightSurface
        } else {
            ReconstructStrategy::GenericContext
        }
    }

    fn is_sky_like(self) -> bool {
        matches!(self, ReconstructStrategy::HighlightSky)
    }

    fn is_highlight(self) -> bool {
        matches!(
            self,
            ReconstructStrategy::HighlightSky | ReconstructStrategy::HighlightSurface
        )
    }
}

/// One row of the per-blob diagnostic table. Every generative fill blob is
/// an INDEPENDENT engine run with its own crop, canvas scale and mask
/// coverage — which is why one region of a selection can follow the prompt
/// while its neighbour returns flat mush. These numbers name the mechanism
/// for each region instead of leaving it to impression.
#[derive(Debug, Clone)]
struct BlobReport {
    index: usize,
    span: u32,
    area: u32,
    density: f32,
    coverage: f32,
    canvas_long_edge: u32,
    blob_px_on_canvas: u32,
    src_std: f64,
    ai_std: f64,
    final_std: f64,
    used_fallback: bool,
}

/// Pre-flight assessment of whether generative fill can succeed on a
/// region, from measurements taken BEFORE the engine runs.
///
/// Measured across three real cases (2026-08-31): success needs a mask
/// compact enough to survive downscaling to the engine canvas AND
/// surroundings that imply the wanted content. A sky gap framed by
/// buildings satisfied both and produced convincing clouds; a blown
/// region ringed by more blown wash continued the flatness; lacy ribs
/// spanning the frame dissolved at canvas resolution and filled flat.
pub(crate) fn fill_warning_for(
    density: f32,
    span: u32,
    blob_px_on_canvas: u32,
    region_mean_luma: f32,
    ring_std: f32,
    has_prompt: bool,
) -> Option<String> {
    if density < 0.30 && span > 1200 {
        return Some(format!(
            "This selection is fine detail spread across the frame ({:.0}% dense over {span}px). \
             Generative fill downscales it for the model, so thin structure will be lost — \
             highlight recovery or the clone tool will preserve it better.",
            density * 100.0
        ));
    }
    if blob_px_on_canvas < 200 {
        return Some(format!(
            "This region renders at only ~{blob_px_on_canvas}px on the model's canvas, \
             too small to carry detail. Zoom the selection or fill it as part of a larger area."
        ));
    }
    if ring_std < 12.0 && region_mean_luma > 200.0 && !has_prompt {
        return Some(
            "The area around this selection is featureless, so a prompt-less fill will simply \
             continue that flatness. Add a prompt describing what belongs here, or use a \
             flat-field profile to recover the real detail."
                .to_string(),
        );
    }
    if ring_std < 8.0 {
        return Some(
            "The surroundings of this selection carry almost no texture for the model to \
             continue, so the fill is likely to come back flat."
                .to_string(),
        );
    }
    None
}

fn component_density(c: &MaskComponent) -> f32 {
    let bbox = ((c.max_x - c.min_x + 1) * (c.max_y - c.min_y + 1)).max(1);
    c.area as f32 / bbox as f32
}

fn component_goes_to_diffusion(c: &MaskComponent, lama_only: bool, reconstruct_fill: bool) -> bool {
    if lama_only {
        return false;
    }
    if reconstruct_fill {
        // Clipped Reconstruct masks are often lacy by nature: the selected
        // pixels are the missing highlight/shadow evidence. Do not treat
        // that lacy shape as a reason to bypass prompt-conditioned fill.
        return c.area >= RECONSTRUCT_MIN_DIFFUSION_AREA || c.span() > ENGINE_SPOT_SPAN;
    }
    c.span() > ENGINE_SPOT_SPAN && component_density(c) >= MIN_SOLID_DENSITY
}

fn patch_uses_clipped_reconstruct(patch: &AiPatchDefinition) -> bool {
    patch
        .sub_masks
        .iter()
        .any(|sm| sm.visible && sm.mask_type == "clipped")
}

fn submask_has_eraser_refine(sm: &SubMask) -> bool {
    sm.parameters
        .get("lines")
        .and_then(Value::as_array)
        .is_some_and(|lines| {
            lines
                .iter()
                .any(|line| line.get("tool").and_then(Value::as_str) == Some("eraser"))
        })
}

fn patch_has_negative_refinement(patch: &AiPatchDefinition) -> bool {
    patch.invert
        || patch.sub_masks.iter().any(|sm| {
            sm.invert
                || matches!(sm.mode, SubMaskMode::Subtractive | SubMaskMode::Intersect)
                || submask_has_eraser_refine(sm)
        })
}

fn consolidate_reconstruct_mask(mask: &mut GrayImage, preserve_negative_refinements: bool) {
    let (_, before) = mask_components(mask, 127);
    if before.is_empty() {
        return;
    }

    let largest_density = before
        .iter()
        .max_by_key(|c| c.area)
        .map(component_density)
        .unwrap_or(1.0);
    let needs_consolidation = before.len() > 12 || largest_density < MIN_SOLID_DENSITY;
    if !needs_consolidation {
        return;
    }

    if preserve_negative_refinements {
        log::info!(
            "[fill] reconstruct mask left unconsolidated to preserve eraser/subtractive refinements ({} regions, max density {:.2})",
            before.len(),
            largest_density
        );
        return;
    }

    let scale = (mask.width().max(mask.height()) as f32 / 2000.0).max(1.0);
    crate::mask_generation::apply_solidify_public(mask, 45.0, scale);
    *mask = ai_processing::round_mask_geometry(mask, 6.0 * scale);

    let (labels, after) = mask_components(mask, 127);
    let min_area = (180.0 * scale * scale).round().max(64.0) as u32;
    let mut drop = vec![false; after.len() + 1];
    for c in &after {
        if c.area < min_area {
            drop[c.id as usize] = true;
        }
    }
    let w = mask.width() as usize;
    for (i, p) in mask.pixels_mut().enumerate() {
        let label = labels[(i / w) * w + (i % w)];
        if label != 0 && drop[label as usize] {
            p[0] = 0;
        }
    }

    let (_, final_components) = mask_components(mask, 127);
    log::info!(
        "[fill] reconstruct mask consolidated: {} regions -> {} regions (solidify + dust drop)",
        before.len(),
        final_components.len()
    );
}

fn materialize_reconstruct_mask(mask: &mut GrayImage) -> (usize, usize) {
    let mut selected = 0usize;
    for p in mask.pixels_mut() {
        if p[0] > 0 {
            selected += 1;
            p[0] = 255;
        }
    }
    let strong = mask.pixels().filter(|p| p[0] > 127).count();
    (selected, strong)
}

fn reconstruct_composite_mask(mask: &GrayImage, preserve_negative_refinements: bool) -> GrayImage {
    let base = if preserve_negative_refinements {
        mask.clone()
    } else {
        let radius = (mask.width().max(mask.height()) / 500).clamp(8, 18);
        let expanded = dilate_mask(mask, radius);
        ai_processing::round_mask_geometry(&expanded, (radius as f32 / 2.0).max(4.0))
    };
    ai_processing::feather_mask_inward(
        &base,
        if preserve_negative_refinements {
            45.0
        } else {
            70.0
        },
    )
}

fn infer_reconstruct_auto_hint(
    source_image: &DynamicImage,
    mask: &GrayImage,
) -> ReconstructAutoHint {
    let source = source_image.to_rgba8();
    infer_reconstruct_auto_hint_rgba(&source, mask)
}

fn infer_reconstruct_auto_hint_rgba(source: &RgbaImage, mask: &GrayImage) -> ReconstructAutoHint {
    if source.dimensions() != mask.dimensions() {
        return ReconstructAutoHint::Generic;
    }

    let (w, h) = source.dimensions();
    let mut count = 0u64;
    let mut luma_sum = 0.0f64;
    let mut y_sum = 0u64;
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;

    for (x, y, m) in mask.enumerate_pixels() {
        if m[0] <= 127 {
            continue;
        }
        let p = source.get_pixel(x, y);
        luma_sum += 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
        y_sum += y as u64;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        count += 1;
    }

    if count < 64 {
        return ReconstructAutoHint::Generic;
    }

    let mean_luma = luma_sum / count as f64;
    if mean_luma <= 42.0 {
        return ReconstructAutoHint::Shadow;
    }
    if mean_luma < 215.0 {
        return ReconstructAutoHint::Generic;
    }

    let centroid_y = y_sum as f64 / count as f64 / h.max(1) as f64;
    let selected_ratio = count as f64 / (w as f64 * h as f64).max(1.0);
    let upper_frame = centroid_y < 0.72 || min_y < h / 2;

    let margin = (w.max(h) / 32).clamp(96, 360);
    let x0 = min_x.saturating_sub(margin);
    let y0 = min_y.saturating_sub(margin);
    let x1 = (max_x + margin).min(w.saturating_sub(1));
    let y1 = (max_y + margin).min(h.saturating_sub(1));
    let stride = (w.max(h) / 1200).max(1) as usize;
    let mut context_count = 0u64;
    let mut context_sum = [0.0f64; 3];

    for y in (y0..=y1).step_by(stride) {
        for x in (x0..=x1).step_by(stride) {
            if mask.get_pixel(x, y)[0] > 127 {
                continue;
            }
            let p = source.get_pixel(x, y);
            for c in 0..3 {
                context_sum[c] += p[c] as f64;
            }
            context_count += 1;
        }
    }

    let sky_like_context = if context_count < 32 {
        upper_frame
    } else {
        let r = context_sum[0] / context_count as f64;
        let g = context_sum[1] / context_count as f64;
        let b = context_sum[2] / context_count as f64;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        luma > 115.0 && b + g * 0.35 >= r * 1.02
    };

    if upper_frame && selected_ratio >= 0.002 && sky_like_context {
        ReconstructAutoHint::HighlightSky
    } else {
        ReconstructAutoHint::Highlight
    }
}

fn effective_reconstruct_prompt<'a>(
    user_prompt: &'a str,
    reconstruct_fill: bool,
    auto_hint: ReconstructAutoHint,
) -> Cow<'a, str> {
    if !reconstruct_fill || !user_prompt.trim().is_empty() {
        return Cow::Borrowed(user_prompt);
    }
    let prompt = match auto_hint {
        ReconstructAutoHint::HighlightSky => {
            "bright backlit cloud detail and pale sky continuing naturally from the surrounding photograph, realistic soft cloud texture, subtle atmospheric haze, match the existing lighting, exposure, color, lens softness, perspective, and camera grain, seamless edge, no flat gray patch, no beige patch, no text, no borders"
        }
        ReconstructAutoHint::Highlight => {
            "plausible recovered highlight detail continuing naturally from the surrounding photograph, realistic texture in the overexposed area, match the existing scene, lighting, exposure, color, lens softness, perspective, and camera grain, seamless edge, no new objects, no flat gray patch, no beige patch, no text, no borders"
        }
        ReconstructAutoHint::Shadow => {
            "plausible recovered shadow detail continuing naturally from the surrounding photograph, realistic dark texture in the underexposed area, preserve the existing low light, color, lens softness, perspective, and camera grain, seamless edge, no new objects, no flat gray patch, no text, no borders"
        }
        ReconstructAutoHint::Generic => {
            "seamlessly continue the surrounding photograph into the selected overexposed or underexposed area, realistic natural background texture, match the existing scene, lighting, exposure, color, camera grain, lens softness, and perspective, no new objects, no text, no borders, no flat gray patch"
        }
    };
    Cow::Borrowed(prompt)
}

fn reconstruct_tone_strength(reconstruct_fill: bool, promptless_reconstruct: bool) -> f32 {
    if promptless_reconstruct {
        0.35
    } else if reconstruct_fill {
        0.0
    } else {
        0.15
    }
}

fn coarse_rotate_rgba(image: DynamicImage, orientation_steps: u8) -> DynamicImage {
    match orientation_steps % 4 {
        1 => image.rotate90(),
        2 => image.rotate180(),
        3 => image.rotate270(),
        _ => image,
    }
}

fn inverse_coarse_rotate_rgba(image: DynamicImage, orientation_steps: u8) -> DynamicImage {
    match orientation_steps % 4 {
        1 => image.rotate270(),
        2 => image.rotate180(),
        3 => image.rotate90(),
        _ => image,
    }
}

fn orient_rgba_for_engine(
    image: &RgbaImage,
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
) -> RgbaImage {
    let rotated = coarse_rotate_rgba(DynamicImage::ImageRgba8(image.clone()), orientation_steps);
    apply_flip(Cow::Owned(rotated), flip_horizontal, flip_vertical)
        .into_owned()
        .to_rgba8()
}

fn orient_gray_for_engine(
    image: &GrayImage,
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
) -> GrayImage {
    let rotated = coarse_rotate_rgba(DynamicImage::ImageLuma8(image.clone()), orientation_steps);
    apply_flip(Cow::Owned(rotated), flip_horizontal, flip_vertical)
        .into_owned()
        .to_luma8()
}

fn deorient_rgba_from_engine(
    image: &RgbaImage,
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
) -> RgbaImage {
    let unflipped = apply_flip(
        Cow::Owned(DynamicImage::ImageRgba8(image.clone())),
        flip_horizontal,
        flip_vertical,
    )
    .into_owned();
    inverse_coarse_rotate_rgba(unflipped, orientation_steps).to_rgba8()
}

fn engine_orientation_active(
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
) -> bool {
    orientation_steps % 4 != 0 || flip_horizontal || flip_vertical
}

fn ai_fill_orientation_from_adjustments(adjustments: &Value) -> (u8, bool, bool) {
    let orientation_steps = adjustments
        .get("orientationSteps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u8;
    let flip_horizontal = adjustments
        .get("flipHorizontal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let flip_vertical = adjustments
        .get("flipVertical")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    (orientation_steps % 4, flip_horizontal, flip_vertical)
}

fn prefill_reconstruct_conditioning(crop_img: &mut RgbaImage, engine_mask: &GrayImage) -> usize {
    let (w, h) = crop_img.dimensions();
    if engine_mask.dimensions() != (w, h) {
        return 0;
    }

    let ring = dilate_mask(engine_mask, 24);
    let mut bbox = (w, h, 0u32, 0u32);
    let mut masked = 0usize;

    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] > 127 {
                bbox.0 = bbox.0.min(x);
                bbox.1 = bbox.1.min(y);
                bbox.2 = bbox.2.max(x);
                bbox.3 = bbox.3.max(y);
                masked += 1;
            }
        }
    }
    if masked == 0 {
        return 0;
    }

    #[derive(Clone, Copy)]
    struct Accum {
        sum: [f32; 3],
        count: f32,
    }

    impl Accum {
        fn add(&mut self, p: &Rgba<u8>) {
            self.sum[0] += p[0] as f32;
            self.sum[1] += p[1] as f32;
            self.sum[2] += p[2] as f32;
            self.count += 1.0;
        }

        fn mean_or(&self, fallback: [f32; 3]) -> [f32; 3] {
            if self.count > 0.0 {
                [
                    self.sum[0] / self.count,
                    self.sum[1] / self.count,
                    self.sum[2] / self.count,
                ]
            } else {
                fallback
            }
        }
    }

    let mut all = Accum {
        sum: [0.0; 3],
        count: 0.0,
    };
    let mut top = all;
    let mut bottom = all;
    let mut left = all;
    let mut right = all;
    let center_x = (bbox.0 + bbox.2) as f32 * 0.5;
    let center_y = (bbox.1 + bbox.3) as f32 * 0.5;

    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] > 127 || ring.get_pixel(x, y)[0] <= 127 {
                continue;
            }
            let p = crop_img.get_pixel(x, y);
            all.add(p);
            if (y as f32) <= center_y {
                top.add(p);
            } else {
                bottom.add(p);
            }
            if (x as f32) <= center_x {
                left.add(p);
            } else {
                right.add(p);
            }
        }
    }

    let all_mean = all.mean_or([128.0, 128.0, 128.0]);
    let top_mean = top.mean_or(all_mean);
    let bottom_mean = bottom.mean_or(all_mean);
    let left_mean = left.mean_or(all_mean);
    let right_mean = right.mean_or(all_mean);
    let lerp = |a: [f32; 3], b: [f32; 3], t: f32| -> [f32; 3] {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    let bbox_w = (bbox.2.saturating_sub(bbox.0)).max(1) as f32;
    let bbox_h = (bbox.3.saturating_sub(bbox.1)).max(1) as f32;
    let mut filled = 0usize;
    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] <= 127 {
                continue;
            }
            let tx = ((x.saturating_sub(bbox.0)) as f32 / bbox_w).clamp(0.0, 1.0);
            let ty = ((y.saturating_sub(bbox.1)) as f32 / bbox_h).clamp(0.0, 1.0);
            let horizontal = lerp(left_mean, right_mean, tx);
            let vertical = lerp(top_mean, bottom_mean, ty);
            let mut color = [
                vertical[0] * 0.55 + horizontal[0] * 0.35 + all_mean[0] * 0.10,
                vertical[1] * 0.55 + horizontal[1] * 0.35 + all_mean[1] * 0.10,
                vertical[2] * 0.55 + horizontal[2] * 0.35 + all_mean[2] * 0.10,
            ];
            let grain = crate::enhancement::grain_noise(y.saturating_mul(w).saturating_add(x));
            for channel in &mut color {
                *channel = (*channel + grain * 5.0).clamp(0.0, 255.0);
            }
            crop_img.put_pixel(
                x,
                y,
                Rgba([
                    color[0].round() as u8,
                    color[1].round() as u8,
                    color[2].round() as u8,
                    255,
                ]),
            );
            filled += 1;
        }
    }

    for _ in 0..4 {
        let prev = crop_img.clone();
        for y in 0..h {
            for x in 0..w {
                if engine_mask.get_pixel(x, y)[0] <= 127 {
                    continue;
                }
                let mut sum = [0u32; 4];
                let mut count = 0u32;
                for yy in y.saturating_sub(1)..=(y + 1).min(h.saturating_sub(1)) {
                    for xx in x.saturating_sub(1)..=(x + 1).min(w.saturating_sub(1)) {
                        let p = prev.get_pixel(xx, yy);
                        for c in 0..4 {
                            sum[c] += p[c] as u32;
                        }
                        count += 1;
                    }
                }
                crop_img.put_pixel(
                    x,
                    y,
                    Rgba([
                        (sum[0] / count) as u8,
                        (sum[1] / count) as u8,
                        (sum[2] / count) as u8,
                        255,
                    ]),
                );
            }
        }
    }

    filled
}

fn seed_reconstruct_engine_canvas(
    crop_img: &mut RgbaImage,
    engine_mask: &GrayImage,
    prompt: &str,
) -> usize {
    let (w, h) = crop_img.dimensions();
    if engine_mask.dimensions() != (w, h) {
        return 0;
    }

    let mut bbox = (w, h, 0u32, 0u32);
    let mut masked = 0usize;
    let mut ring_sum = [0.0f32; 3];
    let mut ring_count = 0.0f32;
    let ring = dilate_mask(engine_mask, 56);

    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] > 127 {
                bbox.0 = bbox.0.min(x);
                bbox.1 = bbox.1.min(y);
                bbox.2 = bbox.2.max(x);
                bbox.3 = bbox.3.max(y);
                masked += 1;
            } else if ring.get_pixel(x, y)[0] > 127 {
                let p = crop_img.get_pixel(x, y);
                ring_sum[0] += p[0] as f32;
                ring_sum[1] += p[1] as f32;
                ring_sum[2] += p[2] as f32;
                ring_count += 1.0;
            }
        }
    }
    if masked == 0 {
        return 0;
    }

    let context = if ring_count > 0.0 {
        [
            ring_sum[0] / ring_count,
            ring_sum[1] / ring_count,
            ring_sum[2] / ring_count,
        ]
    } else {
        [160.0, 175.0, 185.0]
    };

    let prompt_lower = prompt.to_ascii_lowercase();
    let sky_requested = prompt_lower.contains("sky")
        || prompt_lower.contains("cloud")
        || prompt_lower.contains("bright")
        || prompt_lower.contains("backlit");
    let blue_requested = prompt_lower.contains("blue");
    let prompt_seed = if blue_requested && sky_requested {
        [95.0, 165.0, 225.0]
    } else if sky_requested {
        [175.0, 205.0, 220.0]
    } else {
        context
    };

    let bbox_w = (bbox.2.saturating_sub(bbox.0)).max(1) as f32;
    let bbox_h = (bbox.3.saturating_sub(bbox.1)).max(1) as f32;
    let prompt_weight = if sky_requested { 0.78 } else { 0.35 };
    let mut changed = 0usize;

    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] <= 127 {
                continue;
            }
            let tx = ((x.saturating_sub(bbox.0)) as f32 / bbox_w).clamp(0.0, 1.0);
            let ty = ((y.saturating_sub(bbox.1)) as f32 / bbox_h).clamp(0.0, 1.0);
            let shade = 1.05 - ty * 0.18 + (tx - 0.5).abs() * 0.05;
            let grain = crate::enhancement::grain_noise(y.saturating_mul(w).saturating_add(x));
            let mut color = [0u8; 3];
            for c in 0..3 {
                let base = context[c] * (1.0 - prompt_weight) + prompt_seed[c] * prompt_weight;
                color[c] = (base * shade + grain * 4.0).clamp(0.0, 255.0).round() as u8;
            }
            crop_img.put_pixel(x, y, Rgba([color[0], color[1], color[2], 255]));
            changed += 1;
        }
    }

    changed
}

fn should_seed_reconstruct_engine_canvas(
    component_span: u32,
    strategy: ReconstructStrategy,
    blob_kind: crate::comfy_engine::FillKind,
) -> bool {
    if blob_kind == crate::comfy_engine::FillKind::Flux
        && component_span >= 220
        && matches!(
            strategy,
            ReconstructStrategy::HighlightSky | ReconstructStrategy::PromptedSemantic
        )
    {
        return false;
    }
    true
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn soft_reconstruct_noise(x: u32, y: u32, w: u32, h: u32) -> f32 {
    let sx = x as f32 / w.max(1) as f32;
    let sy = y as f32 / h.max(1) as f32;
    let n1 = (sx * 17.13 + sy * 9.71).sin();
    let n2 = (sx * 37.91 - sy * 21.47 + 1.7).sin();
    let n3 = ((sx + sy) * 61.37 + (sx - sy) * 11.3).sin();
    (n1 * 0.52 + n2 * 0.31 + n3 * 0.17).clamp(-1.0, 1.0)
}

struct ReconstructTextureField {
    width: usize,
    height: usize,
    energy: f32,
    values: Vec<[f32; 3]>,
}

impl ReconstructTextureField {
    fn sample(&self, x: u32, y: u32, image_w: u32, image_h: u32) -> [f32; 3] {
        let gx = (x as f32 / image_w.max(1) as f32 * (self.width.saturating_sub(1)) as f32)
            .clamp(0.0, self.width.saturating_sub(1) as f32);
        let gy = (y as f32 / image_h.max(1) as f32 * (self.height.saturating_sub(1)) as f32)
            .clamp(0.0, self.height.saturating_sub(1) as f32);
        let x0 = gx.floor() as usize;
        let y0 = gy.floor() as usize;
        let x1 = (x0 + 1).min(self.width.saturating_sub(1));
        let y1 = (y0 + 1).min(self.height.saturating_sub(1));
        let tx = gx - x0 as f32;
        let ty = gy - y0 as f32;
        let idx = |xx: usize, yy: usize| yy * self.width + xx;
        let mut out = [0.0; 3];
        for c in 0..3 {
            let a = self.values[idx(x0, y0)][c] * (1.0 - tx) + self.values[idx(x1, y0)][c] * tx;
            let b = self.values[idx(x0, y1)][c] * (1.0 - tx) + self.values[idx(x1, y1)][c] * tx;
            out[c] = a * (1.0 - ty) + b * ty;
        }
        out
    }
}

fn reconstruct_texture_candidate(
    p: &Rgba<u8>,
    context: [f32; 3],
    strategy: ReconstructStrategy,
) -> Option<[f32; 3]> {
    let rgb = [p[0] as f32, p[1] as f32, p[2] as f32];
    let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    let max_c = rgb[0].max(rgb[1]).max(rgb[2]);
    let min_c = rgb[0].min(rgb[1]).min(rgb[2]);
    let sat = if max_c > 1.0 {
        (max_c - min_c) / max_c
    } else {
        0.0
    };

    let eligible = match strategy {
        ReconstructStrategy::HighlightSky => {
            luma > 135.0 && sat < 0.42 && rgb[2] + rgb[1] * 0.25 >= rgb[0] * 0.92
        }
        ReconstructStrategy::HighlightSurface => luma > 120.0 && sat < 0.55,
        ReconstructStrategy::ShadowTexture => luma < 110.0,
        ReconstructStrategy::GenericContext | ReconstructStrategy::PromptedSemantic => true,
    };
    if !eligible {
        return None;
    }

    Some([
        (rgb[0] - context[0]).clamp(-42.0, 42.0),
        (rgb[1] - context[1]).clamp(-42.0, 42.0),
        (rgb[2] - context[2]).clamp(-42.0, 42.0),
    ])
}

fn build_reconstruct_texture_field(
    seeded_crop: &RgbaImage,
    engine_mask: &GrayImage,
    context: [f32; 3],
    strategy: ReconstructStrategy,
) -> Option<ReconstructTextureField> {
    let (w, h) = seeded_crop.dimensions();
    if engine_mask.dimensions() != (w, h) {
        return None;
    }

    let grid_w = ((w / 28).clamp(24, 128)) as usize;
    let grid_h = ((h / 28).clamp(24, 128)) as usize;
    let len = grid_w * grid_h;
    let mut values = vec![[0.0f32; 3]; len];
    let mut known = vec![false; len];
    let mut known_count = 0usize;
    let mut luma_delta_sum = 0.0f32;
    let mut luma_delta_sq_sum = 0.0f32;

    for gy in 0..grid_h {
        for gx in 0..grid_w {
            let x0 = (gx as u32 * w) / grid_w as u32;
            let y0 = (gy as u32 * h) / grid_h as u32;
            let x1 = (((gx + 1) as u32 * w) / grid_w as u32).min(w);
            let y1 = (((gy + 1) as u32 * h) / grid_h as u32).min(h);
            let step = ((x1.saturating_sub(x0)).max(y1.saturating_sub(y0)) / 6).max(1) as usize;
            let mut sum = [0.0f32; 3];
            let mut count = 0.0f32;
            for y in (y0..y1).step_by(step) {
                for x in (x0..x1).step_by(step) {
                    if engine_mask.get_pixel(x, y)[0] > 127 {
                        continue;
                    }
                    if let Some(delta) = reconstruct_texture_candidate(
                        seeded_crop.get_pixel(x, y),
                        context,
                        strategy,
                    ) {
                        for c in 0..3 {
                            sum[c] += delta[c];
                        }
                        count += 1.0;
                    }
                }
            }
            if count > 0.0 {
                let idx = gy * grid_w + gx;
                for c in 0..3 {
                    values[idx][c] = sum[c] / count;
                }
                let luma_delta =
                    0.2126 * values[idx][0] + 0.7152 * values[idx][1] + 0.0722 * values[idx][2];
                luma_delta_sum += luma_delta;
                luma_delta_sq_sum += luma_delta * luma_delta;
                known[idx] = true;
                known_count += 1;
            }
        }
    }

    if known_count < 8 {
        return None;
    }

    let mut current = values;
    for _ in 0..96 {
        let prev = current.clone();
        for gy in 0..grid_h {
            for gx in 0..grid_w {
                let idx = gy * grid_w + gx;
                if known[idx] {
                    continue;
                }
                let mut sum = [0.0f32; 3];
                let mut weight = 0.0f32;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = gx as i32 + dx;
                        let ny = gy as i32 + dy;
                        if nx < 0 || ny < 0 || nx >= grid_w as i32 || ny >= grid_h as i32 {
                            continue;
                        }
                        let nidx = ny as usize * grid_w + nx as usize;
                        let wgt = if dx == 0 || dy == 0 { 1.0 } else { 0.62 };
                        for c in 0..3 {
                            sum[c] += prev[nidx][c] * wgt;
                        }
                        weight += wgt;
                    }
                }
                if weight > 0.0 {
                    for c in 0..3 {
                        current[idx][c] = sum[c] / weight * 0.996;
                    }
                }
            }
        }
    }

    let mean_luma_delta = luma_delta_sum / known_count as f32;
    let energy =
        (luma_delta_sq_sum / known_count as f32 - mean_luma_delta * mean_luma_delta).sqrt();

    Some(ReconstructTextureField {
        width: grid_w,
        height: grid_h,
        energy: energy.clamp(2.0, 24.0),
        values: current,
    })
}

fn build_reconstruct_row_texture(
    seeded_crop: &RgbaImage,
    engine_mask: &GrayImage,
    context: [f32; 3],
    strategy: ReconstructStrategy,
) -> Option<Vec<[f32; 3]>> {
    let (w, h) = seeded_crop.dimensions();
    if engine_mask.dimensions() != (w, h) {
        return None;
    }

    let mut rows = vec![[0.0f32; 3]; h as usize];
    let mut known = vec![false; h as usize];
    let mut known_count = 0usize;
    for y in 0..h {
        let mut sum = [0.0f32; 3];
        let mut count = 0.0f32;
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] > 127 {
                continue;
            }
            if let Some(delta) =
                reconstruct_texture_candidate(seeded_crop.get_pixel(x, y), context, strategy)
            {
                for c in 0..3 {
                    sum[c] += delta[c];
                }
                count += 1.0;
            }
        }
        if count >= 3.0 {
            for c in 0..3 {
                rows[y as usize][c] = sum[c] / count;
            }
            known[y as usize] = true;
            known_count += 1;
        }
    }
    if known_count < 4 {
        return None;
    }

    let mut current = rows;
    for _ in 0..48 {
        let prev = current.clone();
        for y in 0..h as usize {
            if known[y] {
                continue;
            }
            let mut sum = [0.0f32; 3];
            let mut weight = 0.0f32;
            if y > 0 {
                for c in 0..3 {
                    sum[c] += prev[y - 1][c];
                }
                weight += 1.0;
            }
            if y + 1 < h as usize {
                for c in 0..3 {
                    sum[c] += prev[y + 1][c];
                }
                weight += 1.0;
            }
            if weight > 0.0 {
                for c in 0..3 {
                    current[y][c] = sum[c] / weight * 0.998;
                }
            }
        }
    }

    Some(current)
}

fn render_reconstruct_fallback_crop(
    seeded_crop: &RgbaImage,
    engine_mask: &GrayImage,
    prompt: &str,
    strategy: ReconstructStrategy,
) -> RgbaImage {
    let (w, h) = seeded_crop.dimensions();
    if engine_mask.dimensions() != (w, h) {
        return seeded_crop.clone();
    }

    #[derive(Clone, Copy)]
    struct Accum {
        sum: [f32; 3],
        count: f32,
    }
    impl Accum {
        fn add(&mut self, p: &Rgba<u8>) {
            self.sum[0] += p[0] as f32;
            self.sum[1] += p[1] as f32;
            self.sum[2] += p[2] as f32;
            self.count += 1.0;
        }
        fn mean_or(&self, fallback: [f32; 3]) -> [f32; 3] {
            if self.count > 0.0 {
                [
                    self.sum[0] / self.count,
                    self.sum[1] / self.count,
                    self.sum[2] / self.count,
                ]
            } else {
                fallback
            }
        }
    }

    let ring = dilate_mask(engine_mask, (w.max(h) / 38).clamp(32, 96));
    let mut all = Accum {
        sum: [0.0; 3],
        count: 0.0,
    };
    let mut top = all;
    let mut bottom = all;
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut masked = 0u32;

    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] > 127 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                masked += 1;
            } else if ring.get_pixel(x, y)[0] > 127 {
                let p = seeded_crop.get_pixel(x, y);
                all.add(p);
            }
        }
    }
    if masked == 0 {
        return seeded_crop.clone();
    }

    let center_y = (min_y + max_y) as f32 * 0.5;
    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] > 127 || ring.get_pixel(x, y)[0] <= 127 {
                continue;
            }
            let p = seeded_crop.get_pixel(x, y);
            if y as f32 <= center_y {
                top.add(p);
            } else {
                bottom.add(p);
            }
        }
    }

    let context = all.mean_or([190.0, 205.0, 212.0]);
    let top_mean = top.mean_or(context);
    let bottom_mean = bottom.mean_or(context);
    let blue_requested = reconstruct_prompt_requests_blue_sky(prompt);
    let prompt_lower = prompt.to_ascii_lowercase();
    let prompt_mentions_sky =
        blue_requested || prompt_lower.contains("sky") || prompt_lower.contains("cloud");
    let prompt_color = if blue_requested {
        [112.0, 174.0, 224.0]
    } else if strategy == ReconstructStrategy::HighlightSky || prompt_mentions_sky {
        [205.0, 220.0, 226.0]
    } else if strategy == ReconstructStrategy::HighlightSurface {
        [
            (context[0] * 0.65 + 235.0 * 0.35).clamp(0.0, 255.0),
            (context[1] * 0.65 + 235.0 * 0.35).clamp(0.0, 255.0),
            (context[2] * 0.65 + 232.0 * 0.35).clamp(0.0, 255.0),
        ]
    } else if strategy == ReconstructStrategy::ShadowTexture {
        [
            (context[0] * 0.82).clamp(0.0, 255.0),
            (context[1] * 0.82).clamp(0.0, 255.0),
            (context[2] * 0.82).clamp(0.0, 255.0),
        ]
    } else {
        context
    };
    let prompt_strength = match strategy {
        ReconstructStrategy::PromptedSemantic if blue_requested => 0.78,
        ReconstructStrategy::PromptedSemantic if prompt_mentions_sky => 0.56,
        ReconstructStrategy::PromptedSemantic => 0.32,
        ReconstructStrategy::HighlightSky => 0.22,
        ReconstructStrategy::HighlightSurface => 0.12,
        ReconstructStrategy::ShadowTexture => 0.18,
        ReconstructStrategy::GenericContext => 0.10,
    };
    let texture_field =
        build_reconstruct_texture_field(seeded_crop, engine_mask, context, strategy);
    let row_texture = build_reconstruct_row_texture(seeded_crop, engine_mask, context, strategy);

    let edge_sigma = (w.max(h) as f32 / 55.0).clamp(22.0, 72.0);
    let soft_mask = image::imageops::blur(engine_mask, edge_sigma);
    let bbox_w = (max_x.saturating_sub(min_x)).max(1) as f32;
    let bbox_h = (max_y.saturating_sub(min_y)).max(1) as f32;
    let mut out = seeded_crop.clone();

    for y in 0..h {
        for x in 0..w {
            if engine_mask.get_pixel(x, y)[0] <= 127 {
                continue;
            }
            let tx = ((x.saturating_sub(min_x)) as f32 / bbox_w).clamp(0.0, 1.0);
            let ty = ((y.saturating_sub(min_y)) as f32 / bbox_h).clamp(0.0, 1.0);
            let edge = soft_mask.get_pixel(x, y)[0] as f32 / 255.0;
            let interior = smoothstep(0.30, 0.98, edge);
            let vertical_context = [
                top_mean[0] + (bottom_mean[0] - top_mean[0]) * ty,
                top_mean[1] + (bottom_mean[1] - top_mean[1]) * ty,
                top_mean[2] + (bottom_mean[2] - top_mean[2]) * ty,
            ];
            let seeded = seeded_crop.get_pixel(x, y);
            let seeded_rgb = [seeded[0] as f32, seeded[1] as f32, seeded[2] as f32];
            let n = soft_reconstruct_noise(x, y, w, h);
            let cloud = smoothstep(-0.12, 0.82, n);
            let shade = match strategy {
                ReconstructStrategy::HighlightSky => 1.09 - ty * 0.04 + (tx - 0.5).abs() * 0.015,
                ReconstructStrategy::HighlightSurface => 1.04 - ty * 0.02,
                ReconstructStrategy::ShadowTexture => 0.96 - ty * 0.06 + (tx - 0.5).abs() * 0.025,
                _ => 1.02 - ty * 0.06 + (tx - 0.5).abs() * 0.02,
            };
            let mut color = [0.0f32; 3];
            for c in 0..3 {
                let prompt_mix = vertical_context[c] * (1.0 - prompt_strength)
                    + prompt_color[c] * prompt_strength;
                let interior_base =
                    seeded_rgb[c] * 0.55 + prompt_mix * 0.35 + vertical_context[c] * 0.10;
                color[c] = vertical_context[c] * (1.0 - interior) + interior_base * interior;
            }
            let white_haze = [242.0, 245.0, 243.0];
            let haze_strength = match strategy {
                ReconstructStrategy::HighlightSky => 0.20,
                ReconstructStrategy::HighlightSurface => 0.12,
                ReconstructStrategy::ShadowTexture => 0.0,
                ReconstructStrategy::PromptedSemantic if prompt_mentions_sky => 0.10,
                _ => 0.04,
            };
            for c in 0..3 {
                color[c] = color[c] * (1.0 - cloud * haze_strength)
                    + white_haze[c] * (cloud * haze_strength);
                color[c] *= shade;
            }
            if strategy.is_sky_like() || prompt_mentions_sky {
                color[0] *= if blue_requested { 0.985 } else { 0.995 };
                color[2] = (color[2]
                    + interior
                        * if strategy == ReconstructStrategy::HighlightSky && !blue_requested {
                            1.5
                        } else if blue_requested {
                            8.0
                        } else {
                            3.0
                        })
                .clamp(0.0, 255.0);
            }
            if strategy.is_highlight() {
                let highlight_floor = if strategy == ReconstructStrategy::HighlightSky {
                    [205.0, 216.0, 218.0]
                } else {
                    [210.0, 210.0, 207.0]
                };
                for c in 0..3 {
                    color[c] = color[c].max(highlight_floor[c] * interior * 0.72);
                }
            }
            if let Some(field) = texture_field.as_ref() {
                let texture = field.sample(x, y, w, h);
                let texture_weight = smoothstep(0.40, 1.0, edge)
                    * match strategy {
                        ReconstructStrategy::HighlightSky => 0.95,
                        ReconstructStrategy::HighlightSurface => 0.54,
                        ReconstructStrategy::ShadowTexture => 0.62,
                        ReconstructStrategy::PromptedSemantic => 0.36,
                        ReconstructStrategy::GenericContext => 0.42,
                    };
                let contextual_detail = soft_reconstruct_noise(
                    x.wrapping_mul(3).wrapping_add(19),
                    y.wrapping_mul(2).wrapping_add(31),
                    w,
                    h,
                ) * field.energy
                    * match strategy {
                        ReconstructStrategy::HighlightSky => 0.48,
                        ReconstructStrategy::HighlightSurface => 0.32,
                        ReconstructStrategy::ShadowTexture => 0.34,
                        ReconstructStrategy::PromptedSemantic => 0.24,
                        ReconstructStrategy::GenericContext => 0.26,
                    };
                let luma_delta = 0.2126 * texture[0]
                    + 0.7152 * texture[1]
                    + 0.0722 * texture[2]
                    + contextual_detail;
                for c in 0..3 {
                    color[c] +=
                        texture[c] * texture_weight * 0.54 + luma_delta * texture_weight * 0.68;
                }
            }
            if let Some(rows) = row_texture.as_ref() {
                let texture = rows[y as usize];
                let texture_weight = interior
                    * match strategy {
                        ReconstructStrategy::HighlightSky => 0.42,
                        ReconstructStrategy::HighlightSurface => 0.26,
                        ReconstructStrategy::ShadowTexture => 0.30,
                        ReconstructStrategy::PromptedSemantic => 0.20,
                        ReconstructStrategy::GenericContext => 0.22,
                    };
                let luma_delta = 0.2126 * texture[0] + 0.7152 * texture[1] + 0.0722 * texture[2];
                for c in 0..3 {
                    color[c] +=
                        texture[c] * texture_weight * 0.38 + luma_delta * texture_weight * 0.58;
                }
            }
            let fine = crate::enhancement::grain_noise(y.wrapping_mul(w).wrapping_add(x));
            for c in 0..3 {
                color[c] = (color[c] + n * 5.0 * interior + fine * 2.2).clamp(0.0, 255.0);
            }
            out.put_pixel(
                x,
                y,
                Rgba([
                    color[0].round() as u8,
                    color[1].round() as u8,
                    color[2].round() as u8,
                    255,
                ]),
            );
        }
    }

    out
}

fn reconstruct_prompt_requests_blue_sky(prompt: &str) -> bool {
    let prompt_lower = prompt.to_ascii_lowercase();
    prompt_lower.contains("blue")
        && (prompt_lower.contains("sky")
            || prompt_lower.contains("cloud")
            || prompt_lower.contains("backlit")
            || prompt_lower.contains("bright"))
}

#[derive(Clone, Copy, Debug)]
struct ReconstructRegionStats {
    mean_rgb: [f64; 3],
    mean_luma: f64,
    luma_std: f64,
    mean_sat: f64,
    count: f64,
}

impl ReconstructRegionStats {
    fn blue_minus_red(&self) -> f64 {
        self.mean_rgb[2] - self.mean_rgb[0]
    }
}

fn reconstruct_region_stats(
    crop: &RgbaImage,
    crop_mask: &GrayImage,
) -> Option<ReconstructRegionStats> {
    if crop.dimensions() != crop_mask.dimensions() {
        return None;
    }

    let mut count = 0f64;
    let mut sum = [0.0f64; 3];
    let mut luma_sum = 0.0f64;
    let mut luma_sq_sum = 0.0f64;
    let mut sat_sum = 0.0f64;

    for (x, y, m) in crop_mask.enumerate_pixels() {
        if m[0] <= 127 {
            continue;
        }
        let p = crop.get_pixel(x, y);
        let rgb = [p[0] as f64, p[1] as f64, p[2] as f64];
        for c in 0..3 {
            sum[c] += rgb[c];
        }
        let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        luma_sum += luma;
        luma_sq_sum += luma * luma;
        let max_c = rgb[0].max(rgb[1]).max(rgb[2]);
        let min_c = rgb[0].min(rgb[1]).min(rgb[2]);
        if max_c > 1.0 {
            sat_sum += (max_c - min_c) / max_c;
        }
        count += 1.0;
    }

    if count < 1024.0 {
        return None;
    }

    let mean_luma = luma_sum / count;
    let variance = (luma_sq_sum / count - mean_luma * mean_luma).max(0.0);
    Some(ReconstructRegionStats {
        mean_rgb: [sum[0] / count, sum[1] / count, sum[2] / count],
        mean_luma,
        luma_std: variance.sqrt(),
        mean_sat: sat_sum / count,
        count,
    })
}

fn reconstruct_output_looks_collapsed(
    filled_crop: &RgbaImage,
    crop_mask: &GrayImage,
    prompt: &str,
) -> bool {
    let Some(stats) = reconstruct_region_stats(filled_crop, crop_mask) else {
        return false;
    };

    let flat_gray = stats.luma_std < 10.0 && stats.mean_sat < 0.075;
    let blue_prompt_missed = reconstruct_prompt_requests_blue_sky(prompt)
        && stats.blue_minus_red() < 12.0
        && stats.mean_sat < 0.16;

    flat_gray || blue_prompt_missed
}

fn reconstruct_ai_result_lost_to_fallback(
    ai_crop: &RgbaImage,
    fallback_crop: &RgbaImage,
    crop_mask: &GrayImage,
    prompt: &str,
    strategy: ReconstructStrategy,
) -> bool {
    let (Some(ai), Some(fallback)) = (
        reconstruct_region_stats(ai_crop, crop_mask),
        reconstruct_region_stats(fallback_crop, crop_mask),
    ) else {
        return false;
    };

    let sky_like = strategy.is_sky_like()
        || prompt.to_ascii_lowercase().contains("sky")
        || prompt.to_ascii_lowercase().contains("cloud")
        || prompt.to_ascii_lowercase().contains("backlit")
        || prompt.to_ascii_lowercase().contains("bright");
    let washed_out = ai.mean_luma > 224.0
        && ai.mean_luma > fallback.mean_luma + 14.0
        && ai.mean_sat < fallback.mean_sat * 0.65
        && ai.mean_sat < 0.08;
    let lost_sky_color = sky_like
        && fallback.blue_minus_red() > 12.0
        && ai.blue_minus_red() < fallback.blue_minus_red() - 14.0
        && ai.mean_sat < fallback.mean_sat * 0.72;
    let low_color_large_sky = sky_like
        && ai.count > 20_000.0
        && ai.mean_luma > 218.0
        && ai.mean_sat < 0.045
        && fallback.mean_sat > 0.06;

    let lost_highlight = strategy == ReconstructStrategy::HighlightSurface
        && ai.mean_luma > 228.0
        && ai.mean_sat < fallback.mean_sat * 0.7
        && ai.luma_std < fallback.luma_std * 0.8;

    washed_out || lost_sky_color || low_color_large_sky || lost_highlight
}

fn safe_debug_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "patch".to_string()
    } else {
        out
    }
}

fn ai_fill_debug_dir(app_handle: &tauri::AppHandle, debug_run_id: &str) -> Option<PathBuf> {
    if debug_run_id.is_empty() {
        return None;
    }
    let dir = app_handle
        .path()
        .app_data_dir()
        .ok()?
        .join("ai-fill-debug")
        .join(safe_debug_id(debug_run_id));
    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!(
            "[fill] failed to create debug artifact dir {:?}: {}",
            dir,
            e
        );
        return None;
    }
    Some(dir)
}

fn save_debug_rgba(app_handle: &tauri::AppHandle, debug_run_id: &str, name: &str, img: &RgbaImage) {
    let Some(dir) = ai_fill_debug_dir(app_handle, debug_run_id) else {
        return;
    };
    let path = dir.join(format!("{name}.png"));
    if let Err(e) = DynamicImage::ImageRgba8(img.clone()).save(&path) {
        log::warn!("[fill] failed to save debug artifact {:?}: {}", path, e);
    }
}

fn save_debug_gray(app_handle: &tauri::AppHandle, debug_run_id: &str, name: &str, img: &GrayImage) {
    let Some(dir) = ai_fill_debug_dir(app_handle, debug_run_id) else {
        return;
    };
    let path = dir.join(format!("{name}.png"));
    if let Err(e) = DynamicImage::ImageLuma8(img.clone()).save(&path) {
        log::warn!("[fill] failed to save debug artifact {:?}: {}", path, e);
    }
}

fn save_debug_bytes(app_handle: &tauri::AppHandle, debug_run_id: &str, name: &str, bytes: &[u8]) {
    let Some(dir) = ai_fill_debug_dir(app_handle, debug_run_id) else {
        return;
    };
    let path = dir.join(format!("{name}.png"));
    if let Err(e) = fs::write(&path, bytes) {
        log::warn!("[fill] failed to save debug artifact {:?}: {}", path, e);
    }
}

fn save_debug_json(app_handle: &tauri::AppHandle, debug_run_id: &str, name: &str, value: &Value) {
    let Some(dir) = ai_fill_debug_dir(app_handle, debug_run_id) else {
        return;
    };
    let path = dir.join(format!("{name}.json"));
    let data = match serde_json::to_vec_pretty(value) {
        Ok(data) => data,
        Err(e) => {
            log::warn!("[fill] failed to serialize debug json {:?}: {}", path, e);
            return;
        }
    };
    if let Err(e) = fs::write(&path, data) {
        log::warn!("[fill] failed to save debug json {:?}: {}", path, e);
    }
}

/// Extracts one component's mask values for a crop window: pixels keep
/// their mask value only where the label map says they belong to `comp`.
fn component_crop_mask(
    mask: &GrayImage,
    labels: &[u32],
    comp_id: u32,
    x0: u32,
    y0: u32,
    crop_w: u32,
    crop_h: u32,
) -> GrayImage {
    let w = mask.width() as usize;
    let mut out = GrayImage::new(crop_w, crop_h);
    for y in 0..crop_h {
        for x in 0..crop_w {
            let idx = (y0 + y) as usize * w + (x0 + x) as usize;
            if labels[idx] == comp_id {
                out.put_pixel(x, y, image::Luma([mask.get_pixel(x0 + x, y0 + y)[0]]));
            }
        }
    }
    out
}

fn crop_mask_window(mask: &GrayImage, x0: u32, y0: u32, crop_w: u32, crop_h: u32) -> GrayImage {
    let mut out = GrayImage::new(crop_w, crop_h);
    for y in 0..crop_h {
        for x in 0..crop_w {
            out.put_pixel(x, y, image::Luma([mask.get_pixel(x0 + x, y0 + y)[0]]));
        }
    }
    out
}

/// Median |4-neighbor Laplacian| of luma over pixels where `region` is
/// true — the fill-patch counterpart of the enhance dialog's fine-noise
/// estimator, restricted to a masked region.
fn region_fine_noise(img: &RgbaImage, region: &GrayImage, want_inside: bool) -> f32 {
    let (w, h) = img.dimensions();
    if w < 4 || h < 4 {
        return 0.0;
    }
    let luma = |x: u32, y: u32| {
        let p = img.get_pixel(x, y);
        (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
    };
    let mut vals: Vec<f32> = Vec::new();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let inside = region.get_pixel(x, y)[0] > 127;
            if inside != want_inside {
                continue;
            }
            let lap = luma(x, y)
                - (luma(x - 1, y) + luma(x + 1, y) + luma(x, y - 1) + luma(x, y + 1)) * 0.25;
            vals.push(lap.abs());
        }
    }
    if vals.len() < 32 {
        return 0.0;
    }
    let mid = vals.len() / 2;
    vals.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    vals[mid] * 1.4826 / 1.118
}

/// Mean RGB over pixels where `region` matches `want_inside`.
fn region_mean(img: &RgbaImage, region: &GrayImage, want_inside: bool) -> Option<[f32; 3]> {
    let mut sum = [0f64; 3];
    let mut count = 0u64;
    for (x, y, p) in img.enumerate_pixels() {
        let inside = region.get_pixel(x, y)[0] > 127;
        if inside != want_inside {
            continue;
        }
        for c in 0..3 {
            sum[c] += p[c] as f64;
        }
        count += 1;
    }
    if count < 32 {
        return None;
    }
    Some([
        (sum[0] / count as f64) as f32,
        (sum[1] / count as f64) as f32,
        (sum[2] / count as f64) as f32,
    ])
}

/// Makes a fill patch carry its surroundings' FINISH: aligns the patch's
/// tone to the ring of original pixels just outside the mask, then closes
/// the fine-noise gap with neutral grain. Inpainted content is otherwise
/// smoother and slightly off-tone versus its neighborhood — the eye reads
/// that as a blotch "placed in" even when the content is plausible.
/// `tone_strength` is 1.0 for prompt-less fills (pure removal/reconstruct)
/// and gentle for prompted ones, where the new content is INTENDED to
/// differ from its surroundings.
fn harmonize_patch(
    original_crop: &RgbaImage,
    filled_crop: &mut RgbaImage,
    crop_mask: &GrayImage,
    tone_strength: f32,
) {
    // Ring: just outside the mask. Inner band: just inside its edge.
    let ring_zone = dilate_mask(crop_mask, 16);
    let mut ring = GrayImage::new(crop_mask.width(), crop_mask.height());
    for (x, y, p) in ring_zone.enumerate_pixels() {
        if p[0] > 127 && crop_mask.get_pixel(x, y)[0] <= 127 {
            ring.put_pixel(x, y, image::Luma([255]));
        }
    }

    // Tone: shift the filled area so its mean matches the ring's, capped
    // so a legitimately different fill can't be washed out.
    if tone_strength > 0.0
        && let (Some(ring_mean), Some(fill_mean)) = (
            region_mean(original_crop, &ring, true),
            region_mean(filled_crop, crop_mask, true),
        )
    {
        const MAX_SHIFT: f32 = 14.0;
        let shift: Vec<f32> = (0..3)
            .map(|c| ((ring_mean[c] - fill_mean[c]) * tone_strength).clamp(-MAX_SHIFT, MAX_SHIFT))
            .collect();
        for (x, y, p) in filled_crop.enumerate_pixels_mut() {
            if crop_mask.get_pixel(x, y)[0] > 0 {
                for c in 0..3 {
                    p[c] = (p[c] as f32 + shift[c]).clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    // Grain: measure the surroundings' fine noise on the ORIGINAL pixels
    // and the fill's on the FILLED pixels; add the deficit inside the
    // mask (deterministic hash noise, same generator as the enhance
    // dialog's grain match).
    let sigma_ring = region_fine_noise(original_crop, &ring, true);
    let sigma_fill = region_fine_noise(filled_crop, crop_mask, true);
    let deficit = (sigma_ring * sigma_ring - sigma_fill * sigma_fill)
        .max(0.0)
        .sqrt();
    let sigma_add = deficit.min(0.05);
    if sigma_add > 1e-3 {
        let amplitude = sigma_add / 0.408 * 255.0;
        let w = filled_crop.width();
        for (x, y, p) in filled_crop.enumerate_pixels_mut() {
            if crop_mask.get_pixel(x, y)[0] > 0 {
                let n =
                    crate::enhancement::grain_noise(y.wrapping_mul(w).wrapping_add(x)) * amplitude;
                for c in 0..3 {
                    p[c] = (p[c] as f32 + n).clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// Soft-blends a filled crop back into the full image.
fn blend_patch_into(
    encoded_full: &mut RgbaImage,
    filled_crop: &RgbaImage,
    crop_mask: &GrayImage,
    x0: u32,
    y0: u32,
) {
    // Feather scales with the patch: a fixed 4px on a full-res image is
    // a razor edge that reads as a pasted silhouette with a halo rim.
    let sigma = (crop_mask.width().max(crop_mask.height()) as f32 / 90.0).clamp(4.0, 26.0);
    // Inward-only soft edge: blurring alone also spills the fill OUTSIDE
    // the mask boundary onto untouched image (the halo box).
    let blurred = image::imageops::blur(crop_mask, sigma);
    let mut soft_mask = crop_mask.clone();
    for (dst, src) in soft_mask.pixels_mut().zip(blurred.pixels()) {
        dst[0] = dst[0].min(src[0]);
    }
    for y in 0..filled_crop.height() {
        for x in 0..filled_crop.width() {
            let m = soft_mask.get_pixel(x, y)[0];
            if m > 0 {
                let alpha = m as f32 / 255.0;
                let p = filled_crop.get_pixel(x, y);
                let dst = encoded_full.get_pixel_mut(x0 + x, y0 + y);
                for c in 0..3 {
                    dst[c] = (p[c] as f32 * alpha + dst[c] as f32 * (1.0 - alpha)) as u8;
                }
            }
        }
    }
}

struct EngineInpaintResult {
    image: RgbaImage,
    is_linear: bool,
    active_kind: Option<&'static str>,
}

/// Engine-backed removal/replace: splits the mask into connected blobs and
/// fills each in its own tight patch — small blobs heal via LaMa (ideal
/// for speckle selections), large ones via the generative engine — then
/// composites back, mirroring the LaMa patch contract including the gamma
/// flag for float sources. One whole-mask bounding box would balloon to
/// the entire image for scattered selections (e.g. color keys) and force
/// the model to repaint everything at reduced resolution.
/// Per-channel mean plus luma standard deviation over the selected pixels.
pub(crate) fn tone_stats(
    image: &RgbaImage,
    bounds: (u32, u32, u32, u32),
    select: impl Fn(u32, u32) -> bool,
) -> Option<([f32; 3], f32)> {
    let (x0, y0, x1, y1) = bounds;
    let (mut sum, mut n) = ([0.0f64; 3], 0u64);
    let (mut lsum, mut lsq) = (0.0f64, 0.0f64);
    for y in y0..=y1.min(image.height().saturating_sub(1)) {
        for x in x0..=x1.min(image.width().saturating_sub(1)) {
            if !select(x, y) {
                continue;
            }
            let p = image.get_pixel(x, y);
            let l = (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0;
            for c in 0..3 {
                sum[c] += p[c] as f64;
            }
            lsum += l;
            lsq += l * l;
            n += 1;
        }
    }
    if n < 256 {
        return None;
    }
    let mean = [
        (sum[0] / n as f64) as f32,
        (sum[1] / n as f64) as f32,
        (sum[2] / n as f64) as f32,
    ];
    let lmean = lsum / n as f64;
    let std = ((lsq / n as f64) - lmean * lmean).max(0.0).sqrt() as f32;
    Some((mean, std))
}

/// Moves generated content toward the photograph's tone.
///
/// Per-channel means are matched, which carries both exposure and colour
/// cast; contrast is scaled by a single factor taken from luma, so colour
/// relationships inside the content survive. `strength` runs 0 (leave the
/// generated tone alone) to 1 (go all the way to the target).
///
/// Measured on DSC08212: generated sky sat at mean 109.6 against real sky
/// beside it at 226.3, which is why it read as a hole punched in the photo,
/// and why the seam solver produced a bright halo reconciling a 135-level
/// step across 56px.
pub(crate) fn match_tone(
    generated: &mut RgbaImage,
    bounds: (u32, u32, u32, u32),
    from: ([f32; 3], f32),
    to: ([f32; 3], f32),
    strength: f32,
) {
    let s = strength.clamp(0.0, 1.0);
    if s <= 0.0 {
        return;
    }
    let (from_mean, from_std) = from;
    let (to_mean, to_std) = to;
    if from_std <= 0.001 {
        return;
    }
    let target_std = from_std + (to_std - from_std) * s;
    let gain = (target_std / from_std).clamp(0.15, 4.0);
    let new_mean = [
        from_mean[0] + (to_mean[0] - from_mean[0]) * s,
        from_mean[1] + (to_mean[1] - from_mean[1]) * s,
        from_mean[2] + (to_mean[2] - from_mean[2]) * s,
    ];
    let (x0, y0, x1, y1) = bounds;
    for y in y0..=y1.min(generated.height().saturating_sub(1)) {
        for x in x0..=x1.min(generated.width().saturating_sub(1)) {
            let p = generated.get_pixel_mut(x, y);
            for c in 0..3 {
                let v = (p[c] as f32 - from_mean[c]) * gain + new_mean[c];
                p[c] = v.clamp(0.0, 255.0).round() as u8;
            }
        }
    }
}

/// Generated content placed on a full-size canvas, ready to composite.
struct GeneratedRegion {
    canvas: RgbaImage,
    width: u32,
    height: u32,
    band: f32,
}

/// Generates content for `mask` from the prompt alone, at the mask's own
/// aspect ratio, and places it on a full-size canvas.
///
/// Deliberately does NOT tone-match the result to the surrounding ring
/// first. On the case this exists for, that ring is the blown-out area
/// being replaced — matching to it would wash the generated content back
/// out. The seam blend handles the join; the patch's opacity control is
/// how the user dials it toward the photograph.
#[allow(clippy::too_many_arguments)]
async fn generate_region(
    encoded_full: &RgbaImage,
    mask: &GrayImage,
    prompt: &str,
    kind: crate::comfy_engine::FillKind,
    content_scale: f32,
    match_photo: f32,
    loras: &[crate::comfy_engine::LoraSpec],
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
    app_handle: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
) -> Result<GeneratedRegion, String> {
    let (fw, fh) = encoded_full.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..fh {
        for x in 0..fw {
            if mask.get_pixel(x, y)[0] > 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == u32::MAX {
        return Err("nothing selected".to_string());
    }
    let bw = x1 - x0 + 1;
    let bh = y1 - y0 + 1;

    // Generate at the region's aspect ratio. 1024 is where these models are
    // happiest and is what the ceiling test used.
    //
    // content_scale > 1 makes cloud forms larger by generating MORE sky than
    // the region needs and using only the middle of it. Generating bigger at
    // the same time keeps the crop's own resolution constant, so bigger
    // features do not cost sharpness.
    const GEN_LONG_EDGE: f32 = 1024.0;
    const GEN_MAX_EDGE: f32 = 1536.0;
    let zoom = content_scale.clamp(1.0, 2.0);
    let long_edge = (GEN_LONG_EDGE * zoom).min(GEN_MAX_EDGE);
    let scale = long_edge / bw.max(bh) as f32;
    let gw = (((bw as f32 * scale) as u32).max(256) / 8) * 8;
    let gh = (((bh as f32 * scale) as u32).max(256) / 8) * 8;

    // Random per run so re-rolling gives genuine variants; logged so a
    // result that works can be identified and reproduced.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 20))
        .unwrap_or(42);
    log::info!(
        "[fill] generate mode: {gw}x{gh} from prompt on {kind:?}, seed {seed}, \
         region {bw}x{bh} at ({x0}, {y0})"
    );

    let png = crate::comfy_engine::run_free_generation(
        app_handle,
        state.inner(),
        kind,
        prompt,
        gw,
        gh,
        seed,
        loras,
        |_| {},
    )
    .await
    .map_err(|e| e.to_string())?;

    let decoded = image::load_from_memory(&png).map_err(|e| e.to_string())?;
    let mut generated = decoded.to_rgba8();

    // Use only the middle of the generated frame when zoomed in: what is
    // discarded is sky we did not need, and what remains lands larger.
    if zoom > 1.001 {
        let cw = ((generated.width() as f32 / zoom) as u32).max(64);
        let ch = ((generated.height() as f32 / zoom) as u32).max(64);
        let cx = (generated.width() - cw) / 2;
        let cy = (generated.height() - ch) / 2;
        generated = image::imageops::crop_imm(&generated, cx, cy, cw, ch).to_image();
    }

    let mut fitted =
        image::imageops::resize(&generated, bw, bh, image::imageops::FilterType::Lanczos3);

    // Sit it in the photograph's tone before it is composited, so the seam
    // solver has almost nothing left to reconcile. The reference is the
    // photo just OUTSIDE the selection with clipped pixels excluded — the
    // rim inside is the blowout being replaced, and matching to that would
    // wash the content straight back out.
    let ring = 240u32;
    let rx0 = x0.saturating_sub(ring);
    let ry0 = y0.saturating_sub(ring);
    let rx1 = (x1 + ring).min(fw - 1);
    let ry1 = (y1 + ring).min(fh - 1);
    let reference = tone_stats(encoded_full, (rx0, ry0, rx1, ry1), |x, y| {
        if mask.get_pixel(x, y)[0] > 0 {
            return false;
        }
        // Skip clipped pixels: they carry no tone information.
        let p = encoded_full.get_pixel(x, y);
        let l = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3;
        (60..=250).contains(&l)
    });
    if let (Some(reference), Some(source)) = (
        reference,
        tone_stats(&fitted, (0, 0, bw - 1, bh - 1), |_, _| true),
    ) {
        // Do not match outright: the surrounding sky's own spread is small
        // (13.8 measured), and adopting it would flatten the clouds back to
        // a gradient. Aim a little under the surroundings in level, with
        // more contrast than they have — recovered highlight detail should
        // read slightly darker than the blowout it replaces.
        let target_mean = [
            reference.0[0] * 0.90,
            reference.0[1] * 0.90,
            reference.0[2] * 0.90,
        ];
        let target_std = (reference.1 * 1.8).clamp(18.0, 30.0);
        log::info!(
            "[fill] generate mode: tone match {:.0}% — generated mean {:.0}/std {:.0} -> \
             target {:.0}/std {:.0} (surroundings {:.0}/std {:.0})",
            match_photo * 100.0,
            source.0.iter().sum::<f32>() / 3.0,
            source.1,
            target_mean.iter().sum::<f32>() / 3.0,
            target_std,
            reference.0.iter().sum::<f32>() / 3.0,
            reference.1
        );
        match_tone(
            &mut fitted,
            (0, 0, bw - 1, bh - 1),
            source,
            (target_mean, target_std),
            match_photo,
        );
    } else {
        log::info!("[fill] generate mode: no usable tone reference nearby; leaving tone as generated");
    }

    // Patches composite in ORIGINAL image space, but the user sees the photo
    // after its flips and coarse rotation. Generated content is upright in
    // the frame the model made it in, so without this it lands upside down
    // on a flipped photo — clouds lit from below. Undo the display transform
    // so it reads correctly on screen: display(S) = flip(R^steps(S)), so
    // S = R^(4-steps)(flip(G)). Flips are their own inverse.
    if flip_horizontal {
        image::imageops::flip_horizontal_in_place(&mut fitted);
    }
    if flip_vertical {
        image::imageops::flip_vertical_in_place(&mut fitted);
    }
    let fitted = match orientation_steps % 4 {
        1 => image::imageops::rotate270(&fitted),
        2 => image::imageops::rotate180(&fitted),
        3 => image::imageops::rotate90(&fitted),
        _ => fitted,
    };
    // A quarter turn swaps the axes, so refit to the region.
    let fitted = if fitted.width() != bw || fitted.height() != bh {
        image::imageops::resize(&fitted, bw, bh, image::imageops::FilterType::Lanczos3)
    } else {
        fitted
    };

    let mut canvas = RgbaImage::new(fw, fh);
    image::imageops::replace(&mut canvas, &fitted, x0 as i64, y0 as i64);

    // Fade the seam correction over a small fraction of the region so the
    // join reads as continuous while the interior keeps what was generated.
    let band = (bw.min(bh) as f32 * 0.06).clamp(8.0, 96.0);
    Ok(GeneratedRegion {
        canvas,
        width: bw,
        height: bh,
        band,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_engine_inpaint_patch(
    source_image: &DynamicImage,
    mask: &GrayImage,
    prompt: &str,
    kind: crate::comfy_engine::FillKind,
    lama_only: bool,
    reconstruct_fill: bool,
    promptless_reconstruct: bool,
    reconstruct_single_path: bool,
    generate_mode: bool,
    content_scale: f32,
    match_photo: f32,
    loras: &[crate::comfy_engine::LoraSpec],
    debug_run_id: &str,
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
    app_handle: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
) -> Result<EngineInpaintResult, String> {
    // Linear-source detection must come from the SOURCE, not the pixel
    // format: compositing patches returns float pixels even for JPEGs,
    // and gamma-encoding an already-display-encoded JPEG double-brightens
    // the canvas the model is conditioned on (measured: canvas mean 0.74
    // vs 0.46 true) — the model then paints matching blown-out wash.
    let is_linear = state
        .original_image
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|l| l.is_raw)
        && matches!(
            source_image,
            DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
        );
    let (w, h) = source_image.dimensions();

    // The engine wants display-referred pixels; float/RAW sources are
    // linear, so encode (and return the composite in the same space with
    // the gamma flag, exactly like the LaMa path).
    let mut encoded_full = if is_linear {
        ai_processing::gamma_encode_rgba8(source_image)
    } else {
        source_image.to_rgba8()
    };

    // Generate mode: build the content from the prompt alone and composite
    // it, rather than asking an inpainting model to continue surroundings
    // that carry no information.
    //
    // Measured 2026-09-01: the same flux1-fill-dev Q8 weights, guidance and
    // cfg that return grey mush inside a blown-out selection paint
    // photorealistic cumulus once the mask is opened and there is nothing
    // to continue. The limit was never the model, the quantization, the
    // guidance or the prompt — it was asking a continuation model to invent.
    //
    // No tiling here: the region is made in one pass, which also removes the
    // cascade, the tile seams and the ordering problem, all of which were
    // artefacts of tiling a job the model could not do anyway.
    if generate_mode && !prompt.trim().is_empty() {
        let generated = generate_region(
            &encoded_full,
            mask,
            prompt,
            kind,
            content_scale,
            match_photo,
            loras,
            orientation_steps,
            flip_horizontal,
            flip_vertical,
            app_handle,
            state,
        )
            .await
            .map_err(|e| format!("Generation failed: {e}"))?;
        let band = generated.band;
        save_debug_rgba(app_handle, debug_run_id, "generate-raw", &generated.canvas);
        let blended =
            crate::heal_blend::blend_generated(&encoded_full, &generated.canvas, mask, band);
        log::info!(
            "[fill] generate mode: composited {}x{} generated region, seam band {band:.0}px",
            generated.width,
            generated.height
        );
        return Ok(EngineInpaintResult {
            image: blended,
            is_linear,
            active_kind: Some(match kind {
                crate::comfy_engine::FillKind::Flux => "flux-generate",
                crate::comfy_engine::FillKind::SdxlFooocus => "sdxl-generate",
                crate::comfy_engine::FillKind::SdxlBase => "sdxl-generate",
            }),
        });
    }

    let (labels, mut comps) = mask_components(mask, 127);
    if comps.is_empty() {
        return Ok(EngineInpaintResult {
            image: encoded_full,
            is_linear,
            active_kind: None,
        });
    }
    comps.sort_by_key(|c| std::cmp::Reverse(c.area));
    // Diffusion is for SOLID, object-like regions only. A blob that fills
    // little of its own bounding box is lace (scattered speckle bridged by
    // dilation, typical of color keys) — a diffusion model would repaint
    // the whole dilated region with invented content and read as blotches.
    // LaMa's texture synthesis is the right tool for lace at any size,
    // except for clipped Reconstruct masks where the lacy pixels ARE the
    // missing content and must stay on the prompt-conditioned engine path.
    let max_diffusion_blobs = if reconstruct_fill {
        MAX_RECONSTRUCT_DIFFUSION_BLOBS
    } else {
        MAX_DIFFUSION_BLOBS
    };
    let (large, spots): (Vec<MaskComponent>, Vec<MaskComponent>) = if reconstruct_single_path {
        let mut whole = MaskComponent {
            id: 0,
            min_x: w,
            min_y: h,
            max_x: 0,
            max_y: 0,
            area: 0,
        };
        for c in comps {
            whole.min_x = whole.min_x.min(c.min_x);
            whole.min_y = whole.min_y.min(c.min_y);
            whole.max_x = whole.max_x.max(c.max_x);
            whole.max_y = whole.max_y.max(c.max_y);
            whole.area += c.area;
        }
        (vec![whole], Vec::new())
    } else {
        let (mut large, mut spots): (Vec<MaskComponent>, Vec<MaskComponent>) = comps
            .into_iter()
            .partition(|c| component_goes_to_diffusion(c, lama_only, reconstruct_fill));
        if large.len() > max_diffusion_blobs {
            // Demotion is silent mush: LaMa has no text input, so a
            // prompted region routed here CANNOT follow the prompt.
            let demoted = large.split_off(max_diffusion_blobs);
            for c in &demoted {
                log::info!(
                    "[fill] blob-cap demotion: span={} area={} -> LaMa spot (prompt is IGNORED on that path; cap={})",
                    c.span(),
                    c.area,
                    max_diffusion_blobs
                );
            }
            spots.extend(demoted);
        }
        (large, spots)
    };
    let route = if lama_only {
        "fast-lama"
    } else if reconstruct_fill {
        "reconstruct"
    } else {
        "repair"
    };
    log::info!(
        "[fill] route={route}, prompt={}, auto_prompt={}, engine_orientation=steps:{},flip_h:{},flip_v:{}, mask split into {} diffusion region(s) + {} LaMa region(s)/spot(s)",
        if prompt.trim().is_empty() {
            "empty"
        } else {
            "set"
        },
        promptless_reconstruct,
        orientation_steps % 4,
        flip_horizontal,
        flip_vertical,
        large.len(),
        spots.len(),
    );
    if reconstruct_fill && !prompt.trim().is_empty() {
        log::info!("[fill] reconstruct prompt text: {:?}", prompt.trim());
    }
    if reconstruct_fill {
        log::info!(
            "[fill] reconstruct routing: {} prompt-conditioned component(s), {} fast cleanup spot(s), single_path={}",
            large.len(),
            spots.len(),
            reconstruct_single_path
        );
    }

    // LaMa serves both the spot heals and the SDXL prefill hint.
    let lama_session = match resolve_and_prepare(
        app_handle,
        &state.model_registry,
        TaskType::Inpaint,
        "inpaint",
        |m| m.params.get("engine").is_none(),
    )
    .await
    {
        Ok((registry, lama)) => registry.get_session(&lama.manifest.id, None).ok(),
        Err(_) => None,
    };

    // Spots first, so diffusion patches see healed surroundings.
    for comp in &spots {
        let Some(session) = lama_session.as_ref() else {
            break;
        };
        let span = comp.span();
        let pad = 96.max(span);
        let x0 = comp.min_x.saturating_sub(pad);
        let y0 = comp.min_y.saturating_sub(pad);
        let x1 = (comp.max_x + pad).min(w.saturating_sub(1));
        let y1 = (comp.max_y + pad).min(h.saturating_sub(1));
        let (crop_w, crop_h) = (x1 - x0 + 1, y1 - y0 + 1);

        let crop_img = image::imageops::crop_imm(&encoded_full, x0, y0, crop_w, crop_h).to_image();
        let crop_mask = component_crop_mask(mask, &labels, comp.id, x0, y0, crop_w, crop_h);
        let crop_mask = dilate_mask(&crop_mask, (span / 8).clamp(4, 12));

        if let Ok((mut healed, _)) = ai_processing::run_lama_inpainting(
            &DynamicImage::ImageRgba8(crop_img.clone()),
            &crop_mask,
            session,
        ) {
            // Spot heals are always pure removal: full-strength tone match.
            harmonize_patch(&crop_img, &mut healed, &crop_mask, 1.0);
            blend_patch_into(&mut encoded_full, &healed, &crop_mask, x0, y0);
        }
    }

    let mut reconstruct_raw_ai_full = reconstruct_fill.then(|| encoded_full.clone());
    let mut reconstruct_fallback_full = reconstruct_fill.then(|| encoded_full.clone());
    let mut reconstruct_rejected_any = false;
    let mut blob_reports: Vec<BlobReport> = Vec::new();

    // Size-tiered model routing: tiny blobs already healed via LaMa above;
    // truly large areas auto-escalate to Flux (the strongest fill tier)
    // when its weights are installed, regardless of the selected model —
    // big reconstructions are where workflow quality dominates, and small
    // ones aren't worth Flux's runtime.
    const FLUX_SPAN: u32 = 320;
    let flux_available =
        crate::comfy_engine::fill_files_present(app_handle, crate::comfy_engine::FillKind::Flux);

    // Progressive tiling. Measured 2026-08-31: a large region returns flat
    // wash in one pass because the model continues its surroundings, while
    // small regions reliably invent content. Splitting an oversized blob
    // into overlapping tiles filled IN SEQUENCE — each tile seeing the
    // previous ones as context — turned that same blob from featureless
    // (in-mask structure 4.3 -> 16.5, no shapes) into visible cloud forms
    // (-> 18.4 with structure). Tiles overlap so the feathered composite
    // crossfades instead of leaving seams.
    //
    // Tiles are filled in raster order, and an attempt to do better by
    // ordering them on surrounding texture was measured WORSE and reverted.
    // Same photo, mask, prompt and settings, matching tiles by area:
    //
    //   area     raster order        texture order
    //   92448    5th -> std  8.63    1st -> std 2.78
    //   196442   6th -> std 10.48    2nd -> std 4.94
    //   212709   4th -> std  9.03    3rd -> std 5.24
    //   202070   3rd -> std 15.01    4th -> std 5.56
    //
    // Every tile got worse, and 202070 collapsed despite moving LATER, so
    // neither "more surrounding texture" nor "later is better" explains it.
    // What fits: flat output propagates. Whatever runs first sets the tone,
    // and a flat result becomes flat context for everything behind it. The
    // raster run survived only because its second tile (98582 — the
    // smallest and sparsest, density 0.25) recovered to 10.22 and seeded
    // real structure. If this is revisited, order by what succeeds most
    // reliably alone — small and sparse first — not by surroundings, and
    // prove it on a full six-tile run before shipping.
    const TILE_SPAN_THRESHOLD: u32 = 900;
    const TILE_TARGET: u32 = 700;
    const MIN_TILE_AREA: u32 = 4000;
    let mut units: Vec<FillUnit> = Vec::new();
    for comp in &large {
        if comp.span() <= TILE_SPAN_THRESHOLD {
            units.push((*comp, None));
            continue;
        }
        let bbox_w = comp.max_x - comp.min_x + 1;
        let bbox_h = comp.max_y - comp.min_y + 1;
        let cols = bbox_w.div_ceil(TILE_TARGET).max(1);
        let rows = bbox_h.div_ceil(TILE_TARGET).max(1);
        let step_x = bbox_w.div_ceil(cols);
        let step_y = bbox_h.div_ceil(rows);
        let overlap_x = (step_x / 5).max(24);
        let overlap_y = (step_y / 5).max(24);
        let mut tile_count = 0;
        for row in 0..rows {
            for col in 0..cols {
                let raw_x0 = comp.min_x + col * step_x;
                let raw_y0 = comp.min_y + row * step_y;
                let tx0 = raw_x0.saturating_sub(overlap_x).max(comp.min_x);
                let ty0 = raw_y0.saturating_sub(overlap_y).max(comp.min_y);
                let tx1 = (raw_x0 + step_x - 1 + overlap_x).min(comp.max_x);
                let ty1 = (raw_y0 + step_y - 1 + overlap_y).min(comp.max_y);
                let mut area = 0u32;
                let (mut nx0, mut ny0, mut nx1, mut ny1) = (u32::MAX, u32::MAX, 0u32, 0u32);
                for y in ty0..=ty1 {
                    for x in tx0..=tx1 {
                        if labels[(y * w + x) as usize] == comp.id {
                            area += 1;
                            nx0 = nx0.min(x);
                            ny0 = ny0.min(y);
                            nx1 = nx1.max(x);
                            ny1 = ny1.max(y);
                        }
                    }
                }
                if area < MIN_TILE_AREA {
                    continue;
                }
                units.push((
                    MaskComponent {
                        id: comp.id,
                        min_x: nx0,
                        min_y: ny0,
                        max_x: nx1,
                        max_y: ny1,
                        area,
                    },
                    Some((tx0, ty0, tx1, ty1)),
                ));
                tile_count += 1;
            }
        }
        // Raster order. Ordering these by how much texture surrounds each
        // tile was measured worse on the same photo, mask and prompt — see
        // the note on the tiling constants above.
        log::info!(
            "[fill] blob span {} exceeds {} — filling as {} progressive tile(s)",
            comp.span(),
            TILE_SPAN_THRESHOLD,
            tile_count
        );
    }

    for (blob_index, (comp, tile_rect)) in units.iter().enumerate() {
        let blob_prefix = format!("blob-{blob_index:02}");
        let mut blob_used_fallback = false;
        // The engine the user picked is the engine that runs. Two branches
        // here used to force Flux — one whenever a Reconstruct patch was
        // single-path, one for any blob spanning FLUX_SPAN or more — which
        // left the model picker inert on exactly the selections people care
        // about: a run logged "selected engine model sdxl-fill-fooocus" and
        // then "forcing Flux Fill" on every tile. Flux may still be the
        // better choice on large context-continuation fills, so say so in
        // the log rather than overriding the choice silently.
        if comp.span() >= FLUX_SPAN && flux_available && kind != crate::comfy_engine::FillKind::Flux
        {
            log::info!(
                "[fill] {blob_prefix} span {} ≥ {} — honouring the selected engine; Flux Fill often does better on continuation fills this large",
                comp.span(),
                FLUX_SPAN
            );
        }
        let blob_kind = kind;
        let span_x = comp.max_x - comp.min_x + 1;
        let span_y = comp.max_y - comp.min_y + 1;
        // Reconstruct needs broader scene context than normal object
        // removal. The model is not just deleting a thing; it is guessing
        // plausible lost pixels from the surrounding photograph.
        let (context_scale, context_cap) = if reconstruct_fill {
            (1.9, 840)
        } else {
            (1.5, 520)
        };
        let pad_x = 192
            .max((span_x as f32 * context_scale) as u32)
            .min(context_cap);
        let pad_y = 192
            .max((span_y as f32 * context_scale) as u32)
            .min(context_cap);
        let x0 = comp.min_x.saturating_sub(pad_x);
        let y0 = comp.min_y.saturating_sub(pad_y);
        let x1 = (comp.max_x + pad_x).min(w.saturating_sub(1));
        let y1 = (comp.max_y + pad_y).min(h.saturating_sub(1));
        let (crop_w, crop_h) = (x1 - x0 + 1, y1 - y0 + 1);

        let mut crop_img =
            image::imageops::crop_imm(&encoded_full, x0, y0, crop_w, crop_h).to_image();
        let crop_mask = if reconstruct_single_path {
            crop_mask_window(mask, x0, y0, crop_w, crop_h)
        } else {
            component_crop_mask(mask, &labels, comp.id, x0, y0, crop_w, crop_h)
        };
        // A tile owns only its slice of the component; the neighbouring
        // slices are filled by their own passes.
        let crop_mask = match tile_rect {
            Some((tx0, ty0, tx1, ty1)) => {
                let mut restricted = crop_mask;
                for (x, y, pixel) in restricted.enumerate_pixels_mut() {
                    let gx = x0 + x;
                    let gy = y0 + y;
                    if gx < *tx0 || gx > *tx1 || gy < *ty0 || gy > *ty1 {
                        pixel[0] = 0;
                    }
                }
                restricted
            }
            None => crop_mask,
        };
        save_debug_rgba(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-source-crop"),
            &crop_img,
        );

        let crop_mask = if reconstruct_single_path {
            log::info!(
                "[fill] {blob_prefix} single-path Reconstruct uses hard full-strength mask with no grow/round step"
            );
            crop_mask
        } else {
            // Grow the mask: slivers of the object just outside the selection
            // otherwise stay visible AND anchor the model to repaint the object.
            let grow = (crop_w.max(crop_h) / 60).clamp(12, 32);
            let crop_mask = dilate_mask(&crop_mask, grow);
            // Square dilation reintroduces corners — round to organic curves.
            ai_processing::round_mask_geometry(&crop_mask, (grow as f32 / 2.0).max(6.0))
        };
        // The normal path gives the model a paint margin beyond the final
        // composite line. The single-path test deliberately does not: it
        // proves whether the small-dot conditioning style itself scales.
        let engine_mask = if reconstruct_single_path {
            crop_mask.clone()
        } else {
            dilate_mask(&crop_mask, 16)
        };
        save_debug_gray(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-crop-mask"),
            &crop_mask,
        );
        save_debug_gray(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-engine-mask-fullres"),
            &engine_mask,
        );
        // Ring stats must come from pre-fill pixels (the prefill below
        // rewrites the masked interior).
        let original_crop = crop_img.clone();

        if reconstruct_fill {
            let scrubbed = prefill_reconstruct_conditioning(&mut crop_img, &engine_mask);
            log::info!(
                "[fill] reconstruct conditioning prefill: {blob_prefix}, {scrubbed} masked px scrubbed before engine"
            );
        }

        // The sampler keeps a low-frequency imprint of whatever occupies
        // the masked area, so non-Reconstruct SDXL tiers get a LaMa prefill
        // as a plausible starting hint. Reconstruct uses the deterministic
        // context prefill above so clipped pixels cannot anchor the model.
        if !reconstruct_fill
            && blob_kind != crate::comfy_engine::FillKind::Flux
            && let Some(session) = lama_session.as_ref()
            && let Ok((prefill, _)) = ai_processing::run_lama_inpainting(
                &DynamicImage::ImageRgba8(crop_img.clone()),
                &crop_mask,
                session,
            )
        {
            crop_img = prefill;
        }
        save_debug_rgba(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-conditioning-crop"),
            &crop_img,
        );

        let orient_for_engine = reconstruct_fill
            && engine_orientation_active(orientation_steps, flip_horizontal, flip_vertical);
        let mut engine_crop_img = if orient_for_engine {
            orient_rgba_for_engine(&crop_img, orientation_steps, flip_horizontal, flip_vertical)
        } else {
            crop_img.clone()
        };
        let engine_mask_for_model = if orient_for_engine {
            orient_gray_for_engine(
                &engine_mask,
                orientation_steps,
                flip_horizontal,
                flip_vertical,
            )
        } else {
            engine_mask.clone()
        };
        let reconstruct_strategy = if reconstruct_fill {
            Some(ReconstructStrategy::from_prompt_and_stats(
                prompt,
                promptless_reconstruct,
                &engine_crop_img,
                &engine_mask_for_model,
            ))
        } else {
            None
        };
        if reconstruct_fill {
            let strategy = reconstruct_strategy.unwrap();
            if reconstruct_single_path
                || should_seed_reconstruct_engine_canvas(comp.span(), strategy, blob_kind)
            {
                let seeded = seed_reconstruct_engine_canvas(
                    &mut engine_crop_img,
                    &engine_mask_for_model,
                    prompt,
                );
                log::info!(
                    "[fill] reconstruct engine seed: {blob_prefix}, strategy={strategy:?}, {seeded} masked px seeded from prompt/context before engine"
                );
            } else {
                log::info!(
                    "[fill] reconstruct engine seed skipped: {blob_prefix}, strategy={strategy:?}, span={}, kind={blob_kind:?}; Flux gets scrubbed context + mask instead of a flat placeholder",
                    comp.span()
                );
            }
        }
        if reconstruct_fill {
            save_debug_rgba(
                app_handle,
                debug_run_id,
                &format!("{blob_prefix}-engine-seeded-conditioning-crop"),
                &engine_crop_img,
            );
        }
        let fallback_engine_crop = if reconstruct_fill {
            let fallback = render_reconstruct_fallback_crop(
                &engine_crop_img,
                &engine_mask_for_model,
                prompt,
                reconstruct_strategy.unwrap(),
            );
            save_debug_rgba(
                app_handle,
                debug_run_id,
                &format!("{blob_prefix}-reconstruct-fallback-crop"),
                &fallback,
            );
            Some(fallback)
        } else {
            None
        };
        if orient_for_engine {
            save_debug_rgba(
                app_handle,
                debug_run_id,
                &format!("{blob_prefix}-engine-view-conditioning-crop"),
                &engine_crop_img,
            );
            save_debug_gray(
                app_handle,
                debug_run_id,
                &format!("{blob_prefix}-engine-view-mask-fullres"),
                &engine_mask_for_model,
            );
        }

        // Big reconstructions earn a bigger canvas (Flux handles 1536
        // comfortably on this hardware).
        let canvas_long_edge = if comp.span() >= 900 { 1536 } else { 1216 };
        // Mask coverage of the engine canvas is the prime suspect for
        // "this blob ignored the prompt": an inpainting model with little
        // surrounding context degenerates toward flat low-frequency output.
        let engine_masked_px = engine_mask_for_model
            .pixels()
            .filter(|p| p[0] > 127)
            .count() as u32;
        let engine_crop_px = (engine_mask_for_model.width() * engine_mask_for_model.height()).max(1);
        let blob_coverage = engine_masked_px as f32 / engine_crop_px as f32;
        let canvas_scale = (canvas_long_edge as f32
            / engine_crop_img.width().max(engine_crop_img.height()).max(1) as f32)
            .min(1.0);
        let blob_px_on_canvas = (comp.span() as f32 * canvas_scale) as u32;
        log::info!(
            "[fill] {blob_prefix} geometry: span={} area={} density={:.2} crop={}x{} canvas_long_edge={} mask_coverage={:.1}% blob_on_canvas~{}px",
            comp.span(),
            comp.area,
            component_density(comp),
            crop_w,
            crop_h,
            canvas_long_edge,
            blob_coverage * 100.0,
            blob_px_on_canvas
        );
        // Pre-flight: measure whether this region CAN work before spending
        // minutes on it. Ring texture is the predictor that separated the
        // three known outcomes — a sky gap framed by buildings succeeded,
        // blown wash ringed by blown wash did not.
        {
            let ring_outer = dilate_mask(&crop_mask, 40);
            let ring_inner = dilate_mask(&crop_mask, 8);
            let (mut region_sum, mut region_n) = (0.0f64, 0u32);
            let (mut ring_sum, mut ring_sq, mut ring_n) = (0.0f64, 0.0f64, 0u32);
            for (x, y, p) in original_crop.enumerate_pixels() {
                let luma =
                    0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
                if crop_mask.get_pixel(x, y)[0] > 127 {
                    region_sum += luma;
                    region_n += 1;
                } else if ring_outer.get_pixel(x, y)[0] > 127
                    && ring_inner.get_pixel(x, y)[0] <= 127
                {
                    ring_sum += luma;
                    ring_sq += luma * luma;
                    ring_n += 1;
                }
            }
            if region_n > 0 && ring_n > 32 {
                let region_mean = (region_sum / region_n as f64) as f32;
                let ring_mean = ring_sum / ring_n as f64;
                let ring_std =
                    ((ring_sq / ring_n as f64) - ring_mean * ring_mean).max(0.0).sqrt() as f32;
                log::info!(
                    "[fill] {blob_prefix} pre-flight: region_mean={region_mean:.1} ring_std={ring_std:.1} density={:.2}",
                    component_density(comp)
                );
                if let Some(warning) = fill_warning_for(
                    component_density(comp),
                    comp.span(),
                    blob_px_on_canvas,
                    region_mean,
                    ring_std,
                    !prompt.trim().is_empty(),
                ) {
                    log::warn!("[fill] {blob_prefix} pre-flight warning: {warning}");
                    let _ = tauri::Emitter::emit(app_handle, "fill-warning", warning);
                }
            }
        }
        let (img_png, mask_png, _, _) = crate::expansion::engine_canvas_pngs_sized(
            &engine_crop_img,
            &engine_mask_for_model,
            canvas_long_edge,
        )?;
        save_debug_bytes(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-engine-input"),
            &img_png,
        );
        save_debug_bytes(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-engine-mask"),
            &mask_png,
        );
        let fill_png = crate::comfy_engine::run_generative_fill(
            app_handle,
            state,
            blob_kind,
            img_png,
            mask_png,
            prompt,
            // Random seed per run: retries produce genuine variants (a
            // fixed seed made every re-roll identical, so users could
            // never sample for a better result).
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 20))
                .unwrap_or(42),
            loras,
            |_| {},
        )
        .await
        .map_err(|e| e.to_string())?;
        save_debug_bytes(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-raw-engine-output"),
            &fill_png,
        );

        let filled = image::load_from_memory(&fill_png)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let filled_engine_crop = image::imageops::resize(
            &filled,
            engine_crop_img.width(),
            engine_crop_img.height(),
            image::imageops::FilterType::Lanczos3,
        );
        let mut filled_crop = if orient_for_engine {
            deorient_rgba_from_engine(
                &filled_engine_crop,
                orientation_steps,
                flip_horizontal,
                flip_vertical,
            )
        } else {
            filled_engine_crop
        };
        if filled_crop.dimensions() != (crop_w, crop_h) {
            filled_crop = image::imageops::resize(
                &filled_crop,
                crop_w,
                crop_h,
                image::imageops::FilterType::Lanczos3,
            );
        }
        // Structure measured IN-MASK ONLY: whole-crop impressions are
        // dominated by the surrounding photo and hide a blank fill.
        let blob_src_std = reconstruct_region_stats(&original_crop, &crop_mask)
            .map(|s| s.luma_std)
            .unwrap_or(0.0);
        let blob_ai_std = reconstruct_region_stats(&filled_crop, &crop_mask)
            .map(|s| s.luma_std)
            .unwrap_or(0.0);
        log::info!(
            "[fill] {blob_prefix} engine output in-mask: luma_std={blob_ai_std:.2} (source region {blob_src_std:.2}); flat below ~3.0"
        );
        if reconstruct_fill {
            let fallback_engine_crop = fallback_engine_crop
                .as_ref()
                .expect("reconstruct fallback crop should exist");
            let mut fallback_crop = if orient_for_engine {
                deorient_rgba_from_engine(
                    fallback_engine_crop,
                    orientation_steps,
                    flip_horizontal,
                    flip_vertical,
                )
            } else {
                fallback_engine_crop.clone()
            };
            if fallback_crop.dimensions() != (crop_w, crop_h) {
                fallback_crop = image::imageops::resize(
                    &fallback_crop,
                    crop_w,
                    crop_h,
                    image::imageops::FilterType::Lanczos3,
                );
            }
            let tone_strength = reconstruct_tone_strength(reconstruct_fill, promptless_reconstruct);
            if let Some(raw_full) = reconstruct_raw_ai_full.as_mut() {
                let mut raw_ai_crop = filled_crop.clone();
                harmonize_patch(&original_crop, &mut raw_ai_crop, &crop_mask, tone_strength);
                save_debug_rgba(
                    app_handle,
                    debug_run_id,
                    &format!("{blob_prefix}-raw-ai-final-crop"),
                    &raw_ai_crop,
                );
                blend_patch_into(raw_full, &raw_ai_crop, &crop_mask, x0, y0);
            }
            if let Some(fallback_full) = reconstruct_fallback_full.as_mut() {
                let mut fallback_final_crop = fallback_crop.clone();
                harmonize_patch(
                    &original_crop,
                    &mut fallback_final_crop,
                    &crop_mask,
                    tone_strength,
                );
                save_debug_rgba(
                    app_handle,
                    debug_run_id,
                    &format!("{blob_prefix}-fallback-final-crop"),
                    &fallback_final_crop,
                );
                blend_patch_into(fallback_full, &fallback_final_crop, &crop_mask, x0, y0);
            }
            if !reconstruct_single_path {
                let collapsed =
                    reconstruct_output_looks_collapsed(&filled_crop, &crop_mask, prompt);
                let worse_than_fallback = reconstruct_ai_result_lost_to_fallback(
                    &filled_crop,
                    &fallback_crop,
                    &crop_mask,
                    prompt,
                    reconstruct_strategy.unwrap(),
                );
                if collapsed || worse_than_fallback {
                    log::info!(
                        "[fill] reconstruct engine output rejected for {blob_prefix} (collapsed={collapsed}, worse_than_fallback={worse_than_fallback}); using reconstruct fallback"
                    );
                    reconstruct_rejected_any = true;
                    blob_used_fallback = true;
                    save_debug_rgba(
                        app_handle,
                        debug_run_id,
                        &format!("{blob_prefix}-seeded-fallback-crop"),
                        &fallback_crop,
                    );
                    filled_crop = fallback_crop;
                }
            } else {
                log::info!(
                    "[fill] single-path Reconstruct keeps raw engine output for {blob_prefix}; fallback rejection disabled for this test"
                );
            }
        }
        // Prompted Reconstruct should not have its intended content
        // pulled back toward the defective clipped ring. Empty-prompt
        // Reconstruct gets a modest pull because its purpose is invisible
        // scene continuation, not semantic replacement.
        let tone_strength = reconstruct_tone_strength(reconstruct_fill, promptless_reconstruct);
        harmonize_patch(&original_crop, &mut filled_crop, &crop_mask, tone_strength);
        save_debug_rgba(
            app_handle,
            debug_run_id,
            &format!("{blob_prefix}-final-crop"),
            &filled_crop,
        );
        let blob_final_std = reconstruct_region_stats(&filled_crop, &crop_mask)
            .map(|s| s.luma_std)
            .unwrap_or(0.0);
        blob_reports.push(BlobReport {
            index: blob_index,
            span: comp.span(),
            area: comp.area,
            density: component_density(comp),
            coverage: blob_coverage,
            canvas_long_edge,
            blob_px_on_canvas,
            src_std: blob_src_std,
            ai_std: blob_ai_std,
            final_std: blob_final_std,
            used_fallback: blob_used_fallback,
        });
        blend_patch_into(&mut encoded_full, &filled_crop, &crop_mask, x0, y0);
    }

    if !blob_reports.is_empty() {
        log::info!("[fill] ---- per-blob diagnostic table ----");
        log::info!(
            "[fill] idx  span   area  dens  cover%  canvas  blobpx  src_std  ai_std  fin_std  fallback  verdict"
        );
        for r in &blob_reports {
            // Verdict names the suspected mechanism for this region.
            let verdict = if r.used_fallback {
                "AI REJECTED -> fallback"
            } else if r.ai_std < 3.0 {
                if r.coverage > 0.35 {
                    "FLAT (mask covers too much canvas)"
                } else if r.blob_px_on_canvas < 200 {
                    "FLAT (blob rendered too small)"
                } else {
                    "FLAT (model returned no structure)"
                }
            } else if r.final_std < r.ai_std * 0.6 {
                "structure lost after blending"
            } else {
                "ok"
            };
            log::info!(
                "[fill] {:>3}  {:>4}  {:>6}  {:.2}  {:>5.1}  {:>6}  {:>6}  {:>7.2}  {:>6.2}  {:>7.2}  {:>8}  {}",
                r.index,
                r.span,
                r.area,
                r.density,
                r.coverage * 100.0,
                r.canvas_long_edge,
                r.blob_px_on_canvas,
                r.src_std,
                r.ai_std,
                r.final_std,
                r.used_fallback,
                verdict
            );
        }
        let flat = blob_reports
            .iter()
            .filter(|r| r.ai_std < 3.0 || r.used_fallback)
            .count();
        log::info!(
            "[fill] ---- {} of {} diffusion region(s) produced no usable structure ----",
            flat,
            blob_reports.len()
        );
    }

    if reconstruct_fill {
        save_debug_rgba(app_handle, debug_run_id, "run-final-full", &encoded_full);
        if let Some(raw) = reconstruct_raw_ai_full.as_ref() {
            save_debug_rgba(app_handle, debug_run_id, "run-raw-ai-full", raw);
        }
        if let Some(fallback) = reconstruct_fallback_full.as_ref() {
            save_debug_rgba(app_handle, debug_run_id, "run-fallback-full", fallback);
        }
    }

    Ok(EngineInpaintResult {
        image: encoded_full,
        is_linear,
        active_kind: reconstruct_fill.then_some(if reconstruct_rejected_any {
            "fallback"
        } else {
            "ai"
        }),
    })
}

/// Copies pixels from a source offset into the masked area. This is the
/// deterministic half of retouching: no model, no engine, no invention —
/// real pixels from elsewhere in the same photograph. It is the right
/// tool exactly where generative fill cannot work, such as fine repeating
/// structure that dissolves at the model's canvas resolution.
/// Graded masks (Clipped, Color select) store per-pixel CONFIDENCE, not
/// opacity: a washed-out sky rasterises at 10-25%, and one measured
/// Clipped mask peaked at 68/255. Consumed literally, that applies an
/// edit at a quarter strength and hides the region from any 50% test.
/// Anything meaningfully selected is promoted to full membership, the rim
/// stays soft, and near-noise dust is dropped.
/// Maps a point from DISPLAY space (what the user paints on: after the
/// coarse rotation and flips, before the crop) back to ORIGINAL image
/// space, where patches are composited. Brush strokes are recorded in
/// display space, so a flipped or rotated photo needs this or the patch
/// lands mirrored — measured on a flipH+flipV+1.4-degree photo where the
/// clone appeared across the frame from the painted area.
#[allow(clippy::too_many_arguments)]
pub(crate) fn display_to_image_point(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    orientation_steps: u32,
    flip_horizontal: bool,
    flip_vertical: bool,
    rotation_deg: f64,
) -> (f64, f64) {
    // Dimensions as seen after the coarse rotation.
    let (rot_w, rot_h) = if orientation_steps % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };

    // 1. undo the fine rotation about the centre
    let angle = -rotation_deg.to_radians();
    let (cx, cy) = (rot_w / 2.0, rot_h / 2.0);
    let (dx, dy) = (x - cx, y - cy);
    let (cos_a, sin_a) = (angle.cos(), angle.sin());
    let mut px = dx * cos_a - dy * sin_a + cx;
    let mut py = dx * sin_a + dy * cos_a + cy;

    // 2. undo the flips
    if flip_horizontal {
        px = rot_w - px;
    }
    if flip_vertical {
        py = rot_h - py;
    }

    // 3. undo the coarse rotation
    match orientation_steps % 4 {
        1 => (py, rot_w - px),
        2 => (rot_w - px, rot_h - py),
        3 => (rot_h - py, px),
        _ => (px, py),
    }
}

/// Same mapping for a DIRECTION (the clone offset): no translation, only
/// the rotation and mirroring parts apply.
pub(crate) fn display_to_image_vector(
    dx: f64,
    dy: f64,
    orientation_steps: u32,
    flip_horizontal: bool,
    flip_vertical: bool,
    rotation_deg: f64,
) -> (f64, f64) {
    let angle = -rotation_deg.to_radians();
    let (cos_a, sin_a) = (angle.cos(), angle.sin());
    let mut vx = dx * cos_a - dy * sin_a;
    let mut vy = dx * sin_a + dy * cos_a;
    if flip_horizontal {
        vx = -vx;
    }
    if flip_vertical {
        vy = -vy;
    }
    match orientation_steps % 4 {
        1 => (vy, -vx),
        2 => (-vx, -vy),
        3 => (-vy, vx),
        _ => (vx, vy),
    }
}

pub(crate) fn boost_mask_confidence(mask: &mut GrayImage) {
    for p in mask.pixels_mut() {
        let v = p[0] as f32 / 255.0;
        let boosted = ((v - 0.06) / (0.30 - 0.06)).clamp(0.0, 1.0);
        p[0] = (boosted * 255.0).round() as u8;
    }
}

/// Splits brush sub-masks into one unit per stroke, so each stroke can be
/// healed from its own source the way Lightroom models heal spots.
///
/// The second element is the stroke's own source offset in display space
/// when it carries one; `None` means it has never been moved and should
/// inherit the container's offset. Sub-masks that are not brush strokes
/// (a Clipped selection, say) stay whole and always inherit.
pub(crate) fn split_heal_units(
    sub_masks: &[crate::mask_generation::SubMask],
) -> Vec<(crate::mask_generation::SubMask, Option<(f64, f64)>)> {
    let mut units = Vec::new();
    for sm in sub_masks {
        let lines = sm
            .parameters
            .get("lines")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if lines.is_empty() {
            units.push((sm.clone(), None));
            continue;
        }
        for line in lines {
            let raw = line
                .get("cloneOffset")
                .and_then(|o| Some((o.get("x")?.as_f64()?, o.get("y")?.as_f64()?)));
            let mut single = sm.clone();
            if let Some(obj) = single.parameters.as_object_mut() {
                obj.insert("lines".to_string(), serde_json::json!([line]));
            }
            units.push((single, raw));
        }
    }
    units
}

/// Bounding box of every non-zero pixel in a mask.
fn mask_bounds(mask: &GrayImage) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = mask.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            if mask.get_pixel(x, y)[0] > 0 {
                any = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    any.then_some((x0, y0, x1, y1))
}

/// Chooses where a heal spot should sample from, so the user never has to.
///
/// A heal needs a source — it repairs by borrowing texture from elsewhere —
/// but picking that spot by hand is busywork. This scores candidate offsets
/// by how closely the photo around the *source* matches the photo around
/// the *destination*, which is exactly the condition under which the
/// gradient-domain blend disappears, and rejects any candidate that would
/// sample the hole being repaired. Nearer sources win ties, since texture
/// and lighting drift across a frame.
pub(crate) fn auto_clone_offset(
    image: &RgbaImage,
    mask: &GrayImage,
    bounds: (u32, u32, u32, u32),
) -> (i32, i32) {
    let (w, h) = image.dimensions();
    let (bx0, by0, bx1, by1) = bounds;
    let bw = (bx1 - bx0 + 1) as i32;
    let bh = (by1 - by0 + 1) as i32;
    let size = bw.max(bh).max(8);
    let luma = |x: i32, y: i32| -> f32 {
        let p = image.get_pixel(x.clamp(0, w as i32 - 1) as u32, y.clamp(0, h as i32 - 1) as u32);
        0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
    };

    // Sample the clean photo in a band around the spot. These are the pixels
    // a good source has to agree with.
    let band = (size / 6).clamp(4, 48);
    let stride = (size / 16).max(2);
    let mut ring: Vec<(i32, i32)> = Vec::new();
    let mut y = by0 as i32 - band;
    while y <= by1 as i32 + band {
        let mut x = bx0 as i32 - band;
        while x <= bx1 as i32 + band {
            let inside_core = x > bx0 as i32 && x < bx1 as i32 && y > by0 as i32 && y < by1 as i32;
            let in_frame = x >= 0 && y >= 0 && x < w as i32 && y < h as i32;
            if in_frame && !inside_core && mask.get_pixel(x as u32, y as u32)[0] == 0 {
                ring.push((x, y));
            }
            x += stride;
        }
        y += stride;
    }
    // Grid of spot pixels used to check a candidate does not sample the hole.
    let mut core: Vec<(i32, i32)> = Vec::new();
    let mut y = by0 as i32;
    while y <= by1 as i32 {
        let mut x = bx0 as i32;
        while x <= bx1 as i32 {
            if mask.get_pixel(x as u32, y as u32)[0] > 127 {
                core.push((x, y));
            }
            x += stride;
        }
        y += stride;
    }
    if ring.len() < 8 || core.is_empty() {
        return (0, -size.max(40));
    }

    let score = |dx: i32, dy: i32| -> Option<f32> {
        // Never sample the area being repaired.
        let mut hit = 0usize;
        for (x, y) in &core {
            let (sx, sy) = (x + dx, y + dy);
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                return None;
            }
            if mask.get_pixel(sx as u32, sy as u32)[0] > 0 {
                hit += 1;
            }
        }
        if hit * 50 > core.len() {
            return None;
        }
        let mut err = 0.0f32;
        for (x, y) in &ring {
            let (sx, sy) = (x + dx, y + dy);
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                return None;
            }
            err += (luma(*x, *y) - luma(sx, sy)).abs();
        }
        let mean = err / ring.len() as f32;
        // Mild pull toward nearer sources: lighting and texture drift.
        Some(mean + 0.004 * ((dx * dx + dy * dy) as f32).sqrt())
    };

    let min_shift = (size * 3) / 4;
    let max_shift = size * 3;
    let mut best: Option<(f32, i32, i32)> = None;
    let coarse = (size / 6).max(8);
    let mut dy = -max_shift;
    while dy <= max_shift {
        let mut dx = -max_shift;
        while dx <= max_shift {
            let far_enough = dx * dx + dy * dy >= min_shift * min_shift;
            match far_enough.then(|| score(dx, dy)).flatten() {
                Some(sc) if best.is_none_or(|(b, _, _)| sc < b) => best = Some((sc, dx, dy)),
                _ => {}
            }
            dx += coarse;
        }
        dy += coarse;
    }
    let Some((_, cx, cy)) = best else {
        return (0, -size.max(40));
    };

    // Refine around the winner so the answer is not stuck on the coarse grid.
    let fine = (coarse / 3).max(2);
    let mut refined = (f32::MAX, cx, cy);
    let mut dy = cy - coarse;
    while dy <= cy + coarse {
        let mut dx = cx - coarse;
        while dx <= cx + coarse {
            let far_enough = dx * dx + dy * dy >= min_shift * min_shift;
            match far_enough.then(|| score(dx, dy)).flatten() {
                Some(sc) if sc < refined.0 => refined = (sc, dx, dy),
                _ => {}
            }
            dx += fine;
        }
        dy += fine;
    }
    (refined.1, refined.2)
}

/// A region the fill will process: a mask component — possibly one tile of
/// a larger one — plus the rect it was cut from when it is a tile.
type FillUnit = (MaskComponent, Option<(u32, u32, u32, u32)>);

pub(crate) fn clone_offset_copy(
    image: &RgbaImage,
    mask: &GrayImage,
    offset_x: i32,
    offset_y: i32,
) -> RgbaImage {
    let (w, h) = image.dimensions();
    let mut out = image.clone();
    for y in 0..h {
        for x in 0..w {
            if mask.get_pixel(x, y)[0] == 0 {
                continue;
            }
            // Clamp so a source point outside the frame reuses the edge
            // rather than leaving a hole.
            let sx = (x as i32 + offset_x).clamp(0, w as i32 - 1) as u32;
            let sy = (y as i32 + offset_y).clamp(0, h as i32 - 1) as u32;
            let src = *image.get_pixel(sx, sy);
            let dst = out.get_pixel_mut(x, y);
            dst[0] = src[0];
            dst[1] = src[1];
            dst[2] = src[2];
        }
    }
    out
}

/// Clone/heal stamp: builds a patch by copying from a source offset,
/// harmonised to its surroundings exactly like a fill patch so the seam
/// disappears. Deterministic and fast — no engine involved.
#[tauri::command]
pub async fn apply_clone_patch(
    path: String,
    patch_definition: AiPatchDefinition,
    heal: bool,
    current_adjustments: Value,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let mut source_adjustments = current_adjustments.clone();
    if let Some(patches) = source_adjustments
        .get_mut("aiPatches")
        .and_then(|v| v.as_array_mut())
    {
        patches.retain(|p| p.get("id").and_then(|id| id.as_str()) != Some(&patch_definition.id));
    }

    let t_total = std::time::Instant::now();
    let t_source = std::time::Instant::now();
    let (base_image, _) = get_full_image_for_processing(&state)?;
    let source_image = composite_patches_on_image(&base_image, &source_adjustments)
        .map_err(|e| format!("Failed to prepare source image: {}", e))?;
    let ms_source = t_source.elapsed().as_millis();
    let (img_w, img_h) = source_image.dimensions();

    let mut sub_masks = patch_definition.sub_masks.clone();
    neutralize_display_orientation(&mut sub_masks);

    // Brush strokes are recorded in display space; the patch composites in
    // original space. On a flipped or rotated photo those differ.
    let steps = current_adjustments["orientationSteps"].as_u64().unwrap_or(0) as u32;
    let flip_h = current_adjustments["flipHorizontal"].as_bool().unwrap_or(false);
    let flip_v = current_adjustments["flipVertical"].as_bool().unwrap_or(false);
    let rotation = current_adjustments["rotation"].as_f64().unwrap_or(0.0);
    let needs_mapping = steps % 4 != 0 || flip_h || flip_v || rotation.abs() > 1e-6;
    if needs_mapping {
        for sm in sub_masks.iter_mut() {
            let Some(lines) = sm
                .parameters
                .get_mut("lines")
                .and_then(|v| v.as_array_mut())
            else {
                continue;
            };
            for line in lines.iter_mut() {
                let Some(points) = line.get_mut("points").and_then(|v| v.as_array_mut()) else {
                    continue;
                };
                for point in points.iter_mut() {
                    let (Some(px), Some(py)) = (
                        point.get("x").and_then(|v| v.as_f64()),
                        point.get("y").and_then(|v| v.as_f64()),
                    ) else {
                        continue;
                    };
                    let (mx, my) = display_to_image_point(
                        px,
                        py,
                        img_w as f64,
                        img_h as f64,
                        steps,
                        flip_h,
                        flip_v,
                        rotation,
                    );
                    point["x"] = serde_json::json!(mx);
                    point["y"] = serde_json::json!(my);
                }
            }
        }
        log::info!(
            "[clone] mapped strokes from display space (steps={steps}, flipH={flip_h}, flipV={flip_v}, rot={rotation})"
        );
    }
    // Each brush stroke is its own heal spot pulling from its own source,
    // the way Lightroom models them. Build one mask per stroke so they can
    // be healed independently; a stroke with no source of its own (and any
    // non-brush selection, e.g. a Clipped one) falls back to the
    // container's offset.
    let build_def = |masks: Vec<crate::mask_generation::SubMask>| MaskDefinition {
        id: patch_definition.id.clone(),
        name: patch_definition.name.clone(),
        visible: true,
        invert: patch_definition.invert,
        opacity: 100.0,
        grow: 0.0,
        feather: 0.0,
        adjustments: Value::Null,
        sub_masks: masks,
    };

    // A stroke keeps whatever source the user dragged it to; one that has
    // never been touched gets a source chosen for it below, once its mask
    // exists. Nobody has to dial in an offset by hand.
    let units: Vec<(crate::mask_generation::SubMask, Option<(i32, i32)>)> =
        split_heal_units(&sub_masks)
            .into_iter()
            .map(|(sm, raw)| {
                let off = match raw {
                    Some((ox, oy)) if needs_mapping => {
                        let (mx, my) =
                            display_to_image_vector(ox, oy, steps, flip_h, flip_v, rotation);
                        Some((mx.round() as i32, my.round() as i32))
                    }
                    Some((ox, oy)) => Some((ox.round() as i32, oy.round() as i32)),
                    None => None,
                };
                (sm, off)
            })
            .collect();

    let full_def = build_def(sub_masks.clone());
    let warped = resolve_warped_image_for_masks(
        &state,
        &current_adjustments,
        std::slice::from_ref(&full_def),
    );

    let is_linear = state
        .original_image
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|l| l.is_raw)
        && matches!(
            source_image,
            DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
        );
    let encoded_full = if is_linear {
        ai_processing::gamma_encode_rgba8(&source_image)
    } else {
        source_image.to_rgba8()
    };

    // Heal and clone differ in exactly one way: a clone reproduces the
    // source verbatim, a heal keeps its texture but takes its tone from the
    // destination. Global harmonisation is not enough for the latter — on a
    // real edit it left a 78/255 step at the mask edge — so healing solves
    // for a seamless blend instead.
    //
    // Spots are healed in turn, each reading from the running result, so a
    // later spot may sample from an earlier repair — the same way repeated
    // stamping behaves by hand.
    let mut working = encoded_full.clone();
    let mut union_mask = image::GrayImage::new(img_w, img_h);
    let mut healed_spots = 0usize;
    let mut auto_sourced = 0usize;
    let (mut ms_masks, mut ms_heal) = (0u128, 0u128);
    for (unit, unit_offset) in units {
        let t_mask = std::time::Instant::now();
        let unit_def = build_def(vec![unit]);
        let Some(bitmap) = generate_mask_bitmap(
            &unit_def,
            img_w,
            img_h,
            1.0,
            (0.0, 0.0),
            warped.as_deref(),
        ) else {
            continue;
        };
        let unwarped = apply_unwarp_geometry(
            Cow::Owned(DynamicImage::ImageLuma8(bitmap)),
            &current_adjustments,
        )
        .into_owned();
        let mut spot_mask = unwarped.to_luma8();
        // Same promotion the generative path does: a Clipped selection
        // hands over confidence values (one measured mask peaked at
        // 68/255), which consumed literally would clone at quarter strength.
        boost_mask_confidence(&mut spot_mask);
        let Some(spot_bounds) = mask_bounds(&spot_mask) else {
            continue;
        };
        let (ox, oy) = match unit_offset {
            Some(off) => off,
            None => {
                auto_sourced += 1;
                auto_clone_offset(&encoded_full, &spot_mask, spot_bounds)
            }
        };
        ms_masks += t_mask.elapsed().as_millis();
        let t_heal = std::time::Instant::now();
        working = if heal {
            crate::heal_blend::heal_blend(&working, &spot_mask, ox, oy)
        } else {
            let mut c = clone_offset_copy(&working, &spot_mask, ox, oy);
            harmonize_patch(&working, &mut c, &spot_mask, 1.0);
            c
        };
        ms_heal += t_heal.elapsed().as_millis();
        for (u, s) in union_mask.pixels_mut().zip(spot_mask.pixels()) {
            u[0] = u[0].max(s[0]);
        }
        healed_spots += 1;
    }

    let masked_px = union_mask.pixels().filter(|p| p[0] > 0).count();
    if masked_px == 0 {
        // Every spot has been deleted. Returning an error here left the
        // previous composite in place, so a deleted repair stayed on screen
        // after its marker had gone. Hand back an explicit empty result so
        // the caller clears it.
        // Evict the cached bitmap too: the preview sends a null patchData
        // for a cleared patch, and hydrate_adjustments would otherwise
        // rehydrate the deleted repair straight back onto the photo.
        if let Ok(mut cache) = state.patch_cache.lock() {
            cache.remove(&patch_definition.id);
        }
        log::info!("[clone] no spots left — patch and cached bitmap cleared");
        return Ok("null".to_string());
    }
    let t_encode = std::time::Instant::now();
    let patch_json = encode_patch_result(&working, is_linear, &union_mask)?;
    log::info!(
        "[clone] {healed_spots} spot(s) ({auto_sourced} auto-sourced), {masked_px} px total \
         (heal={heal}) | source {ms_source}ms, masks {ms_masks}ms, blend {ms_heal}ms, \
         encode {}ms, total {}ms",
        t_encode.elapsed().as_millis(),
        t_total.elapsed().as_millis()
    );
    let _ = path;
    let _ = app_handle;
    Ok(patch_json)
}

#[tauri::command]
pub async fn invoke_generative_replace_with_mask_def(
    path: String,
    patch_definition: AiPatchDefinition,
    current_adjustments: Value,
    use_fast_inpaint: bool,
    token: Option<String>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let reconstruct_fill = patch_uses_clipped_reconstruct(&patch_definition);
    let reconstruct_single_path = reconstruct_fill && patch_definition.reconstruct_single_path;
    let preserve_negative_refinements = patch_has_negative_refinement(&patch_definition);
    let (orientation_steps, flip_horizontal, flip_vertical) =
        ai_fill_orientation_from_adjustments(&current_adjustments);
    let force_engine_for_reconstruct =
        reconstruct_fill && (reconstruct_single_path || !patch_definition.prompt.trim().is_empty());
    let effective_use_fast_inpaint = use_fast_inpaint && !force_engine_for_reconstruct;
    if force_engine_for_reconstruct && use_fast_inpaint {
        log::info!(
            "[fill] Reconstruct requested fast mode; using prompt-conditioned engine instead"
        );
    }
    if reconstruct_single_path {
        log::info!("[fill] single-path Reconstruct test enabled");
    }
    let debug_run_id = if reconstruct_fill {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("{}-{millis}", patch_definition.id)
    } else {
        String::new()
    };
    let debug_dir = if !debug_run_id.is_empty() {
        ai_fill_debug_dir(&app_handle, &debug_run_id).map(|dir| {
            log::info!("[fill] debug artifacts: {:?}", dir);
            dir.to_string_lossy().to_string()
        })
    } else {
        None
    };

    let mut source_image_adjustments = current_adjustments.clone();
    if let Some(patches) = source_image_adjustments
        .get_mut("aiPatches")
        .and_then(|v| v.as_array_mut())
    {
        patches.retain(|p| p.get("id").and_then(|id| id.as_str()) != Some(&patch_definition.id));
    }

    let (base_image, _) = get_full_image_for_processing(&state)?;
    let source_image = composite_patches_on_image(&base_image, &source_image_adjustments)
        .map_err(|e| format!("Failed to prepare source image: {}", e))?;

    let (img_w, img_h) = source_image.dimensions();
    // Value-derived masks (clipped/color/luminance) threshold the WARPED
    // full image — their output is already in full-image space. The
    // orientation parameters they carry (rotation/flips/coarse steps)
    // exist to map that mask into DISPLAY space for the overlay; applying
    // them here left the fill mask mirrored/rotated relative to the
    // original on any flipped or rotated photo — the fill then edited the
    // mirror-image position ("found the highlights, edited the foliage").
    let mut sub_masks = patch_definition.sub_masks;
    neutralize_display_orientation(&mut sub_masks);
    let mask_def_for_generation = MaskDefinition {
        id: patch_definition.id.clone(),
        name: patch_definition.name.clone(),
        visible: patch_definition.visible,
        invert: patch_definition.invert,
        opacity: 100.0,
        grow: 0.0,
        feather: 0.0,
        adjustments: serde_json::Value::Null,
        sub_masks,
    };

    let warped_image = resolve_warped_image_for_masks(
        &state,
        &current_adjustments,
        std::slice::from_ref(&mask_def_for_generation),
    );

    let mask_bitmap = generate_mask_bitmap(
        &mask_def_for_generation,
        img_w,
        img_h,
        1.0,
        (0.0, 0.0),
        warped_image.as_deref(),
    )
    .ok_or("Failed to generate mask bitmap for AI replace")?;

    let mask_dynamic = DynamicImage::ImageLuma8(mask_bitmap);
    let unwarped_dynamic =
        apply_unwarp_geometry(Cow::Borrowed(&mask_dynamic), &current_adjustments).into_owned();
    let mut mask_bitmap = unwarped_dynamic.to_luma8();

    // Graded masks (Clipped/Color select) store per-pixel CONFIDENCE, not
    // opacity — a washed-out sky rasterizes at 10-25%. Consumed literally,
    // that (a) blends the fill at one-fifth strength (invisible result)
    // and (b) hides the region from the blob router, which thresholds at
    // 50%. A generative fill needs region membership: boost anything
    // meaningfully selected to full strength, keep only the rim soft, and
    // drop near-noise dust (which otherwise becomes thousands of
    // one-speck LaMa jobs).
    let pre_boost = mask_bitmap.pixels().filter(|p| p[0] > 0).count();
    boost_mask_confidence(&mut mask_bitmap);
    let mut nonzero = mask_bitmap.pixels().filter(|p| p[0] > 0).count();
    log::info!(
        "[fill] mask confidence boost: {pre_boost} px selected -> {nonzero} px at working strength"
    );

    if reconstruct_fill && !effective_use_fast_inpaint {
        let (selected, strong) = materialize_reconstruct_mask(&mut mask_bitmap);
        log::info!(
            "[fill] reconstruct mask materialized: {selected} selected px -> {strong} full-strength px"
        );
        consolidate_reconstruct_mask(&mut mask_bitmap, preserve_negative_refinements);
    }

    // Consolidation guard: threshold-noise selections fragment into
    // thousands of specks, each of which would become its own LaMa job
    // (measured: 6,304 specks = ~an hour of compute for invisible dust
    // healing). When the mask is badly fragmented, close nearby specks
    // into solid regions and drop isolated dust outright.
    if !(reconstruct_fill && preserve_negative_refinements) {
        let (_, comps) = mask_components(&mask_bitmap, 127);
        if comps.len() > 200 {
            let scale = (mask_bitmap.width().max(mask_bitmap.height()) as f32 / 2000.0).max(1.0);
            crate::mask_generation::apply_solidify_public(&mut mask_bitmap, 60.0, scale);
            // Square-kernel closing leaves rectangles; round them into
            // organic contours before anything downstream consumes them.
            mask_bitmap = ai_processing::round_mask_geometry(&mask_bitmap, 8.0 * scale);
            let (labels2, comps2) = mask_components(&mask_bitmap, 127);
            let mut drop = vec![false; comps2.len() + 1];
            for c in &comps2 {
                if c.area < 250 {
                    drop[c.id as usize] = true;
                }
            }
            let w = mask_bitmap.width() as usize;
            for (i, p) in mask_bitmap.pixels_mut().enumerate() {
                let label = labels2[(i / w) * w + (i % w)];
                if label != 0 && drop[label as usize] {
                    p[0] = 0;
                }
            }
            let (_, comps3) = mask_components(&mask_bitmap, 127);
            log::info!(
                "[fill] consolidation guard: {} fragments -> {} regions (solidify + dust drop)",
                comps.len(),
                comps3.len()
            );
        }
    } else {
        let (_, comps) = mask_components(&mask_bitmap, 127);
        if comps.len() > 200 {
            log::info!(
                "[fill] skipped fragmentation consolidation to preserve reconstruct refinements ({} regions)",
                comps.len()
            );
        }
    }
    nonzero = mask_bitmap.pixels().filter(|p| p[0] > 0).count();
    log::info!(
        "generative_replace: image {}x{}, mask {}x{}, {} masked px, {} sub-masks, fast={}, reconstruct={}",
        img_w,
        img_h,
        mask_bitmap.width(),
        mask_bitmap.height(),
        nonzero,
        mask_def_for_generation.sub_masks.len(),
        effective_use_fast_inpaint,
        reconstruct_fill
    );
    // An empty selection previously slipped through to the model, which
    // returns the image unchanged — looking like a silent failure.
    if nonzero == 0 {
        return Err(
            "The selection is empty — brush over the area to remove, then try again.".to_string(),
        );
    }
    let composite_mask_bitmap = if reconstruct_fill && !effective_use_fast_inpaint {
        let blend = reconstruct_composite_mask(&mask_bitmap, preserve_negative_refinements);
        let blend_nonzero = blend.pixels().filter(|p| p[0] > 0).count();
        let blend_strong = blend.pixels().filter(|p| p[0] > 127).count();
        log::info!(
            "[fill] reconstruct composite mask: {blend_nonzero} soft px, {blend_strong} strong px"
        );
        blend
    } else {
        mask_bitmap.clone()
    };
    let promptless_reconstruct = reconstruct_fill && patch_definition.prompt.trim().is_empty();
    let auto_hint = if promptless_reconstruct {
        infer_reconstruct_auto_hint(&source_image, &mask_bitmap)
    } else {
        ReconstructAutoHint::Generic
    };
    let prompt_for_engine =
        effective_reconstruct_prompt(&patch_definition.prompt, reconstruct_fill, auto_hint);
    if promptless_reconstruct {
        log::info!("[fill] using automatic Reconstruct prompt: {:?}", auto_hint);
    }

    // Which local inpaint model is selected decides the local paths: the
    // generative engine (SDXL fill) handles both plain removal and
    // prompt-driven replace; LaMa remains the fast texture fill.
    // The Fast toggle is the user's word: honor it even when an engine
    // model is selected (previously the engine silently overrode it and
    // 'fast' runs took six diffusion round-trips).
    let engine_model = if effective_use_fast_inpaint {
        None
    } else {
        resolve_and_prepare(
            &app_handle,
            &state.model_registry,
            TaskType::Inpaint,
            "inpaint",
            |m| m.params.get("engine").and_then(|v| v.as_str()) == Some("comfy"),
        )
        .await
        .ok()
    };

    let patch_result = if let Some((_, model)) = engine_model {
        let kind = crate::comfy_engine::FillKind::from_params(&model.manifest.params);
        log::info!(
            "[fill] selected engine model {} ({})",
            model.manifest.id,
            model.manifest.display_name
        );
        run_engine_inpaint_patch(
            &source_image,
            &mask_bitmap,
            prompt_for_engine.as_ref(),
            kind,
            false,
            reconstruct_fill,
            promptless_reconstruct,
            reconstruct_single_path,
            patch_definition.generate_mode,
            patch_definition.content_scale,
            patch_definition.match_photo,
            &patch_definition.loras,
            &debug_run_id,
            orientation_steps,
            flip_horizontal,
            flip_vertical,
            &app_handle,
            &state,
        )
        .await?
    } else if effective_use_fast_inpaint {
        // Fast mode gets the same per-blob split + harmonization as the
        // engine path — every blob heals via LaMa. The old whole-mask
        // single LaMa pass is exactly what produced smeary results on
        // scattered selections.
        run_engine_inpaint_patch(
            &source_image,
            &mask_bitmap,
            prompt_for_engine.as_ref(),
            crate::comfy_engine::FillKind::SdxlBase,
            true,
            false,
            false,
            false,
            false,
            1.0,
            0.8,
            &[],
            "",
            orientation_steps,
            flip_horizontal,
            flip_vertical,
            &app_handle,
            &state,
        )
        .await?
    } else if settings.ai_provider.as_deref() == Some("cloud")
        && let Some(auth_token) = token
    {
        let base_url = "https://getrapidraw.com/api";

        let mut rgba_mask = RgbaImage::new(img_w, img_h);
        for (x, y, luma_pixel) in mask_bitmap.enumerate_pixels() {
            let intensity = luma_pixel[0];
            rgba_mask.put_pixel(x, y, Rgba([intensity, intensity, intensity, 255]));
        }
        let mask_image_dynamic = DynamicImage::ImageRgba8(rgba_mask);

        let (real_path_buf, _) = crate::file_management::parse_virtual_path(&path);

        ai_connector::process_inpainting(
            base_url,
            &real_path_buf.to_string_lossy(),
            &source_image,
            &mask_image_dynamic,
            prompt_for_engine.to_string(),
            Some(&auth_token),
        )
        .await
        .map_err(|e| e.to_string())
        .map(|img| EngineInpaintResult {
            image: img,
            is_linear: false,
            active_kind: None,
        })?
    } else if settings.ai_provider.as_deref() == Some("ai-connector")
        && let Some(address) = settings.ai_connector_address
    {
        let base_url = format!("http://{}", address);

        let mut rgba_mask = RgbaImage::new(img_w, img_h);
        for (x, y, luma_pixel) in mask_bitmap.enumerate_pixels() {
            let intensity = luma_pixel[0];
            rgba_mask.put_pixel(x, y, Rgba([intensity, intensity, intensity, 255]));
        }
        let mask_image_dynamic = DynamicImage::ImageRgba8(rgba_mask);

        let (real_path_buf, _) = crate::file_management::parse_virtual_path(&path);

        ai_connector::process_inpainting(
            &base_url,
            &real_path_buf.to_string_lossy(),
            &source_image,
            &mask_image_dynamic,
            prompt_for_engine.to_string(),
            None,
        )
        .await
        .map_err(|e| e.to_string())
        .map(|img| EngineInpaintResult {
            image: img,
            is_linear: false,
            active_kind: None,
        })?
    } else {
        return Err(
            "No generative backend configured or connection invalid. Please check your AI settings."
                .to_string(),
        );
    };

    let mut payload = encode_patch_result_value(
        &patch_result.image,
        patch_result.is_linear,
        &composite_mask_bitmap,
    )?;

    if reconstruct_fill {
        payload = attach_reconstruct_response_metadata(
            payload,
            ReconstructResponseInputs {
                current_adjustments: &current_adjustments,
                patch_id: &patch_definition.id,
                prompt: &patch_definition.prompt,
                debug_run_id: &debug_run_id,
                debug_dir: debug_dir.clone(),
                auto_hint,
                effective_prompt: prompt_for_engine.as_ref(),
                active_kind: patch_result.active_kind.unwrap_or("ai"),
            },
        );
        save_debug_json(
            &app_handle,
            &debug_run_id,
            "manifest",
            payload
                .get("reconstructManifest")
                .unwrap_or(&serde_json::Value::Null),
        );
    }

    Ok(payload.to_string())
}

/// Encodes a full-size result image + mask into the aiPatches payload the
/// frontend stores in the sidecar (PNG, not JPEG: deep-shadow fills live at
/// pixel values 0-5 where JPEG block noise becomes banding under exposure
/// boosts; the patch is mostly black, so PNG stays small).
fn encode_patch_result_value(
    patch_rgba: &RgbaImage,
    patch_is_gamma: bool,
    mask_bitmap: &GrayImage,
) -> Result<Value, String> {
    let (patch_w, patch_h) = patch_rgba.dimensions();
    let scaled_mask_bitmap = image::imageops::resize(
        mask_bitmap,
        patch_w,
        patch_h,
        image::imageops::FilterType::Lanczos3,
    );
    let mut color_image = RgbImage::new(patch_w, patch_h);
    let mask_image = scaled_mask_bitmap.clone();

    for y in 0..patch_h {
        for x in 0..patch_w {
            let mask_value = scaled_mask_bitmap.get_pixel(x, y)[0];

            if mask_value > 0 {
                let patch_pixel = patch_rgba.get_pixel(x, y);
                color_image.put_pixel(x, y, Rgb([patch_pixel[0], patch_pixel[1], patch_pixel[2]]));
            } else {
                color_image.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
    }

    let mut color_buf = Cursor::new(Vec::new());
    color_image
        .write_to(&mut color_buf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let color_base64 = general_purpose::STANDARD.encode(color_buf.get_ref());

    let mut mask_buf = Cursor::new(Vec::new());
    mask_image
        .write_to(&mut mask_buf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mask_base64 = general_purpose::STANDARD.encode(mask_buf.get_ref());

    Ok(serde_json::json!({
        "color": color_base64,
        "mask": mask_base64,
        "encoding": if patch_is_gamma { "gamma" } else { "linear" },
    }))
}

fn encode_patch_result(
    patch_rgba: &RgbaImage,
    patch_is_gamma: bool,
    mask_bitmap: &GrayImage,
) -> Result<String, String> {
    Ok(encode_patch_result_value(patch_rgba, patch_is_gamma, mask_bitmap)?.to_string())
}

fn current_patch_data_value<'a>(
    current_adjustments: &'a Value,
    patch_id: &str,
) -> Option<&'a Value> {
    current_adjustments
        .get("aiPatches")
        .and_then(Value::as_array)?
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some(patch_id))?
        .get("patchData")
}

fn reconstruct_variant_from_payload(
    id: String,
    label: String,
    kind: &str,
    payload: &Value,
    prompt: &str,
    debug_run_id: &str,
    debug_dir: Option<&str>,
) -> Value {
    serde_json::json!({
        "id": id,
        "label": label,
        "kind": kind,
        "color": payload.get("color").and_then(Value::as_str).unwrap_or_default(),
        "mask": payload.get("mask").and_then(Value::as_str).unwrap_or_default(),
        "encoding": payload.get("encoding").and_then(Value::as_str).unwrap_or("gamma"),
        "prompt": prompt,
        "debugRunId": debug_run_id,
        "debugDir": debug_dir,
        "createdAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    })
}

struct ReconstructResponseInputs<'a> {
    current_adjustments: &'a Value,
    patch_id: &'a str,
    prompt: &'a str,
    debug_run_id: &'a str,
    debug_dir: Option<String>,
    auto_hint: ReconstructAutoHint,
    effective_prompt: &'a str,
    active_kind: &'static str,
}

fn attach_reconstruct_response_metadata(
    mut active_payload: Value,
    inputs: ReconstructResponseInputs<'_>,
) -> Value {
    let previous_patch_data = current_patch_data_value(inputs.current_adjustments, inputs.patch_id);
    let mut variants = previous_patch_data
        .and_then(|pd| pd.get("reconstructVariants"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if variants.is_empty()
        && let Some(previous) = previous_patch_data
        && previous
            .get("color")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    {
        variants.push(reconstruct_variant_from_payload(
            "attempt-1".to_string(),
            "Attempt 1".to_string(),
            previous
                .get("reconstructActiveKind")
                .and_then(Value::as_str)
                .unwrap_or("ai"),
            previous,
            previous
                .get("reconstructPrompt")
                .and_then(Value::as_str)
                .unwrap_or(inputs.prompt),
            previous
                .get("reconstructDebugRunId")
                .and_then(Value::as_str)
                .unwrap_or(""),
            previous.get("reconstructDebugDir").and_then(Value::as_str),
        ));
    }

    let attempt_index = variants
        .iter()
        .filter(|v| {
            v.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("attempt-"))
        })
        .count()
        + 1;
    let active_id = format!("attempt-{attempt_index}");
    variants.push(reconstruct_variant_from_payload(
        active_id.clone(),
        format!("Attempt {attempt_index}"),
        inputs.active_kind,
        &active_payload,
        inputs.prompt,
        inputs.debug_run_id,
        inputs.debug_dir.as_deref(),
    ));

    let manifest = serde_json::json!({
        "patchId": inputs.patch_id,
        "activeVariantId": active_id,
        "activeKind": inputs.active_kind,
        "debugRunId": inputs.debug_run_id,
        "debugDir": inputs.debug_dir,
        "prompt": inputs.prompt,
        "effectivePrompt": inputs.effective_prompt,
        "autoHint": format!("{:?}", inputs.auto_hint),
        "variantCount": variants.len(),
    });

    active_payload["reconstructVariants"] = Value::Array(variants);
    active_payload["reconstructActiveVariantId"] = Value::String(active_id);
    active_payload["reconstructActiveKind"] = Value::String(inputs.active_kind.to_string());
    active_payload["reconstructPrompt"] = Value::String(inputs.prompt.to_string());
    active_payload["reconstructEffectivePrompt"] =
        Value::String(inputs.effective_prompt.to_string());
    active_payload["reconstructAutoHint"] = Value::String(format!("{:?}", inputs.auto_hint));
    active_payload["reconstructDebugRunId"] = Value::String(inputs.debug_run_id.to_string());
    active_payload["reconstructDebugDir"] =
        inputs.debug_dir.map(Value::String).unwrap_or(Value::Null);
    active_payload["reconstructManifest"] = manifest;

    active_payload
}

/// Spot enhancement: runs the selected enhancement model (deblur / restore /
/// upscale-as-sharpen) on a crop around the brushed region only, and
/// feather-composites the result back — same patch contract as removal.
#[allow(clippy::too_many_arguments)]
async fn run_spot_enhance_patch(
    source_image: &DynamicImage,
    mask: &GrayImage,
    task_type: crate::model_registry::TaskType,
    task_key: &str,
    strength: f32,
    texture: f32,
    grain: f32,
    patch_id: &str,
    app_handle: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
) -> Result<(RgbaImage, bool), String> {
    // Linear-source detection must come from the SOURCE, not the pixel
    // format: compositing patches returns float pixels even for JPEGs,
    // and gamma-encoding an already-display-encoded JPEG double-brightens
    // the canvas the model is conditioned on (measured: canvas mean 0.74
    // vs 0.46 true) — the model then paints matching blown-out wash.
    let is_linear = state
        .original_image
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|l| l.is_raw)
        && matches!(
            source_image,
            DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
        );
    let (w, h) = source_image.dimensions();

    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0u32, 0u32);
    for (x, y, p) in mask.enumerate_pixels() {
        if p[0] > 0 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    // Enhancement needs less surrounding context than generative fill, but
    // fixed-size models (NAFNet's 512 tiles) want a reasonable crop.
    let pad_x = 96.max(((max_x - min_x + 1) as f32 * 0.75) as u32);
    let pad_y = 96.max(((max_y - min_y + 1) as f32 * 0.75) as u32);
    let x0 = min_x.saturating_sub(pad_x);
    let y0 = min_y.saturating_sub(pad_y);
    let x1 = (max_x + pad_x).min(w.saturating_sub(1));
    let y1 = (max_y + pad_y).min(h.saturating_sub(1));
    let (crop_w, crop_h) = (x1 - x0 + 1, y1 - y0 + 1);

    let mut encoded_full = if is_linear {
        ai_processing::gamma_encode_rgba8(source_image)
    } else {
        source_image.to_rgba8()
    };
    let crop_img = image::imageops::crop_imm(&encoded_full, x0, y0, crop_w, crop_h).to_image();
    let crop_mask = image::imageops::crop_imm(mask, x0, y0, crop_w, crop_h).to_image();

    let (registry, model) = crate::model_registry::resolve_and_prepare(
        app_handle,
        &state.model_registry,
        task_type,
        task_key,
        |_| true,
    )
    .await
    .map_err(|e| e.to_string())?;

    let is_engine = model.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy");

    // f32 [0,1] working copy of the crop for the model + blend.
    let crop_f32: image::Rgb32FImage = DynamicImage::ImageRgba8(crop_img.clone()).to_rgb32f();

    let enhanced_f32: image::Rgb32FImage = if is_engine {
        let model_file = std::path::Path::new(&model.manifest.file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| "Invalid engine model path".to_string())?
            .to_string();
        // 2x-and-back through the engine sharpens real detail; align dims
        // to what the engine tolerates.
        let target = (crop_w.min(crop_h) * 2).clamp(256, 1024);
        let aligned = crate::enhancement::align_for_engine(&crop_f32, target, target);
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb32F(aligned)
            .to_rgb8()
            .write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        let result_png = crate::comfy_engine::run_seedvr2(
            app_handle,
            state,
            &model_file,
            buf.into_inner(),
            target,
            42,
            |_| {},
        )
        .await
        .map_err(|e| e.to_string())?;
        let out = image::load_from_memory(&result_png)
            .map_err(|e| e.to_string())?
            .to_rgb32f();
        image::imageops::resize(&out, crop_w, crop_h, image::imageops::FilterType::Lanczos3)
    } else {
        let session = registry
            .get_session(&model.manifest.id, None)
            .map_err(|e| e.to_string())?;
        let params = crate::enhancement::model_params(&model);
        let raw = tokio::task::spawn_blocking(move || {
            let out = if params.single_pass {
                crate::enhancement::run_single_pass_enhancement(
                    &crop_f32,
                    &session,
                    params.scale,
                    params.pad_multiple,
                )
                .map_err(|e| e.to_string())
            } else {
                crate::enhancement::run_tiled_enhancement(
                    &crop_f32,
                    &session,
                    params.scale,
                    params.tile_size,
                    params.tile_overlap,
                    params.fixed_size,
                    |_, _| {},
                )
                .map_err(|e| e.to_string())
            };
            registry.unload(&model.manifest.id);
            out
        })
        .await
        .map_err(|e| e.to_string())??;
        // Deliver at 1x: an upscaler resized back acts as a pure sharpener.
        image::imageops::resize(&raw, crop_w, crop_h, image::imageops::FilterType::Lanczos3)
    };

    // The full strength/texture/grain blend (same engine as the enhance
    // dialog), then feathered composite of only the brushed pixels.
    let original_f32: image::Rgb32FImage = DynamicImage::ImageRgba8(crop_img).to_rgb32f();
    let pristine = encoded_full.clone();
    let result = composite_spot_blend(
        &mut encoded_full,
        &enhanced_f32,
        &original_f32,
        &crop_mask,
        (x0, y0),
        strength,
        texture,
        grain,
    );

    // Cache the raw region so strength/texture/grain stay editable after
    // rendering — a re-blend, not a model re-run.
    *state.spot_raw.lock().unwrap() = Some(crate::app_state::SpotRaw {
        patch_id: patch_id.to_string(),
        raw: enhanced_f32,
        original: original_f32,
        encoded_full: pristine,
        crop_mask,
        crop_origin: (x0, y0),
        full_mask: mask.clone(),
        is_linear,
    });

    Ok((result, is_linear))
}

/// Blends the spot region at the given settings and composites it into a
/// copy of `encoded_full`, feathered to the brushed pixels only.
#[allow(clippy::too_many_arguments)]
fn composite_spot_blend(
    encoded_full: &mut RgbaImage,
    enhanced_f32: &image::Rgb32FImage,
    original_f32: &image::Rgb32FImage,
    crop_mask: &GrayImage,
    origin: (u32, u32),
    strength: f32,
    texture: f32,
    grain: f32,
) -> RgbaImage {
    let (crop_w, crop_h) = enhanced_f32.dimensions();
    let blended = crate::enhancement::blend_result(
        enhanced_f32,
        original_f32,
        crop_w,
        crop_h,
        strength,
        texture,
        grain,
    );
    let feather = ((crop_w.max(crop_h) as f32) / 100.0).clamp(3.0, 12.0);
    let soft_mask = image::imageops::blur(crop_mask, feather);
    for y in 0..crop_h {
        for x in 0..crop_w {
            let m = soft_mask.get_pixel(x, y)[0];
            if m > 0 {
                let alpha = m as f32 / 255.0;
                let b = blended.get_pixel(x, y);
                let o = original_f32.get_pixel(x, y);
                let dst = encoded_full.get_pixel_mut(origin.0 + x, origin.1 + y);
                for c in 0..3 {
                    let v = b[c] * alpha + o[c] * (1.0 - alpha);
                    dst[c] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
    }
    encoded_full.clone()
}

/// Re-blends the last spot enhance at new settings from the cached raw
/// region — instant, no model run. Returns the same patch payload as
/// `invoke_spot_enhance_with_mask_def`.
#[tauri::command]
pub async fn respot_enhance(
    patch_id: String,
    strength: Option<f32>,
    texture: Option<f32>,
    grain: Option<f32>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let spot_handle = state.spot_raw.clone();
    tokio::task::spawn_blocking(move || {
        let guard = spot_handle.lock().unwrap();
        let Some(cache) = guard.as_ref().filter(|c| c.patch_id == patch_id) else {
            return Err(
                "This edit's raw result is no longer cached — run Enhance again.".to_string(),
            );
        };
        log::info!("[spot] re-blend patch {} from cache", patch_id);
        let mut base = cache.encoded_full.clone();
        let result = composite_spot_blend(
            &mut base,
            &cache.raw,
            &cache.original,
            &cache.crop_mask,
            cache.crop_origin,
            strength.unwrap_or(0.7),
            texture.unwrap_or(0.0),
            grain.unwrap_or(0.0),
        );
        encode_patch_result(&result, cache.is_linear, &cache.full_mask)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn invoke_spot_enhance_with_mask_def(
    path: String,
    patch_definition: AiPatchDefinition,
    current_adjustments: Value,
    task: String,
    strength: Option<f32>,
    texture: Option<f32>,
    grain: Option<f32>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let _ = path;
    log::info!(
        "[spot] enhance requested: task={} strength={:?} texture={:?} grain={:?} patch={}",
        task,
        strength,
        texture,
        grain,
        patch_definition.id
    );
    let task_type = match crate::model_registry::TaskType::parse(&task) {
        Some(
            t @ (crate::model_registry::TaskType::Upscale
            | crate::model_registry::TaskType::Deblur
            | crate::model_registry::TaskType::Restore),
        ) => t,
        _ => return Err(format!("'{}' is not an enhancement task", task)),
    };

    let mut source_image_adjustments = current_adjustments.clone();
    if let Some(patches) = source_image_adjustments
        .get_mut("aiPatches")
        .and_then(|v| v.as_array_mut())
    {
        patches.retain(|p| p.get("id").and_then(|id| id.as_str()) != Some(&patch_definition.id));
    }

    let (base_image, _) = get_full_image_for_processing(&state)?;
    let source_image = composite_patches_on_image(&base_image, &source_image_adjustments)
        .map_err(|e| format!("Failed to prepare source image: {}", e))?;

    let (img_w, img_h) = source_image.dimensions();
    let mask_def_for_generation = MaskDefinition {
        id: patch_definition.id.clone(),
        name: patch_definition.name.clone(),
        visible: patch_definition.visible,
        invert: patch_definition.invert,
        opacity: 100.0,
        grow: 0.0,
        feather: 0.0,
        adjustments: serde_json::Value::Null,
        sub_masks: patch_definition.sub_masks.clone(),
    };

    let warped_image = resolve_warped_image_for_masks(
        &state,
        &current_adjustments,
        std::slice::from_ref(&mask_def_for_generation),
    );

    let mask_bitmap = generate_mask_bitmap(
        &mask_def_for_generation,
        img_w,
        img_h,
        1.0,
        (0.0, 0.0),
        warped_image.as_deref(),
    )
    .ok_or("Failed to generate mask bitmap for spot enhance")?;

    let mask_dynamic = DynamicImage::ImageLuma8(mask_bitmap);
    let unwarped_dynamic =
        apply_unwarp_geometry(Cow::Borrowed(&mask_dynamic), &current_adjustments).into_owned();
    let mask_bitmap = unwarped_dynamic.to_luma8();

    if mask_bitmap.pixels().filter(|p| p[0] > 0).count() == 0 {
        return Err(
            "The selection is empty — brush over the area to enhance, then try again.".to_string(),
        );
    }

    let (patch_rgba, patch_is_gamma) = run_spot_enhance_patch(
        &source_image,
        &mask_bitmap,
        task_type,
        &task,
        strength.unwrap_or(0.7),
        texture.unwrap_or(0.0),
        grain.unwrap_or(0.0),
        &patch_definition.id,
        &app_handle,
        &state,
    )
    .await
    .inspect_err(|e| log::error!("[spot] enhance FAILED: {}", e))?;

    encode_patch_result(&patch_rgba, patch_is_gamma, &mask_bitmap)
}

#[cfg(test)]
mod fill_component_tests {
    use super::*;

    /// Scattered selections must decompose into separate blobs — one box
    /// around everything is exactly the failure mode that made color-key
    /// fills repaint the whole photo.
    #[test]
    fn scattered_mask_splits_into_components() {
        let mut mask = GrayImage::new(400, 300);
        // Big blob: 120x80 at (20,20). Speck: 6x6 at (350,250).
        for y in 20..100 {
            for x in 20..140 {
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }
        for y in 250..256 {
            for x in 350..356 {
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let (labels, comps) = mask_components(&mask, 127);
        assert_eq!(comps.len(), 2, "two disjoint blobs expected");
        let big = comps.iter().max_by_key(|c| c.area).unwrap();
        let small = comps.iter().min_by_key(|c| c.area).unwrap();
        assert_eq!(
            (big.min_x, big.min_y, big.max_x, big.max_y),
            (20, 20, 139, 99)
        );
        assert_eq!(big.area, 120 * 80);
        assert!(big.span() > 96, "big blob goes to diffusion");
        assert!(small.span() <= 96, "speck goes to the LaMa spot path");
        // The solid big blob passes the density gate; lace must not.
        let bbox = (big.max_x - big.min_x + 1) * (big.max_y - big.min_y + 1);
        assert!(big.area as f32 / bbox as f32 >= 0.35, "solid blob is dense");
        // Labels separate the blobs.
        assert_ne!(labels[25 * 400 + 25], labels[252 * 400 + 352]);
        assert_eq!(labels[0], 0, "background unlabeled");
    }

    /// A big-but-lacy blob (scattered speckle bridged into one component)
    /// must fail the solidity gate — diffusion repaints lace as blotches;
    /// only solid object-like regions earn a diffusion patch.
    #[test]
    fn lacy_blob_fails_the_density_gate() {
        let mut mask = GrayImage::new(400, 400);
        // Sparse 4px dots on a 16px grid across a 300x300 area, connected
        // by thin 1px bridges so they form ONE component.
        for gy in 0..19 {
            for gx in 0..19 {
                let (bx, by) = (20 + gx * 15, 20 + gy * 15);
                for y in by..by + 4 {
                    for x in bx..bx + 4 {
                        mask.put_pixel(x, y, image::Luma([255]));
                    }
                }
                // bridge to the right neighbor
                if gx < 18 {
                    for x in bx + 4..bx + 15 {
                        mask.put_pixel(x, by + 1, image::Luma([255]));
                    }
                }
                if gy < 18 && gx == 0 {
                    for y in by + 4..by + 15 {
                        mask.put_pixel(bx + 1, y, image::Luma([255]));
                    }
                }
            }
        }
        let (_, comps) = mask_components(&mask, 127);
        assert_eq!(comps.len(), 1, "bridged lace is one component");
        let c = &comps[0];
        assert!(c.span() > 96, "it is large");
        let bbox = (c.max_x - c.min_x + 1) * (c.max_y - c.min_y + 1);
        assert!(
            (c.area as f32 / bbox as f32) < 0.35,
            "lace density {:.2} must fail the solidity gate",
            c.area as f32 / bbox as f32
        );
        assert!(
            !component_goes_to_diffusion(c, false, false),
            "ordinary repair keeps lacy masks on the texture-fill path"
        );
        assert!(
            component_goes_to_diffusion(c, false, true),
            "Reconstruct must keep lacy clipped masks on the prompt-conditioned engine path"
        );
        assert!(
            !component_goes_to_diffusion(c, true, true),
            "explicit fast mode remains LaMa-only"
        );
    }

    #[test]
    fn reconstruct_consolidation_merges_lacy_clipped_masks() {
        let mut mask = GrayImage::new(200, 200);
        for gy in 0..10 {
            for gx in 0..10 {
                let (bx, by) = (50 + gx * 10, 50 + gy * 10);
                for y in by..by + 3 {
                    for x in bx..bx + 3 {
                        mask.put_pixel(x, y, image::Luma([255]));
                    }
                }
            }
        }

        let (_, before) = mask_components(&mask, 127);
        consolidate_reconstruct_mask(&mut mask, false);
        let (_, after) = mask_components(&mask, 127);

        assert!(
            after.len() < before.len(),
            "solidify should merge clipped speckle into fewer regions"
        );
        assert!(
            mask.get_pixel(55, 55)[0] > 127,
            "inter-dot gap should be selected after Reconstruct consolidation"
        );
    }

    #[test]
    fn reconstruct_consolidation_preserves_negative_refinements_when_requested() {
        let mut mask = GrayImage::new(120, 120);
        for gy in 0..8 {
            for gx in 0..8 {
                let (bx, by) = (20 + gx * 10, 20 + gy * 10);
                for y in by..by + 3 {
                    for x in bx..bx + 3 {
                        mask.put_pixel(x, y, image::Luma([255]));
                    }
                }
            }
        }

        let before = mask.clone();
        consolidate_reconstruct_mask(&mut mask, true);
        assert_eq!(
            mask.as_raw(),
            before.as_raw(),
            "backend consolidation should not override eraser/subtractive refinements"
        );
    }

    #[test]
    fn reconstruct_materialization_turns_confidence_into_opacity() {
        let mut mask = GrayImage::new(8, 1);
        mask.put_pixel(0, 0, image::Luma([0]));
        mask.put_pixel(1, 0, image::Luma([1]));
        mask.put_pixel(2, 0, image::Luma([32]));
        mask.put_pixel(3, 0, image::Luma([127]));
        mask.put_pixel(4, 0, image::Luma([128]));

        let (selected, strong) = materialize_reconstruct_mask(&mut mask);

        assert_eq!(selected, 4);
        assert_eq!(strong, 4);
        assert_eq!(mask.get_pixel(0, 0)[0], 0);
        for x in 1..=4 {
            assert_eq!(
                mask.get_pixel(x, 0)[0],
                255,
                "selected confidence pixel {x} should become full opacity"
            );
        }
    }

    #[test]
    fn reconstruct_routing_keeps_components_independent() {
        let large = MaskComponent {
            id: 1,
            min_x: 100,
            min_y: 100,
            max_x: 260,
            max_y: 220,
            area: 12_000,
        };
        let nearby_small = MaskComponent {
            id: 2,
            min_x: 310,
            min_y: 145,
            max_x: 322,
            max_y: 157,
            area: 40,
        };

        assert!(component_goes_to_diffusion(&large, false, true));
        assert!(
            !component_goes_to_diffusion(&nearby_small, false, true),
            "tiny cleanup specks should stay fast instead of being merged into the large prompt path"
        );
    }

    #[test]
    fn large_flux_reconstruct_skips_flat_prompt_seed() {
        assert!(!should_seed_reconstruct_engine_canvas(
            480,
            ReconstructStrategy::HighlightSky,
            crate::comfy_engine::FillKind::Flux
        ));
        assert!(should_seed_reconstruct_engine_canvas(
            64,
            ReconstructStrategy::HighlightSky,
            crate::comfy_engine::FillKind::Flux
        ));
        assert!(should_seed_reconstruct_engine_canvas(
            480,
            ReconstructStrategy::GenericContext,
            crate::comfy_engine::FillKind::Flux
        ));
    }

    #[test]
    fn reconstruct_composite_mask_expands_and_softens_edges() {
        let mut mask = GrayImage::new(80, 80);
        for y in 30..50 {
            for x in 30..50 {
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let blend = reconstruct_composite_mask(&mask, false);
        let original_selected = mask.pixels().filter(|p| p[0] > 0).count();
        let blend_selected = blend.pixels().filter(|p| p[0] > 0).count();
        let blend_strong = blend.pixels().filter(|p| p[0] > 127).count();

        assert!(
            blend_selected > original_selected,
            "composite mask should use the generated seam outside the clipped core"
        );
        assert!(
            blend_selected > blend_strong,
            "composite mask should include a soft edge, not only full-opacity pixels"
        );
    }

    #[test]
    fn empty_reconstruct_uses_internal_prompt() {
        let prompt = effective_reconstruct_prompt("", true, ReconstructAutoHint::Generic);
        assert!(
            prompt.contains("seamlessly continue"),
            "empty Reconstruct should still be semantically conditioned"
        );
        let sky_prompt = effective_reconstruct_prompt("", true, ReconstructAutoHint::HighlightSky);
        assert!(
            sky_prompt.contains("cloud detail"),
            "sky-like clipped highlights should get a more useful prompt than generic continuation"
        );
        assert_eq!(
            effective_reconstruct_prompt("storm clouds", true, ReconstructAutoHint::HighlightSky)
                .as_ref(),
            "storm clouds",
            "user prompts must pass through unchanged"
        );
        assert_eq!(
            effective_reconstruct_prompt("", false, ReconstructAutoHint::Generic).as_ref(),
            "",
            "non-Reconstruct empty prompts keep existing behavior"
        );
    }

    #[test]
    fn reconstruct_auto_hint_detects_large_upper_highlight_as_sky() {
        let mut img = RgbaImage::from_pixel(200, 120, Rgba([180, 205, 215, 255]));
        let mut mask = GrayImage::new(200, 120);
        for y in 18..70 {
            for x in 48..152 {
                img.put_pixel(x, y, Rgba([252, 252, 250, 255]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        assert_eq!(
            infer_reconstruct_auto_hint_rgba(&img, &mask),
            ReconstructAutoHint::HighlightSky
        );
    }

    #[test]
    fn promptless_reconstruct_gets_blend_tone_matching() {
        assert!(reconstruct_tone_strength(true, true) > 0.0);
        assert_eq!(reconstruct_tone_strength(true, false), 0.0);
        assert_eq!(reconstruct_tone_strength(false, false), 0.15);
    }

    #[test]
    fn reconstruct_prefill_scrubs_clipped_conditioning_pixels() {
        let mut crop = RgbaImage::from_pixel(48, 48, Rgba([150, 190, 210, 255]));
        let mut mask = GrayImage::new(48, 48);
        for y in 16..32 {
            for x in 16..32 {
                crop.put_pixel(x, y, Rgba([255, 255, 255, 255]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let changed = prefill_reconstruct_conditioning(&mut crop, &mask);
        let center = crop.get_pixel(24, 24);

        assert_eq!(changed, 16 * 16);
        assert!(
            center[0] < 220 && center[1] < 230 && center[2] < 240,
            "center should be scrubbed from clipped white into surrounding context, got {:?}",
            center
        );
    }

    #[test]
    fn reconstruct_engine_seed_honors_blue_sky_prompt() {
        let mut crop = RgbaImage::from_pixel(64, 64, Rgba([230, 230, 220, 255]));
        let mut mask = GrayImage::new(64, 64);
        for y in 18..46 {
            for x in 18..46 {
                crop.put_pixel(x, y, Rgba([250, 250, 250, 255]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let changed = seed_reconstruct_engine_canvas(&mut crop, &mask, "blue sky");
        let center = crop.get_pixel(32, 32);

        assert_eq!(changed, 28 * 28);
        assert!(
            center[2] > center[0] + 45 && center[1] > center[0] + 25,
            "blue sky prompt should seed the engine canvas blue before Flux sees it, got {:?}",
            center
        );
    }

    #[test]
    fn reconstruct_collapse_guard_rejects_flat_gray_large_fill() {
        let (w, h) = (96u32, 96u32);
        let filled = RgbaImage::from_pixel(w, h, Rgba([186, 187, 184, 255]));
        let mask = GrayImage::from_pixel(w, h, image::Luma([255]));

        assert!(
            reconstruct_output_looks_collapsed(&filled, &mask, ""),
            "large flat gray reconstruct output should be treated as collapsed"
        );
    }

    #[test]
    fn reconstruct_collapse_guard_rejects_blue_sky_prompt_that_returns_gray() {
        let (w, h) = (96u32, 96u32);
        let filled = RgbaImage::from_pixel(w, h, Rgba([170, 176, 181, 255]));
        let mask = GrayImage::from_pixel(w, h, image::Luma([255]));

        assert!(
            reconstruct_output_looks_collapsed(&filled, &mask, "blue sky"),
            "blue sky prompt should not accept a neutral gray fill"
        );
    }

    #[test]
    fn reconstruct_collapse_guard_keeps_colored_textured_fill() {
        let (w, h) = (96u32, 96u32);
        let mut filled = RgbaImage::new(w, h);
        let mask = GrayImage::from_pixel(w, h, image::Luma([255]));
        for y in 0..h {
            for x in 0..w {
                let v = ((x + y) % 21) as u8;
                filled.put_pixel(x, y, Rgba([90 + v, 155 + v, 218 + v, 255]));
            }
        }

        assert!(
            !reconstruct_output_looks_collapsed(&filled, &mask, "blue sky"),
            "colored, varied sky-like output should be accepted"
        );
    }

    #[test]
    fn reconstruct_fallback_renderer_adds_subtle_sky_variation() {
        let (w, h) = (160u32, 120u32);
        let mut seeded = RgbaImage::from_pixel(w, h, Rgba([190, 208, 216, 255]));
        let mut mask = GrayImage::new(w, h);
        for y in 24..96 {
            for x in 32..128 {
                seeded.put_pixel(x, y, Rgba([190, 208, 216, 255]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let rendered = render_reconstruct_fallback_crop(
            &seeded,
            &mask,
            "bright backlit cloud detail and pale sky",
            ReconstructStrategy::HighlightSky,
        );
        let stats = reconstruct_region_stats(&rendered, &mask).unwrap();

        assert!(
            stats.luma_std > 2.5,
            "fallback should add subtle haze/cloud variation, got luma std {:.2}",
            stats.luma_std
        );
        assert!(
            stats.blue_minus_red() > 14.0,
            "fallback should remain sky-like, got b-r {:.2}",
            stats.blue_minus_red()
        );
    }

    #[test]
    fn reconstruct_fallback_carries_surrounding_sky_texture_into_mask() {
        let (w, h) = (192u32, 144u32);
        let mut seeded = RgbaImage::new(w, h);
        let mut mask = GrayImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let wave = (((x as f32 / 13.0).sin() + (y as f32 / 17.0).cos()) * 9.0) as i16;
                let cloud_band = if (y > 28 && y < 50) || (x > 142 && y > 82) {
                    24
                } else {
                    0
                };
                let base = 184i16 + (y as i16 / 8) + wave + cloud_band;
                seeded.put_pixel(
                    x,
                    y,
                    Rgba([
                        base.clamp(0, 255) as u8,
                        (base + 18).clamp(0, 255) as u8,
                        (base + 30).clamp(0, 255) as u8,
                        255,
                    ]),
                );
            }
        }
        for y in 36..116 {
            for x in 42..150 {
                seeded.put_pixel(x, y, Rgba([232, 233, 230, 255]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let rendered =
            render_reconstruct_fallback_crop(&seeded, &mask, "", ReconstructStrategy::HighlightSky);
        let stats = reconstruct_region_stats(&rendered, &mask).unwrap();
        let mut upper = 0.0f64;
        let mut lower = 0.0f64;
        let mut upper_count = 0.0f64;
        let mut lower_count = 0.0f64;
        for y in 48..104 {
            for x in 62..130 {
                let p = rendered.get_pixel(x, y);
                let luma = 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
                if y < 76 {
                    upper += luma;
                    upper_count += 1.0;
                } else {
                    lower += luma;
                    lower_count += 1.0;
                }
            }
        }

        assert!(
            stats.luma_std > 3.6,
            "texture-guided fallback should stay meaningfully above a flat tone, got luma std {:.2}",
            stats.luma_std
        );
        assert!(
            ((upper / upper_count) - (lower / lower_count)).abs() > 2.0,
            "surrounding tonal structure should carry through the interior"
        );
    }

    #[test]
    fn reconstruct_response_metadata_appends_attempt_without_losing_previous() {
        let current = serde_json::json!({
            "aiPatches": [{
                "id": "patch-a",
                "patchData": {
                    "color": "old-color",
                    "mask": "old-mask",
                    "encoding": "gamma"
                }
            }]
        });
        let active = serde_json::json!({
            "color": "new-color",
            "mask": "new-mask",
            "encoding": "gamma"
        });
        let payload = attach_reconstruct_response_metadata(
            active,
            ReconstructResponseInputs {
                current_adjustments: &current,
                patch_id: "patch-a",
                prompt: "",
                debug_run_id: "patch-a-1",
                debug_dir: Some("/tmp/debug".to_string()),
                auto_hint: ReconstructAutoHint::HighlightSky,
                effective_prompt: "auto sky prompt",
                active_kind: "ai",
            },
        );
        let variants = payload
            .get("reconstructVariants")
            .and_then(Value::as_array)
            .unwrap();

        assert_eq!(
            payload
                .get("reconstructActiveVariantId")
                .and_then(Value::as_str),
            Some("attempt-2")
        );
        assert_eq!(variants.len(), 2);
        assert_eq!(
            variants[0].get("color").and_then(Value::as_str),
            Some("old-color")
        );
        assert!(
            variants.iter().all(|v| v
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .starts_with("attempt-")),
            "normal patch history should contain user-facing attempts only"
        );
    }

    #[test]
    fn reconstruct_quality_gate_rejects_no_prompt_sky_washout() {
        let (w, h) = (160u32, 160u32);
        let fallback = RgbaImage::from_pixel(w, h, Rgba([191, 209, 215, 255]));
        let ai = RgbaImage::from_pixel(w, h, Rgba([235, 235, 233, 255]));
        let mask = GrayImage::from_pixel(w, h, image::Luma([255]));

        assert!(
            reconstruct_ai_result_lost_to_fallback(
                &ai,
                &fallback,
                &mask,
                "bright backlit cloud detail and pale sky continuing naturally",
                ReconstructStrategy::HighlightSky
            ),
            "no-prompt sky Reconstruct should reject AI output that washes the seeded sky candidate back to white-gray"
        );
    }

    #[test]
    fn reconstruct_strategy_keeps_shadow_fallback_dark() {
        let (w, h) = (140u32, 120u32);
        let mut seeded = RgbaImage::from_pixel(w, h, Rgba([34, 38, 42, 255]));
        let mut mask = GrayImage::new(w, h);
        for y in 24..96 {
            for x in 28..112 {
                seeded.put_pixel(x, y, Rgba([18, 20, 22, 255]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let strategy = ReconstructStrategy::from_prompt_and_stats("", true, &seeded, &mask);
        let rendered = render_reconstruct_fallback_crop(
            &seeded,
            &mask,
            "",
            ReconstructStrategy::ShadowTexture,
        );
        let stats = reconstruct_region_stats(&rendered, &mask).unwrap();

        assert_eq!(strategy, ReconstructStrategy::ShadowTexture);
        assert!(
            stats.mean_luma < 55.0,
            "shadow reconstruct should stay dark, got luma {:.1}",
            stats.mean_luma
        );
    }

    #[test]
    fn reconstruct_generic_fallback_does_not_force_sky_color() {
        let (w, h) = (140u32, 120u32);
        let seeded = RgbaImage::from_pixel(w, h, Rgba([128, 112, 92, 255]));
        let mut mask = GrayImage::new(w, h);
        for y in 24..96 {
            for x in 28..112 {
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }

        let rendered = render_reconstruct_fallback_crop(
            &seeded,
            &mask,
            "",
            ReconstructStrategy::GenericContext,
        );
        let stats = reconstruct_region_stats(&rendered, &mask).unwrap();

        assert!(
            stats.blue_minus_red() < 0.0,
            "generic context should keep warm local color instead of forcing blue, got b-r {:.1}",
            stats.blue_minus_red()
        );
    }

    #[test]
    fn engine_orientation_round_trips_reconstruct_crops() {
        let mut crop = RgbaImage::new(3, 2);
        for y in 0..2 {
            for x in 0..3 {
                crop.put_pixel(x, y, Rgba([(x * 50) as u8, (y * 80) as u8, 17, 255]));
            }
        }

        for steps in 0..4 {
            for flip_h in [false, true] {
                for flip_v in [false, true] {
                    let oriented = orient_rgba_for_engine(&crop, steps, flip_h, flip_v);
                    let restored = deorient_rgba_from_engine(&oriented, steps, flip_h, flip_v);
                    assert_eq!(restored.dimensions(), crop.dimensions());
                    assert_eq!(restored, crop);
                }
            }
        }
    }

    #[test]
    fn engine_orientation_keeps_mask_aligned_with_image() {
        let mut mask = GrayImage::new(4, 3);
        mask.put_pixel(0, 0, image::Luma([255]));
        mask.put_pixel(3, 2, image::Luma([128]));

        let oriented = orient_gray_for_engine(&mask, 0, true, true);
        assert_eq!(oriented.get_pixel(3, 2)[0], 255);
        assert_eq!(oriented.get_pixel(0, 0)[0], 128);
    }

    /// The "placed-in blotch" fingerprint is a fill that is brighter/
    /// off-tone and smoother than its surroundings. Harmonization must
    /// pull the patch's tone toward the ring and close the noise gap.
    #[test]
    fn harmonization_matches_ring_tone_and_noise() {
        let (w, h) = (200u32, 200u32);
        // Original: mid-gray with visible noise everywhere.
        let mut original = RgbaImage::new(w, h);
        for (i, p) in original.pixels_mut().enumerate() {
            let n = crate::enhancement::grain_noise(i as u32) * 18.0;
            let v = (120.0 + n).clamp(0.0, 255.0) as u8;
            *p = Rgba([v, v, v, 255]);
        }
        // Mask: centered 60x60 blob.
        let mut mask = GrayImage::new(w, h);
        for y in 70..130 {
            for x in 70..130 {
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }
        // Fill: perfectly smooth and 10 levels too bright inside the mask.
        let mut filled = original.clone();
        for y in 70..130 {
            for x in 70..130 {
                filled.put_pixel(x, y, Rgba([130, 130, 130, 255]));
            }
        }

        let noise_before = region_fine_noise(&filled, &mask, true);
        harmonize_patch(&original, &mut filled, &mask, 1.0);

        let ring_zone = dilate_mask(&mask, 16);
        let mut ring = GrayImage::new(w, h);
        for (x, y, p) in ring_zone.enumerate_pixels() {
            if p[0] > 127 && mask.get_pixel(x, y)[0] <= 127 {
                ring.put_pixel(x, y, image::Luma([255]));
            }
        }
        let ring_mean = region_mean(&original, &ring, true).unwrap();
        let fill_mean = region_mean(&filled, &mask, true).unwrap();
        assert!(
            (fill_mean[0] - ring_mean[0]).abs() < 3.0,
            "fill mean {} must land near ring mean {}",
            fill_mean[0],
            ring_mean[0]
        );

        let noise_after = region_fine_noise(&filled, &mask, true);
        let ring_noise = region_fine_noise(&original, &ring, true);
        assert!(
            noise_before < ring_noise * 0.2,
            "test setup: fill starts smooth"
        );
        assert!(
            noise_after > ring_noise * 0.5,
            "fill noise {noise_after:.4} must approach ring noise {ring_noise:.4}"
        );
    }
}

/// Strips display-orientation parameters from value-derived sub-masks so
/// their bitmaps stay in full-image space (see the comment at the call
/// site in `invoke_generative_replace_with_mask_def`).
pub(crate) fn neutralize_display_orientation(sub_masks: &mut [crate::mask_generation::SubMask]) {
    for sm in sub_masks {
        if matches!(sm.mask_type.as_str(), "clipped" | "color" | "luminance")
            && let Some(params) = sm.parameters.as_object_mut()
        {
            params.insert("rotation".into(), serde_json::json!(0.0));
            params.insert("flipHorizontal".into(), serde_json::json!(false));
            params.insert("flipVertical".into(), serde_json::json!(false));
            params.insert("orientationSteps".into(), serde_json::json!(0));
        }
    }
}

#[cfg(test)]
mod fill_mask_orientation_tests {
    use crate::mask_generation::{MaskDefinition, SubMask, generate_mask_bitmap};
    use image::DynamicImage;

    /// A clipped mask on a FLIPPED photo must select the bright region
    /// where it actually is in the source image — not its mirror. This is
    /// the "found the highlights, edited the foliage" bug.
    #[test]
    fn clipped_fill_mask_lands_on_highlights_not_their_mirror() {
        let (w, h) = (200u32, 150u32);
        let mut img = image::Rgb32FImage::from_pixel(w, h, image::Rgb([0.25f32, 0.25, 0.25]));
        for y in 20..50 {
            for x in 30..70 {
                img.put_pixel(x, y, image::Rgb([0.99f32, 0.99, 0.99]));
            }
        }
        let reference = DynamicImage::ImageRgb32F(img);

        let sub_mask: SubMask = serde_json::from_value(serde_json::json!({
            "id": "t", "type": "clipped", "visible": true, "mode": "additive",
            "parameters": {
                "whiteThreshold": 90, "blackThreshold": 0,
                "feather": 0, "grow": 0, "clean": 0, "solidify": 0,
                "flipHorizontal": true, "flipVertical": true,
                "rotation": 0.0, "orientationSteps": 0
            }
        }))
        .unwrap();
        let mut sub_masks = vec![sub_mask];
        super::neutralize_display_orientation(&mut sub_masks);
        let def = MaskDefinition {
            id: "t".into(),
            name: "t".into(),
            visible: true,
            invert: false,
            opacity: 100.0,
            grow: 0.0,
            feather: 0.0,
            adjustments: serde_json::Value::Null,
            sub_masks,
        };
        let mask = generate_mask_bitmap(&def, w, h, 1.0, (0.0, 0.0), Some(&reference))
            .expect("mask generated");
        let (mut sx, mut sy, mut n) = (0f64, 0f64, 0u64);
        for (x, y, p) in mask.enumerate_pixels() {
            if p[0] > 127 {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
        }
        assert!(n > 0, "clipped mask selected nothing");
        let (cx, cy) = (sx / n as f64, sy / n as f64);
        assert!(
            (30.0..70.0).contains(&cx) && (20.0..50.0).contains(&cy),
            "mask centroid ({cx:.0},{cy:.0}) not on the bright region (30-70, 20-50) — \
             landed at the mirror position"
        );
    }
}

#[cfg(test)]
mod fill_preflight_tests {
    use super::fill_warning_for;

    /// The DSC08310 sky gap: compact, well-sized, textured surroundings.
    /// This one worked, so it must NOT warn.
    #[test]
    fn compact_region_with_textured_surroundings_is_clean() {
        assert!(fill_warning_for(0.55, 2495, 1149, 250.7, 70.8, false).is_none());
    }

    /// The Oculus ribs: lacy over a huge span. Must warn about lost detail.
    #[test]
    fn lacy_wide_selection_warns_about_structure() {
        let w = fill_warning_for(0.20, 6459, 1500, 64.5, 22.1, false).expect("should warn");
        assert!(w.contains("fine detail"), "unexpected warning: {w}");
    }

    /// The rig photo: compact but ringed by more blown wash, no prompt.
    #[test]
    fn featureless_surroundings_warn_when_promptless() {
        let w = fill_warning_for(0.47, 1584, 745, 221.0, 4.3, false).expect("should warn");
        assert!(w.contains("featureless"), "unexpected warning: {w}");
    }

    /// Same region WITH a prompt: the flatness warning no longer applies,
    /// but genuinely textureless surroundings still get the softer note.
    #[test]
    fn prompt_changes_the_featureless_advice() {
        let w = fill_warning_for(0.47, 1584, 745, 221.0, 4.3, true);
        assert!(w.is_some_and(|m| m.contains("almost no texture")));
        assert!(fill_warning_for(0.47, 1584, 745, 221.0, 20.0, true).is_none());
    }

    #[test]
    fn tiny_canvas_region_warns() {
        let w = fill_warning_for(0.8, 300, 90, 128.0, 30.0, true).expect("should warn");
        assert!(w.contains("too small"), "unexpected warning: {w}");
    }
}

#[cfg(test)]
mod clone_stamp_tests {
    use super::clone_offset_copy;
    use image::{GrayImage, Luma, Rgba, RgbaImage};

    fn gradient(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.put_pixel(x, y, Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]));
            }
        }
        img
    }

    /// Masked pixels take their colour from the source offset; everything
    /// outside the mask is untouched.
    #[test]
    fn copies_from_the_offset_and_leaves_the_rest_alone() {
        let img = gradient(64, 64);
        let mut mask = GrayImage::new(64, 64);
        for y in 20..30 {
            for x in 20..30 {
                mask.put_pixel(x, y, Luma([255]));
            }
        }
        let out = clone_offset_copy(&img, &mask, 10, 5);
        assert_eq!(out.get_pixel(25, 25).0[..3], img.get_pixel(35, 30).0[..3]);
        assert_eq!(out.get_pixel(5, 5), img.get_pixel(5, 5));
        assert_eq!(out.get_pixel(40, 40), img.get_pixel(40, 40));
    }

    /// A source point outside the frame clamps to the edge instead of
    /// leaving a hole.
    #[test]
    fn clamps_source_at_the_border() {
        let img = gradient(32, 32);
        let mut mask = GrayImage::new(32, 32);
        for y in 0..4 {
            for x in 0..4 {
                mask.put_pixel(x, y, Luma([255]));
            }
        }
        let out = clone_offset_copy(&img, &mask, -20, -20);
        assert_eq!(out.get_pixel(0, 0).0[..3], img.get_pixel(0, 0).0[..3]);
        assert_eq!(out.get_pixel(3, 3).0[..3], img.get_pixel(0, 0).0[..3]);
    }

    #[test]
    fn empty_mask_is_a_no_op() {
        let img = gradient(16, 16);
        let mask = GrayImage::new(16, 16);
        let out = clone_offset_copy(&img, &mask, 4, 4);
        assert_eq!(out.as_raw(), img.as_raw());
    }
}

#[cfg(test)]
mod auto_source_tests {
    use super::{auto_clone_offset, mask_bounds};
    use image::{GrayImage, Luma, Rgba, RgbaImage};

    /// Horizontally periodic scene: a source one period away is a perfect
    /// match, so a working picker should land near a multiple of it.
    fn periodic_scene(period: u32) -> RgbaImage {
        let mut img = RgbaImage::new(600, 400);
        for y in 0..400 {
            for x in 0..600 {
                let phase = (x % period) as f32 / period as f32 * std::f32::consts::TAU;
                let v = (128.0 + 60.0 * phase.sin() + 20.0 * (y as f32 / 19.0).sin())
                    .clamp(0.0, 255.0) as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    fn spot(x0: u32, y0: u32, x1: u32, y1: u32) -> GrayImage {
        let mut m = GrayImage::new(600, 400);
        for y in y0..=y1 {
            for x in x0..=x1 {
                m.put_pixel(x, y, Luma([255]));
            }
        }
        m
    }

    #[test]
    fn picks_a_source_whose_surroundings_match() {
        let period = 60u32;
        let img = periodic_scene(period);
        let m = spot(280, 180, 340, 240);
        let b = mask_bounds(&m).expect("bounds");
        let (dx, dy) = auto_clone_offset(&img, &m, b);

        // Score the chosen offset against a deliberately mismatched one,
        // using the same ring-agreement measure the picker optimises.
        let ring_err = |ox: i32, oy: i32| {
            let (mut e, mut n) = (0.0f32, 0u32);
            for y in 170..=250i32 {
                for x in 270..=350i32 {
                    if (280..=340).contains(&x) && (180..=240).contains(&y) {
                        continue;
                    }
                    let g = |px: i32, py: i32| img.get_pixel(px as u32, py as u32)[0] as f32;
                    e += (g(x, y) - g(x + ox, y + oy)).abs();
                    n += 1;
                }
            }
            e / n as f32
        };
        let chosen = ring_err(dx, dy);
        let half_period_off = ring_err(period as i32 * 2 + period as i32 / 2, 0);
        assert!(
            chosen < half_period_off * 0.5,
            "chosen offset ({dx},{dy}) err {chosen:.2} should beat a mismatched source err {half_period_off:.2}"
        );
    }

    /// The source must not sit on the hole being repaired, or the heal
    /// would copy the damage back over itself.
    #[test]
    fn never_samples_the_area_being_repaired() {
        let img = periodic_scene(60);
        let m = spot(280, 180, 340, 240);
        let b = mask_bounds(&m).expect("bounds");
        let (dx, dy) = auto_clone_offset(&img, &m, b);
        let mut overlap = 0;
        for y in 180..=240i32 {
            for x in 280..=340i32 {
                let (sx, sy) = (x + dx, y + dy);
                if (0..600).contains(&sx)
                    && (0..400).contains(&sy)
                    && m.get_pixel(sx as u32, sy as u32)[0] > 0
                {
                    overlap += 1;
                }
            }
        }
        assert_eq!(overlap, 0, "source at ({dx},{dy}) overlaps the mask");
    }

    #[test]
    fn offset_is_never_degenerate() {
        let img = periodic_scene(60);
        let m = spot(280, 180, 340, 240);
        let b = mask_bounds(&m).expect("bounds");
        let (dx, dy) = auto_clone_offset(&img, &m, b);
        assert!(dx != 0 || dy != 0, "a zero offset would heal from itself");
    }

    #[test]
    fn bounds_are_the_extent_of_the_mask() {
        let m = spot(10, 20, 30, 45);
        assert_eq!(mask_bounds(&m), Some((10, 20, 30, 45)));
        assert_eq!(mask_bounds(&GrayImage::new(40, 40)), None);
    }
}

#[cfg(test)]
mod tone_match_tests {
    use super::{match_tone, tone_stats};
    use image::{Rgba, RgbaImage};

    /// Stand-in for generated sky: dark, contrasty, cloud-like variation.
    fn generated() -> RgbaImage {
        let mut img = RgbaImage::new(120, 120);
        for y in 0..120 {
            for x in 0..120 {
                let n = ((x as f32 * 12.9898 + y as f32 * 78.233).sin() * 43758.547).fract();
                let v = (110.0 + n.abs() * 80.0 - 40.0).clamp(0.0, 255.0) as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        img
    }

    const FULL: (u32, u32, u32, u32) = (0, 0, 119, 119);

    #[test]
    fn full_strength_lands_on_the_target_tone() {
        let mut img = generated();
        let before = tone_stats(&img, FULL, |_, _| true).unwrap();
        match_tone(&mut img, FULL, before, ([203.0, 203.0, 203.0], 25.0), 1.0);
        let after = tone_stats(&img, FULL, |_, _| true).unwrap();
        let mean = after.0.iter().sum::<f32>() / 3.0;
        assert!(
            (mean - 203.0).abs() < 6.0,
            "mean landed at {mean:.1}, expected ~203"
        );
        assert!(
            (after.1 - 25.0).abs() < 5.0,
            "std landed at {:.1}, expected ~25",
            after.1
        );
    }

    /// The point of the strength control: partway must be partway, not a
    /// switch between "dark hole" and "washed out".
    #[test]
    fn half_strength_lands_between() {
        let mut img = generated();
        let before = tone_stats(&img, FULL, |_, _| true).unwrap();
        let start = before.0.iter().sum::<f32>() / 3.0;
        match_tone(&mut img, FULL, before, ([203.0, 203.0, 203.0], 25.0), 0.5);
        let mean = tone_stats(&img, FULL, |_, _| true).unwrap().0.iter().sum::<f32>() / 3.0;
        let midpoint = (start + 203.0) / 2.0;
        assert!(
            (mean - midpoint).abs() < 8.0,
            "half strength gave {mean:.1}, expected near the midpoint {midpoint:.1}"
        );
    }

    #[test]
    fn zero_strength_changes_nothing() {
        let mut img = generated();
        let original = img.clone();
        let before = tone_stats(&img, FULL, |_, _| true).unwrap();
        match_tone(&mut img, FULL, before, ([203.0, 203.0, 203.0], 25.0), 0.0);
        assert_eq!(original, img);
    }

    /// Matching must lift the tone without flattening the cloud detail —
    /// the failure this whole feature exists to avoid.
    #[test]
    fn detail_survives_the_match() {
        let mut img = generated();
        let before = tone_stats(&img, FULL, |_, _| true).unwrap();
        match_tone(&mut img, FULL, before, ([203.0, 203.0, 203.0], 25.0), 1.0);
        let after = tone_stats(&img, FULL, |_, _| true).unwrap();
        assert!(
            after.1 > 15.0,
            "contrast collapsed to std {:.1}; the clouds would be gone",
            after.1
        );
    }

    /// Clipped and near-black pixels carry no tone information, so the
    /// reference must be sampled from what the caller selects.
    #[test]
    fn stats_honour_the_selection() {
        let mut img = RgbaImage::from_pixel(60, 60, Rgba([255, 255, 255, 255]));
        for y in 0..30 {
            for x in 0..60 {
                img.put_pixel(x, y, Rgba([100, 100, 100, 255]));
            }
        }
        let only_dark = tone_stats(&img, (0, 0, 59, 59), |_, y| y < 30).unwrap();
        assert!((only_dark.0[0] - 100.0).abs() < 1.0);
        let everything = tone_stats(&img, (0, 0, 59, 59), |_, _| true).unwrap();
        assert!(everything.0[0] > 150.0, "unfiltered stats should include the white half");
    }
}

#[cfg(test)]
mod heal_unit_tests {
    use super::split_heal_units;
    use crate::mask_generation::{SubMask, SubMaskMode};
    use serde_json::json;

    fn brush(lines: serde_json::Value) -> SubMask {
        SubMask {
            id: "sm1".to_string(),
            mask_type: "brush".to_string(),
            visible: true,
            invert: false,
            opacity: 100.0,
            mode: SubMaskMode::Additive,
            parameters: json!({ "lines": lines }),
        }
    }

    /// Two strokes must become two independent units, each carrying only
    /// its own stroke — otherwise every spot would heal the whole mask.
    #[test]
    fn each_stroke_becomes_its_own_unit() {
        let sm = brush(json!([
            { "points": [{"x": 10, "y": 10}], "cloneOffset": {"x": 100, "y": -50} },
            { "points": [{"x": 80, "y": 80}] },
        ]));
        let units = split_heal_units(&[sm]);
        assert_eq!(units.len(), 2, "expected one unit per stroke");

        let first_lines = units[0].0.parameters["lines"].as_array().unwrap();
        assert_eq!(first_lines.len(), 1, "a unit must hold exactly one stroke");
        assert_eq!(first_lines[0]["points"][0]["x"], 10);
        assert_eq!(units[0].1, Some((100.0, -50.0)));

        let second_lines = units[1].0.parameters["lines"].as_array().unwrap();
        assert_eq!(second_lines[0]["points"][0]["x"], 80);
        // No source of its own: inherits the container's.
        assert_eq!(units[1].1, None);
    }

    /// A non-brush selection has no strokes and must survive whole.
    #[test]
    fn a_sub_mask_without_strokes_stays_intact() {
        let sm = SubMask {
            id: "clipped".to_string(),
            mask_type: "clipped".to_string(),
            visible: true,
            invert: false,
            opacity: 100.0,
            mode: SubMaskMode::Additive,
            parameters: json!({ "threshold": 0.9 }),
        };
        let units = split_heal_units(&[sm]);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].1, None);
        assert_eq!(units[0].0.parameters["threshold"], 0.9);
    }

    #[test]
    fn a_partial_offset_is_ignored_rather_than_half_read() {
        let sm = brush(json!([{ "points": [{"x": 1, "y": 1}], "cloneOffset": {"x": 5} }]));
        let units = split_heal_units(&[sm]);
        assert_eq!(units[0].1, None, "an offset missing y must not be used");
    }
}

#[cfg(test)]
mod display_space_tests {
    use super::{display_to_image_point, display_to_image_vector};

    const W: f64 = 7008.0;
    const H: f64 = 4672.0;

    /// With no transforms the mapping is the identity.
    #[test]
    fn identity_without_transforms() {
        let (x, y) = display_to_image_point(3195.0, 2591.0, W, H, 0, false, false, 0.0);
        assert!((x - 3195.0).abs() < 1e-6 && (y - 2591.0).abs() < 1e-6);
    }

    /// The user's actual photo: flipped both ways, so a painted point maps
    /// to the opposite side — this is what put the clone across the frame.
    #[test]
    fn double_flip_mirrors_through_the_centre() {
        let (x, y) = display_to_image_point(3195.0, 2591.0, W, H, 0, true, true, 0.0);
        assert!((x - (W - 3195.0)).abs() < 1e-6, "x was {x}");
        assert!((y - (H - 2591.0)).abs() < 1e-6, "y was {y}");
    }

    /// A direction mirrors without translating: dragging the source right
    /// on a flipped photo means left in image space.
    #[test]
    fn vector_mirrors_but_does_not_translate() {
        let (dx, dy) = display_to_image_vector(1207.0, 255.0, 0, true, true, 0.0);
        assert!((dx + 1207.0).abs() < 1e-6, "dx was {dx}");
        assert!((dy + 255.0).abs() < 1e-6, "dy was {dy}");
    }

    /// Fine rotation is undone, so a rotated photo still lands true.
    #[test]
    fn fine_rotation_round_trips() {
        let (x, y) = display_to_image_point(3500.0, 2400.0, W, H, 0, false, false, 1.4);
        // Rotating the result forward by the same angle returns the input.
        let angle: f64 = 1.4_f64.to_radians();
        let (cx, cy) = (W / 2.0, H / 2.0);
        let (dx, dy) = (x - cx, y - cy);
        let back_x = dx * angle.cos() - dy * angle.sin() + cx;
        let back_y = dx * angle.sin() + dy * angle.cos() + cy;
        assert!((back_x - 3500.0).abs() < 1e-6, "x round trip {back_x}");
        assert!((back_y - 2400.0).abs() < 1e-6, "y round trip {back_y}");
    }

    /// A quarter turn swaps the axes rather than mirroring.
    #[test]
    fn quarter_turn_swaps_axes() {
        let (x, y) = display_to_image_point(100.0, 200.0, W, H, 1, false, false, 0.0);
        // Display is H-wide by W-tall after the coarse rotation.
        assert!((x - 200.0).abs() < 1e-6, "x was {x}");
        assert!((y - (H - 100.0)).abs() < 1e-6, "y was {y}");
    }
}
