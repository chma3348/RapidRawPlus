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
use crate::model_registry::{TaskType, mask_subtype_filter, resolve_and_prepare};
use crate::cache_utils::GEOMETRY_KEYS;
use crate::image_loader::composite_patches_on_image;
use crate::image_processing::apply_unwarp_geometry;
use crate::mask_generation::{AiPatchDefinition, MaskDefinition, generate_mask_bitmap};
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
pub(crate) fn mask_components(
    mask: &GrayImage,
    threshold: u8,
) -> (Vec<u32>, Vec<MaskComponent>) {
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
    let (mut large, mut spots): (Vec<_>, Vec<_>) =
        comps.into_iter().partition(|c| c.span() > SPOT_SPAN);
    if large.len() > MAX_DIFFUSION_BLOBS {
        spots.extend(large.split_off(MAX_DIFFUSION_BLOBS));
    }
    log::info!(
        "[fill] mask split into {} diffusion blob(s) + {} LaMa spot(s)",
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

        if let Ok((healed, _)) = ai_processing::run_lama_inpainting(
            &DynamicImage::ImageRgba8(crop_img),
            &crop_mask,
            session,
        ) {
            blend_patch_into(&mut encoded_full, &healed, &crop_mask, x0, y0);
        }
    }

    for comp in &large {
        let span_x = comp.max_x - comp.min_x + 1;
        let span_y = comp.max_y - comp.min_y + 1;
        let pad_x = 192.max((span_x as f32 * 1.5) as u32);
        let pad_y = 192.max((span_y as f32 * 1.5) as u32);
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

        // The sampler keeps a low-frequency imprint of whatever occupies
        // the masked area, so the SDXL tiers get a LaMa prefill as a
        // plausible starting hint. Flux conditions on the mask natively.
        if kind != crate::comfy_engine::FillKind::Flux
            && let Some(session) = lama_session.as_ref()
            && let Ok((prefill, _)) = ai_processing::run_lama_inpainting(
                &DynamicImage::ImageRgba8(crop_img.clone()),
                &crop_mask,
                session,
            )
        {
            crop_img = prefill;
        }

        let (img_png, mask_png, _, _) = crate::expansion::engine_canvas_pngs(&crop_img, &crop_mask)?;
        let fill_png = crate::comfy_engine::run_generative_fill(
            app_handle,
            state,
            kind,
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
        let filled_crop =
            image::imageops::resize(&filled, crop_w, crop_h, image::imageops::FilterType::Lanczos3);
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
    let mask_def_for_generation = MaskDefinition {
        id: patch_definition.id.clone(),
        name: patch_definition.name.clone(),
        visible: patch_definition.visible,
        invert: patch_definition.invert,
        opacity: 100.0,
        grow: 0.0,
        feather: 0.0,
        adjustments: serde_json::Value::Null,
        sub_masks: patch_definition.sub_masks,
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
    let mask_bitmap = unwarped_dynamic.to_luma8();

    let nonzero = mask_bitmap.pixels().filter(|p| p[0] > 0).count();
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
    let engine_model = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Inpaint,
        "inpaint",
        |_| true,
    )
    .await
    .ok()
    .filter(|(_, m)| m.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy"));

    let (patch_rgba, patch_is_gamma) = if let Some((_, model)) = engine_model {
        let kind = crate::comfy_engine::FillKind::from_params(&model.manifest.params);
        run_engine_inpaint_patch(
            &source_image,
            &mask_bitmap,
            &patch_definition.prompt,
            kind,
            &app_handle,
            &state,
        )
        .await?
    } else if use_fast_inpaint {
        let (registry, model) = resolve_and_prepare(
            &app_handle,
            &state.model_registry,
            TaskType::Inpaint,
            "inpaint",
            |_| true,
        )
        .await
        .map_err(|e| e.to_string())?;
        let inpaint_session = registry
            .get_session(&model.manifest.id, None)
            .map_err(|e| e.to_string())?;

        ai_processing::run_lama_inpainting(&source_image, &mask_bitmap, &inpaint_session)
            .map_err(|e| e.to_string())?
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
async fn run_spot_enhance_patch(
    source_image: &DynamicImage,
    mask: &GrayImage,
    task_type: crate::model_registry::TaskType,
    task_key: &str,
    strength: f32,
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

    let is_engine =
        model.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy");

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

    // Strength blend against the untouched crop, then feathered composite
    // of only the brushed pixels.
    let strength = strength.clamp(0.0, 1.0);
    let original_f32: image::Rgb32FImage = DynamicImage::ImageRgba8(crop_img).to_rgb32f();
    let feather = ((crop_w.max(crop_h) as f32) / 100.0).clamp(3.0, 12.0);
    let soft_mask = image::imageops::blur(&crop_mask, feather);
    for y in 0..crop_h {
        for x in 0..crop_w {
            let m = soft_mask.get_pixel(x, y)[0];
            if m > 0 {
                let alpha = (m as f32 / 255.0) * strength;
                let e = enhanced_f32.get_pixel(x, y);
                let o = original_f32.get_pixel(x, y);
                let dst = encoded_full.get_pixel_mut(x0 + x, y0 + y);
                for c in 0..3 {
                    let v = e[c] * alpha + o[c] * (1.0 - alpha);
                    dst[c] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
    }
    Ok((encoded_full, is_linear))
}


#[tauri::command]
pub async fn invoke_spot_enhance_with_mask_def(
    path: String,
    patch_definition: AiPatchDefinition,
    current_adjustments: Value,
    task: String,
    strength: Option<f32>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let _ = path;
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
        &app_handle,
        &state,
    )
    .await?;

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
        assert_eq!((big.min_x, big.min_y, big.max_x, big.max_y), (20, 20, 139, 99));
        assert_eq!(big.area, 120 * 80);
        assert!(big.span() > 96, "big blob goes to diffusion");
        assert!(small.span() <= 96, "speck goes to the LaMa spot path");
        // Labels separate the blobs.
        assert_ne!(labels[25 * 400 + 25], labels[252 * 400 + 352]);
        assert_eq!(labels[0], 0, "background unlabeled");
    }
}
