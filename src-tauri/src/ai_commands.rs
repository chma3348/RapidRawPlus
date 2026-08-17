use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose};
use image::{
    DynamicImage, GenericImageView, GrayImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage,
};
use serde_json::Value;

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
use crate::image_processing::apply_unwarp_geometry;
use crate::mask_generation::{AiPatchDefinition, MaskDefinition, generate_mask_bitmap};
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
    let soft_mask = image::imageops::blur(crop_mask, 4.0);
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

/// Engine-backed removal/replace: splits the mask into connected blobs and
/// fills each in its own tight patch — small blobs heal via LaMa (ideal
/// for speckle selections), large ones via the generative engine — then
/// composites back, mirroring the LaMa patch contract including the gamma
/// flag for float sources. One whole-mask bounding box would balloon to
/// the entire image for scattered selections (e.g. color keys) and force
/// the model to repaint everything at reduced resolution.
async fn run_engine_inpaint_patch(
    source_image: &DynamicImage,
    mask: &GrayImage,
    prompt: &str,
    kind: crate::comfy_engine::FillKind,
    lama_only: bool,
    app_handle: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
) -> Result<(RgbaImage, bool), String> {
    const SPOT_SPAN: u32 = 96;
    const MAX_DIFFUSION_BLOBS: usize = 6;

    let is_linear = matches!(
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

    let (labels, mut comps) = mask_components(mask, 127);
    if comps.is_empty() {
        return Ok((encoded_full, is_linear));
    }
    comps.sort_by_key(|c| std::cmp::Reverse(c.area));
    // Diffusion is for SOLID, object-like regions only. A blob that fills
    // little of its own bounding box is lace (scattered speckle bridged by
    // dilation, typical of color keys) — a diffusion model would repaint
    // the whole dilated region with invented content and read as blotches.
    // LaMa's texture synthesis is the right tool for lace at any size.
    const MIN_SOLID_DENSITY: f32 = 0.35;
    let is_solid = |c: &MaskComponent| {
        let bbox = ((c.max_x - c.min_x + 1) * (c.max_y - c.min_y + 1)).max(1);
        c.area as f32 / bbox as f32 >= MIN_SOLID_DENSITY
    };
    let (mut large, mut spots): (Vec<_>, Vec<_>) = comps
        .into_iter()
        .partition(|c| !lama_only && c.span() > SPOT_SPAN && is_solid(c));
    if large.len() > MAX_DIFFUSION_BLOBS {
        spots.extend(large.split_off(MAX_DIFFUSION_BLOBS));
    }
    log::info!(
        "[fill] mask split into {} solid diffusion blob(s) + {} LaMa region(s)/spot(s)",
        large.len(),
        spots.len()
    );

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

    // Size-tiered model routing: tiny blobs already healed via LaMa above;
    // truly large areas auto-escalate to Flux (the strongest fill tier)
    // when its weights are installed, regardless of the selected model —
    // big reconstructions are where workflow quality dominates, and small
    // ones aren't worth Flux's runtime.
    const FLUX_SPAN: u32 = 320;
    let flux_available =
        crate::comfy_engine::fill_files_present(app_handle, crate::comfy_engine::FillKind::Flux);

    for comp in &large {
        let blob_kind = if comp.span() >= FLUX_SPAN
            && flux_available
            && kind != crate::comfy_engine::FillKind::Flux
        {
            log::info!(
                "[fill] blob span {} ≥ {} — escalating to Flux Fill",
                comp.span(),
                FLUX_SPAN
            );
            crate::comfy_engine::FillKind::Flux
        } else {
            kind
        };
        let span_x = comp.max_x - comp.min_x + 1;
        let span_y = comp.max_y - comp.min_y + 1;
        // Generous context, but CAPPED: a 1.5x pad around a 1500px blob
        // produced a ~6000px crop that the engine downscaled to 1216 —
        // the blob itself rendered at ~300px and upscaled back as mush.
        let pad_x = 192.max((span_x as f32 * 1.5) as u32).min(520);
        let pad_y = 192.max((span_y as f32 * 1.5) as u32).min(520);
        let x0 = comp.min_x.saturating_sub(pad_x);
        let y0 = comp.min_y.saturating_sub(pad_y);
        let x1 = (comp.max_x + pad_x).min(w.saturating_sub(1));
        let y1 = (comp.max_y + pad_y).min(h.saturating_sub(1));
        let (crop_w, crop_h) = (x1 - x0 + 1, y1 - y0 + 1);

        let mut crop_img =
            image::imageops::crop_imm(&encoded_full, x0, y0, crop_w, crop_h).to_image();
        let crop_mask = component_crop_mask(mask, &labels, comp.id, x0, y0, crop_w, crop_h);

        // Grow the mask: slivers of the object just outside the selection
        // otherwise stay visible AND anchor the model to repaint the object.
        let grow = (crop_w.max(crop_h) / 60).clamp(12, 32);
        let crop_mask = dilate_mask(&crop_mask, grow);
        // Ring stats must come from pre-fill pixels (the prefill below
        // rewrites the masked interior).
        let original_crop = crop_img.clone();

        // The sampler keeps a low-frequency imprint of whatever occupies
        // the masked area, so the SDXL tiers get a LaMa prefill as a
        // plausible starting hint. Flux conditions on the mask natively.
        if blob_kind != crate::comfy_engine::FillKind::Flux
            && let Some(session) = lama_session.as_ref()
            && let Ok((prefill, _)) = ai_processing::run_lama_inpainting(
                &DynamicImage::ImageRgba8(crop_img.clone()),
                &crop_mask,
                session,
            )
        {
            crop_img = prefill;
        }

        let (img_png, mask_png, _, _) =
            crate::expansion::engine_canvas_pngs_sized(
                &crop_img,
                &crop_mask,
                // Big reconstructions earn a bigger canvas (Flux handles
                // 1536 comfortably on this hardware).
                if comp.span() >= 900 { 1536 } else { 1216 },
            )?;
        let fill_png = crate::comfy_engine::run_generative_fill(
            app_handle,
            state,
            blob_kind,
            img_png,
            mask_png,
            prompt,
            42,
            |_| {},
        )
        .await
        .map_err(|e| e.to_string())?;

        let filled = image::load_from_memory(&fill_png)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let mut filled_crop = image::imageops::resize(
            &filled,
            crop_w,
            crop_h,
            image::imageops::FilterType::Lanczos3,
        );
        // Prompted fills intentionally differ from their surroundings —
        // only nudge those; prompt-less removal gets the full match.
        let tone_strength = if prompt.trim().is_empty() { 1.0 } else { 0.35 };
        harmonize_patch(&original_crop, &mut filled_crop, &crop_mask, tone_strength);
        blend_patch_into(&mut encoded_full, &filled_crop, &crop_mask, x0, y0);
    }

    Ok((encoded_full, is_linear))
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
    for p in mask_bitmap.pixels_mut() {
        let v = p[0] as f32 / 255.0;
        let boosted = ((v - 0.06) / (0.30 - 0.06)).clamp(0.0, 1.0);
        p[0] = (boosted * 255.0).round() as u8;
    }
    let nonzero = mask_bitmap.pixels().filter(|p| p[0] > 0).count();
    log::info!(
        "[fill] mask confidence boost: {pre_boost} px selected -> {nonzero} px at working strength"
    );
    log::info!(
        "generative_replace: image {}x{}, mask {}x{}, {} masked px, {} sub-masks, fast={}",
        img_w,
        img_h,
        mask_bitmap.width(),
        mask_bitmap.height(),
        nonzero,
        mask_def_for_generation.sub_masks.len(),
        use_fast_inpaint
    );
    // An empty selection previously slipped through to the model, which
    // returns the image unchanged — looking like a silent failure.
    if nonzero == 0 {
        return Err(
            "The selection is empty — brush over the area to remove, then try again.".to_string(),
        );
    }

    // Which local inpaint model is selected decides the local paths: the
    // generative engine (SDXL fill) handles both plain removal and
    // prompt-driven replace; LaMa remains the fast texture fill.
    // The Fast toggle is the user's word: honor it even when an engine
    // model is selected (previously the engine silently overrode it and
    // 'fast' runs took six diffusion round-trips).
    let engine_model = if use_fast_inpaint {
        None
    } else {
        resolve_and_prepare(
            &app_handle,
            &state.model_registry,
            TaskType::Inpaint,
            "inpaint",
            |_| true,
        )
        .await
        .ok()
        .filter(|(_, m)| m.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy"))
    };

    let (patch_rgba, patch_is_gamma) = if let Some((_, model)) = engine_model {
        let kind = crate::comfy_engine::FillKind::from_params(&model.manifest.params);
        run_engine_inpaint_patch(
            &source_image,
            &mask_bitmap,
            &patch_definition.prompt,
            kind,
            false,
            &app_handle,
            &state,
        )
        .await?
    } else if use_fast_inpaint {
        // Fast mode gets the same per-blob split + harmonization as the
        // engine path — every blob heals via LaMa. The old whole-mask
        // single LaMa pass is exactly what produced smeary results on
        // scattered selections.
        run_engine_inpaint_patch(
            &source_image,
            &mask_bitmap,
            &patch_definition.prompt,
            crate::comfy_engine::FillKind::SdxlBase,
            true,
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
            patch_definition.prompt,
            Some(&auth_token),
        )
        .await
        .map_err(|e| e.to_string())
        .map(|img| (img, false))?
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
            patch_definition.prompt,
            None,
        )
        .await
        .map_err(|e| e.to_string())
        .map(|img| (img, false))?
    } else {
        return Err(
            "No generative backend configured or connection invalid. Please check your AI settings."
                .to_string(),
        );
    };

    encode_patch_result(&patch_rgba, patch_is_gamma, &mask_bitmap)
}

/// Encodes a full-size result image + mask into the aiPatches payload the
/// frontend stores in the sidecar (PNG, not JPEG: deep-shadow fills live at
/// pixel values 0-5 where JPEG block noise becomes banding under exposure
/// boosts; the patch is mostly black, so PNG stays small).
fn encode_patch_result(
    patch_rgba: &RgbaImage,
    patch_is_gamma: bool,
    mask_bitmap: &GrayImage,
) -> Result<String, String> {
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

    let result_json = serde_json::json!({
        "color": color_base64,
        "mask": mask_base64,
        "encoding": if patch_is_gamma { "gamma" } else { "linear" },
    })
    .to_string();

    Ok(result_json)
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
    let is_linear = matches!(
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
    use crate::mask_generation::{MaskDefinition, SubMask, SubMaskMode, generate_mask_bitmap};
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
