#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use mimalloc::MiMalloc;

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod adjustment_utils;
mod ai_commands;
mod ai_connector;
pub mod ai_processing;
mod android_integration;
mod app_settings;
mod app_state;
pub mod auto_level;
mod cache_utils;
pub mod color_engine;
pub mod comfy_engine;
mod culling;
mod denoising;
pub mod enhancement;
mod exif_processing;
pub mod expansion;
mod export_processing;
mod file_management;
pub mod finder_tags;
pub mod flat_field;
mod flog2c;
pub mod formats;
pub mod gpu_processing;
pub mod hdr_merge;
pub mod heal_blend;
pub mod image_loader;
pub mod image_processing;
mod lens_correction;
pub mod lut_processing;
mod mask_generation;
pub mod merge_discovery;
pub mod model_library;
pub mod model_registry;
mod negative_conversion;
mod panorama_stitching;
mod panorama_utils;
mod preset_converter;
mod raw_processing;
pub mod replacement_blend;
pub mod scene_masks;
pub mod sky_commands;
pub mod sky_replace;
pub mod subject_selection;
mod tagging;
mod tagging_utils;
pub mod versions;
pub mod video;
pub mod video_server;
pub mod white_balance;
mod window_customizer;
pub mod xmp;

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::io::Write;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageFormat, RgbImage, Rgba};
use imageproc::drawing::draw_line_segment_mut;
use imageproc::edges::canny;
use imageproc::hough::{LineDetectionOptions, detect_lines};
use mozjpeg_rs::{Encoder, Preset};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{Emitter, Manager, ipc::Response};
use tempfile::NamedTempFile;

use crate::cache_utils::{GEOMETRY_KEYS, calculate_geometry_hash, calculate_visual_hash};
use crate::exif_processing::{read_exposure_time_secs, read_iso};
use crate::file_management::parse_virtual_path;
use crate::image_loader::load_base_image_from_bytes;
use crate::image_processing::{
    GeometryParams, apply_coarse_rotation, apply_cpu_default_raw_processing, apply_flip,
    apply_geometry_warp, apply_linear_to_srgb, apply_srgb_to_linear, get_or_init_gpu_context,
    render_adjustments_for_empty, warp_image_geometry,
};
use crate::mask_generation::resolve_warped_image_for_masks;
use crate::window_customizer::PinchZoomDisablePlugin;
pub use adjustment_utils::*;
pub use android_integration::*;
pub use app_settings::*;
pub use app_state::*;
use tagging_utils::{candidates, hierarchy};

#[cfg(target_os = "macos")]
extern "C" fn force_exit(_signal: libc::c_int) {
    unsafe {
        libc::_exit(0);
    }
}

#[cfg(target_os = "macos")]
pub fn register_exit_handler() {
    unsafe {
        libc::signal(libc::SIGABRT, force_exit as *const () as libc::sighandler_t);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn register_exit_handler() {}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommunityPreset {
    pub name: String,
    pub creator: String,
    pub adjustments: Value,
    #[serde(rename = "includeMasks")]
    pub include_masks: Option<bool>,
    #[serde(rename = "includeCropTransform")]
    pub include_crop_transform: Option<bool>,
}

#[derive(serde::Serialize)]
struct ImageDimensions {
    width: u32,
    height: u32,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WgpuTransformPayload {
    pub window_width: f32,
    pub window_height: f32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub clip_x: f32,
    pub clip_y: f32,
    pub clip_width: f32,
    pub clip_height: f32,
    pub bg_primary: [f32; 4],
    pub bg_secondary: [f32; 4],
    pub pixelated: bool,
}

#[tauri::command]
fn get_image_dimensions(path: String) -> Result<ImageDimensions, String> {
    let (source_path, _) = parse_virtual_path(&path);
    image::image_dimensions(&source_path)
        .map(|(width, height)| ImageDimensions { width, height })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn cancel_thumbnail_generation(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state
        .thumbnail_cancellation_token
        .store(true, Ordering::SeqCst);

    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total = 0;
    tracker.completed = 0;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": 0, "total": 0 }),
    );
    Ok(())
}

pub fn get_cached_full_warped_image(
    state: &tauri::State<AppState>,
    js_adjustments: &serde_json::Value,
) -> Result<Arc<DynamicImage>, String> {
    let geo_hash = calculate_geometry_hash(js_adjustments);

    {
        let cache_lock = state.full_warped_cache.lock().unwrap();
        if let Some((hash, img)) = cache_lock.as_ref()
            && *hash == geo_hash
        {
            return Ok(Arc::clone(img));
        }
    }

    let (mut full_image, is_raw) = get_full_image_for_processing(state)?;
    if is_raw {
        apply_cpu_default_raw_processing(&mut full_image);
    }
    let warped_image = apply_geometry_warp(Cow::Borrowed(&full_image), js_adjustments).into_owned();
    let warped_arc = Arc::new(warped_image);

    {
        let mut cache_lock = state.full_warped_cache.lock().unwrap();
        *cache_lock = Some((geo_hash, Arc::clone(&warped_arc)));
    }

    Ok(warped_arc)
}

#[tauri::command]
async fn update_wgpu_transform(
    payload: WgpuTransformPayload,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let context = match state.gpu_context.lock().unwrap().as_ref() {
        Some(c) => c.clone(),
        None => return Ok(()),
    };

    tokio::task::spawn_blocking(move || {
        let mut display_lock = context.display.lock().unwrap();
        if let Some(display) = display_lock.as_mut() {
            display.latest_transform.rect = [payload.x, payload.y, payload.width, payload.height];
            display.latest_transform.clip = [
                payload.clip_x,
                payload.clip_y,
                payload.clip_width,
                payload.clip_height,
            ];
            display.latest_transform.window = [payload.window_width, payload.window_height];
            display.latest_transform.bg_primary = payload.bg_primary;
            display.latest_transform.bg_secondary = payload.bg_secondary;
            display.latest_transform.pixelated = if payload.pixelated { 1.0 } else { 0.0 };

            context.queue.write_buffer(
                &display.transform_buffer,
                0,
                bytemuck::bytes_of(&display.latest_transform),
            );
            display.render(&context.device, &context.queue);
        }
    })
    .await
    .map_err(|e| format!("Task panicked: {}", e))?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_preview_job(
    app_handle: &tauri::AppHandle,
    state: tauri::State<AppState>,
    mut adjustments_json: serde_json::Value,
    is_interactive: bool,
    target_resolution: Option<u32>,
    _roi: Option<(f32, f32, f32, f32)>,
    compute_waveform: bool,
    active_waveform_channel: Option<&str>,
) -> Result<Vec<u8>, String> {
    let _fn_start = std::time::Instant::now();
    let context = get_or_init_gpu_context(&state, app_handle)?;
    hydrate_adjustments(&state, &mut adjustments_json);
    let adjustments_clone = render_adjustments_for_empty(&adjustments_json).into_owned();

    let loaded_image_guard = state.original_image.lock().unwrap();
    let loaded_image = loaded_image_guard
        .as_ref()
        .ok_or("No original image loaded")?
        .clone();
    drop(loaded_image_guard);

    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let dimension = target_resolution.unwrap_or(settings.editor_preview_resolution.unwrap_or(1920));
    let dimension = if is_interactive {
        dimension.min(1280)
    } else {
        dimension
    };
    // In the output space exports use, so the editor shows what the file holds.
    let frame = color_engine::application::render_for_output(
        &context,
        &state,
        &loaded_image.path,
        &adjustments_clone,
        Some(dimension),
    )
    .map_err(|e| e.to_string())?;
    let (width, height) = frame.encoded_srgb.dimensions();
    if let Some(sender) = state.analytics_worker_tx.lock().unwrap().clone() {
        let _ = sender.send(AnalyticsJob {
            path: loaded_image.path.clone(),
            image: Arc::new(DynamicImage::ImageRgba8(frame.preview_rgba8())),
            compute_waveform,
            active_waveform_channel: active_waveform_channel.map(str::to_owned),
        });
    }
    // The clipping warning is drawn over the preview only, after the
    // histogram has seen the real picture.
    let mut frame = frame;
    if adjustments_clone["showClipping"].as_bool() == Some(true) {
        frame.mark_clipping();
    }
    let mut response = Vec::new();
    if is_interactive {
        for v in [0u32, 0, width, height, width, height] {
            response.extend_from_slice(&v.to_le_bytes());
        }
    }
    frame
        .write_display_png(&mut response)
        .map_err(|e| e.to_string())?;
    Ok(response)
}

fn start_analytics_worker(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let (tx, rx): (Sender<AnalyticsJob>, Receiver<AnalyticsJob>) = mpsc::channel();
    *state.analytics_worker_tx.lock().unwrap() = Some(tx);

    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            while let Ok(latest) = rx.try_recv() {
                job = latest;
            }

            if let Ok(histogram_data) = image_processing::calculate_histogram_from_image(&job.image)
            {
                let _ = app_handle.emit(
                    "histogram-update",
                    serde_json::json!({ "path": job.path, "data": histogram_data }),
                );
            }

            if job.compute_waveform
                && let Ok(waveform_data) = image_processing::calculate_waveform_from_image(
                    &job.image,
                    job.active_waveform_channel.as_deref(),
                )
            {
                let _ = app_handle.emit(
                    "waveform-update",
                    serde_json::json!({ "path": job.path, "data": waveform_data }),
                );
            }
        }
    });
}

fn start_preview_worker(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let (tx, rx): (Sender<PreviewJob>, Receiver<PreviewJob>) = mpsc::channel();

    *state.preview_worker_tx.lock().unwrap() = Some(tx);

    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            while let Ok(latest_job) = rx.try_recv() {
                job = latest_job;
            }

            let state = app_handle.state::<AppState>();
            let responder = job.responder;
            match process_preview_job(
                &app_handle,
                state,
                job.adjustments,
                job.is_interactive,
                job.target_resolution,
                job.roi,
                job.compute_waveform,
                job.active_waveform_channel.as_deref(),
            ) {
                Ok(bytes) => {
                    let _ = responder.send(bytes);
                }
                Err(e) => {
                    log::error!("Preview worker error: {}", e);
                }
            }
        }
    });
}

#[tauri::command]
async fn apply_adjustments(
    js_adjustments: serde_json::Value,
    is_interactive: bool,
    target_resolution: Option<u32>,
    roi: Option<(f32, f32, f32, f32)>,
    compute_waveform: bool,
    active_waveform_channel: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Response, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let tx_guard = state.preview_worker_tx.lock().unwrap();
        if let Some(worker_tx) = &*tx_guard {
            let job = PreviewJob {
                adjustments: js_adjustments,
                is_interactive,
                target_resolution,
                roi,
                compute_waveform,
                active_waveform_channel,
                responder: tx,
            };
            worker_tx
                .send(job)
                .map_err(|e| format!("Failed to send to preview worker: {}", e))?;
        } else {
            return Err("Preview worker not running".to_string());
        }
    }

    match rx.await {
        Ok(bytes) => Ok(Response::new(bytes)),
        Err(_) => Err("Superseded or worker failed".to_string()),
    }
}

#[tauri::command]
fn generate_uncropped_preview(
    js_adjustments: serde_json::Value,
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let mut adjustments_clone = js_adjustments.clone();
    hydrate_adjustments(&state, &mut adjustments_clone);
    let adjustments_clone = render_adjustments_for_empty(&adjustments_clone).into_owned();

    let loaded_image = state
        .original_image
        .lock()
        .unwrap()
        .clone()
        .ok_or("No original image loaded")?;

    // Each request supersedes the last: renders finish out of order, and an
    // older one arriving after a newer one would put a stale picture back.
    static LATEST_UNCROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let request = LATEST_UNCROPPED.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

    thread::spawn(move || {
        let state = app_handle.state::<AppState>();
        let path = loaded_image.path.clone();

        // The crop view draws the whole picture and turns it by the fine
        // rotation itself, so this render must have neither the crop nor
        // the rotation. With the rotation baked in, every tilt was applied
        // twice and the picture jumped as each render arrived.
        let mut edits = adjustments_clone.clone();
        edits["crop"] = Value::Null;
        edits["rotation"] = serde_json::json!(0.0);
        if request != LATEST_UNCROPPED.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let rendered =
            color_engine::application::preview_bytes(&context, &state, &path, &edits, 1920);
        if request != LATEST_UNCROPPED.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        match rendered {
            Ok(bytes) => {
                let _ = app_handle.emit(
                    "preview-update-uncropped",
                    format!(
                        "data:image/png;base64,{}",
                        general_purpose::STANDARD.encode(bytes)
                    ),
                );
            }
            Err(e) => log::error!("V3 uncropped preview: {e}"),
        }
    });
    Ok(())
}

#[tauri::command]
fn generate_original_transformed_preview(
    js_adjustments: serde_json::Value,
    target_resolution: Option<u32>,
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let loaded_image = state
        .original_image
        .lock()
        .unwrap()
        .clone()
        .ok_or("No original image loaded")?;

    let mut adjustments_clone = js_adjustments.clone();
    hydrate_adjustments(&state, &mut adjustments_clone);

    let context = get_or_init_gpu_context(&state, &app_handle)?;
    adjustments_clone = color_engine::application::ungraded(&adjustments_clone);
    adjustments_clone["aiPatches"] = serde_json::json!([]);
    let bytes = color_engine::application::preview_bytes(
        &context,
        &state,
        &loaded_image.path,
        &adjustments_clone,
        target_resolution.unwrap_or(1920),
    )?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(bytes)
    ))
}

#[tauri::command]
async fn preview_geometry_transform(
    params: GeometryParams,
    js_adjustments: serde_json::Value,
    show_lines: bool,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let (loaded_image_path, is_raw) = {
        let guard = state.original_image.lock().unwrap();
        let loaded = guard.as_ref().ok_or("No image loaded")?;
        (loaded.path.clone(), loaded.is_raw)
    };

    let visual_hash = calculate_visual_hash(&loaded_image_path, &js_adjustments);

    let base_image_to_warp = {
        let maybe_cached_image = state
            .geometry_cache
            .lock()
            .unwrap()
            .get(&visual_hash)
            .cloned();

        if let Some(cached_image) = maybe_cached_image {
            cached_image
        } else {
            let context = get_or_init_gpu_context(&state, &app_handle)?;

            let settings = load_settings(app_handle.clone()).unwrap_or_default();
            let target_dim =
                (settings.editor_preview_resolution.unwrap_or(1920) as f32 / 1.5) as u32;

            let mut temp_adjustments = js_adjustments.clone();
            hydrate_adjustments(&state, &mut temp_adjustments);

            if let Some(obj) = temp_adjustments.as_object_mut() {
                obj.insert("crop".to_string(), serde_json::Value::Null);
                obj.insert("rotation".to_string(), serde_json::json!(0.0));
                obj.insert("orientationSteps".to_string(), serde_json::json!(0));
                obj.insert("flipHorizontal".to_string(), serde_json::json!(false));
                obj.insert("flipVertical".to_string(), serde_json::json!(false));
                for key in GEOMETRY_KEYS {
                    match *key {
                        "transformScale"
                        | "lensDistortionAmount"
                        | "lensVignetteAmount"
                        | "lensTcaAmount" => {
                            obj.insert(key.to_string(), serde_json::json!(100.0));
                        }
                        "lensDistortionParams" | "lensMaker" | "lensModel" => {
                            obj.insert(key.to_string(), serde_json::Value::Null);
                        }
                        "lensDistortionEnabled" | "lensTcaEnabled" | "lensVignetteEnabled" => {
                            obj.insert(key.to_string(), serde_json::json!(true));
                        }
                        _ => {
                            obj.insert(key.to_string(), serde_json::json!(0.0));
                        }
                    }
                }
            }
            let temp_adjustments = render_adjustments_for_empty(&temp_adjustments).into_owned();

            let processed_base = DynamicImage::ImageRgba8(
                color_engine::application::render_file(
                    &context,
                    &state,
                    &loaded_image_path,
                    &temp_adjustments,
                    Some(target_dim),
                )
                .map_err(|e| e.to_string())?
                .preview_rgba8(),
            );

            let mut cache = state.geometry_cache.lock().unwrap();
            if cache.len() > 5 {
                cache.clear();
            }
            cache.insert(visual_hash, processed_base.clone());

            processed_base
        }
    };

    let final_image = tokio::task::spawn_blocking(move || -> DynamicImage {
        let mut adjusted_params = params;

        if is_raw {
            // approximate linear vignetting correction on gamma-baked & tonemapped geometry preview
            adjusted_params.lens_vignette_amount *= 0.4;
        } else {
            adjusted_params.lens_vignette_amount *= 0.8;
        }

        let warped_image = warp_image_geometry(&base_image_to_warp, adjusted_params);
        let orientation_steps = js_adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
        let flip_horizontal = js_adjustments["flipHorizontal"].as_bool().unwrap_or(false);
        let flip_vertical = js_adjustments["flipVertical"].as_bool().unwrap_or(false);

        let coarse_rotated_image =
            apply_coarse_rotation(Cow::Owned(warped_image), orientation_steps);
        let flipped_image =
            apply_flip(coarse_rotated_image, flip_horizontal, flip_vertical).into_owned();

        if show_lines {
            let gray_image = flipped_image.to_luma8();
            let mut visualization = flipped_image.to_rgba8();
            let edges = canny(&gray_image, 50.0, 100.0);

            let min_dim = gray_image.width().min(gray_image.height());

            let options = LineDetectionOptions {
                vote_threshold: (min_dim as f32 * 0.24) as u32,
                suppression_radius: 15,
            };

            let lines = detect_lines(&edges, options);

            for line in lines {
                let angle_deg = line.angle_in_degrees as f32;
                let angle_norm = angle_deg % 180.0;
                let alignment_threshold = 0.5;
                let is_vertical =
                    angle_norm < alignment_threshold || angle_norm > (180.0 - alignment_threshold);
                let is_horizontal = (angle_norm - 90.0).abs() < alignment_threshold;

                let color = if is_vertical || is_horizontal {
                    Rgba([0, 255, 0, 255])
                } else {
                    Rgba([255, 0, 0, 255])
                };

                let r = line.r;
                let theta_rad = angle_deg.to_radians();
                let a = theta_rad.cos();
                let b = theta_rad.sin();
                let x0 = a * r;
                let y0 = b * r;

                let dist = (visualization.width().max(visualization.height()) * 2) as f32;

                let x1 = x0 + dist * (-b);
                let y1 = y0 + dist * (a);
                let x2 = x0 - dist * (-b);
                let y2 = y0 - dist * (a);

                draw_line_segment_mut(&mut visualization, (x1, y1), (x2, y2), color);
                draw_line_segment_mut(
                    &mut visualization,
                    (x1 + a, y1 + b),
                    (x2 + a, y2 + b),
                    color,
                );
            }

            DynamicImage::ImageRgba8(visualization)
        } else {
            flipped_image
        }
    })
    .await
    .map_err(|e| e.to_string())?;

    let (width, height) = final_image.dimensions();
    let rgb_pixels = final_image.to_rgb8().into_vec();

    let bytes = Encoder::new(Preset::BaselineFastest)
        .quality(75)
        .encode_rgb(&rgb_pixels, width, height)
        .map_err(|e| format!("Failed to encode with mozjpeg-rs: {}", e))?;

    let base64_str = general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:image/jpeg;base64,{}", base64_str))
}

pub fn get_full_image_for_processing(
    state: &tauri::State<AppState>,
) -> Result<(DynamicImage, bool), String> {
    let original_image_lock = state.original_image.lock().unwrap();
    let loaded_image = original_image_lock
        .as_ref()
        .ok_or("No original image loaded")?;
    Ok((
        loaded_image.image.clone().as_ref().clone(),
        loaded_image.is_raw,
    ))
}

#[tauri::command]
fn generate_preset_preview(
    js_adjustments: serde_json::Value,
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let render_adjustments_cow = render_adjustments_for_empty(&js_adjustments);
    let render_adjustments = render_adjustments_cow.as_ref();

    let loaded_image = state
        .original_image
        .lock()
        .unwrap()
        .clone()
        .ok_or("No original image loaded for preset preview")?;

    color_engine::application::preview_bytes(
        &context,
        &state,
        &loaded_image.path,
        render_adjustments,
        400,
    )
    .map(Response::new)
}

#[tauri::command]
async fn fetch_community_presets() -> Result<Vec<CommunityPreset>, String> {
    let client = reqwest::Client::new();
    let url = "https://raw.githubusercontent.com/CyberTimon/RapidRAW-Presets/main/manifest.json";

    let response = client
        .get(url)
        .header("User-Agent", "RapidRAW-App")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch manifest from GitHub: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("GitHub returned an error: {}", response.status()));
    }

    let presets: Vec<CommunityPreset> = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse manifest.json: {}", e))?;

    Ok(presets)
}

#[tauri::command]
async fn generate_all_community_previews(
    image_paths: Vec<String>,
    presets: Vec<CommunityPreset>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let mut results: HashMap<String, Vec<u8>> = HashMap::new();

    const TILE_DIM: u32 = 360;
    const PROCESSING_DIM: u32 = TILE_DIM * 2;

    for preset in presets.iter() {
        let mut processed_tiles: Vec<RgbImage> = Vec::new();
        let mut js_adjustments = preset.adjustments.clone();
        if let Some(obj) = js_adjustments.as_object_mut() {
            for key in ["v3Pipeline", "v3Input", "v3RawRecovery"] {
                obj.remove(key);
            }
        }
        for image_path in &image_paths {
            let processed_image = DynamicImage::ImageRgba8(
                color_engine::application::render_file(
                    &context,
                    &state,
                    image_path,
                    &js_adjustments,
                    Some(PROCESSING_DIM),
                )
                .map_err(|e| e.to_string())?
                .preview_rgba8(),
            )
            .to_rgb8();

            let (proc_w, proc_h) = processed_image.dimensions();
            let size = proc_w.min(proc_h);
            let cropped_processed_image = image::imageops::crop_imm(
                &processed_image,
                (proc_w - size) / 2,
                (proc_h - size) / 2,
                size,
                size,
            )
            .to_image();

            let final_tile = image::imageops::resize(
                &cropped_processed_image,
                TILE_DIM,
                TILE_DIM,
                image::imageops::FilterType::Lanczos3,
            );
            processed_tiles.push(final_tile);
        }

        let final_image_buffer = match processed_tiles.len() {
            1 => processed_tiles.remove(0),
            2 => {
                let mut canvas = RgbImage::new(TILE_DIM * 2, TILE_DIM);
                image::imageops::overlay(&mut canvas, &processed_tiles[0], 0, 0);
                image::imageops::overlay(&mut canvas, &processed_tiles[1], TILE_DIM as i64, 0);
                canvas
            }
            4 => {
                let mut canvas = RgbImage::new(TILE_DIM * 2, TILE_DIM * 2);
                image::imageops::overlay(&mut canvas, &processed_tiles[0], 0, 0);
                image::imageops::overlay(&mut canvas, &processed_tiles[1], TILE_DIM as i64, 0);
                image::imageops::overlay(&mut canvas, &processed_tiles[2], 0, TILE_DIM as i64);
                image::imageops::overlay(
                    &mut canvas,
                    &processed_tiles[3],
                    TILE_DIM as i64,
                    TILE_DIM as i64,
                );
                canvas
            }
            _ => continue,
        };

        let mut buf = Cursor::new(Vec::new());
        if final_image_buffer
            .write_with_encoder(JpegEncoder::new_with_quality(&mut buf, 75))
            .is_ok()
        {
            results.insert(preset.name.clone(), buf.into_inner());
        }
    }

    Ok(results)
}

#[tauri::command]
async fn save_temp_file(bytes: Vec<u8>) -> Result<String, String> {
    let mut temp_file = NamedTempFile::new().map_err(|e| e.to_string())?;
    temp_file.write_all(&bytes).map_err(|e| e.to_string())?;
    let (_file, path) = temp_file.keep().map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn merge_hdr(
    paths: Vec<String>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if paths.len() < 2 {
        return Err("Please select at least two images to merge.".to_string());
    }

    let hdr_result_handle = state.hdr_result.clone();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    let loaded_items: Vec<(String, DynamicImage, Duration, f32)> = paths
        .iter()
        .map(|path| {
            let _ = app_handle.emit(
                "hdr-progress",
                format!(
                    "Processing '{}'",
                    Path::new(path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
            );

            let file_bytes =
                fs::read(path).map_err(|e| format!("Failed to read image {}: {}", path, e))?;
            let mut dynamic_image =
                load_base_image_from_bytes(&file_bytes, path, false, &settings, None)
                    .map_err(|e| format!("Failed to load image {}: {}", path, e))?;

            if !crate::formats::is_raw_file(path) {
                dynamic_image = apply_srgb_to_linear(dynamic_image);
            }

            // ISO only scales effective exposure; a missing value is no
            // longer fatal, it just means "same sensitivity as the others".
            let gains = read_iso(path, &file_bytes)
                .map(|v| v as f32)
                .unwrap_or(100.0);

            let exposure = match read_exposure_time_secs(path, &file_bytes) {
                None => return Err(format!("Image {} is missing ExposureTime data", path)),
                Some(exp) => Duration::from_secs_f32(exp),
            };

            Ok((path.clone(), dynamic_image, exposure, gains))
        })
        .collect::<Result<Vec<_>, String>>()?;

    if let Some((first_path, first_img, _, _)) = loaded_items.first() {
        let (width, height) = (first_img.width(), first_img.height());

        for (path, img, _, _) in loaded_items.iter().skip(1) {
            if img.width() != width || img.height() != height {
                return Err(format!(
                    "Dimension mismatch detected.\n\nBase image ({}): {}x{}\nTarget image ({}): {}x{}\n\nHDR merge requires all images to be exactly the same size.",
                    Path::new(first_path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    width,
                    height,
                    Path::new(path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    img.width(),
                    img.height()
                ));
            }
        }
    }

    // Effective exposure folds ISO in, so ISO-varied brackets merge
    // correctly too; the reference frame's ISO sets the scale.
    let reference_iso = loaded_items
        .first()
        .map(|(_, _, _, g)| *g)
        .unwrap_or(100.0)
        .max(1.0);
    let exposures: Vec<f32> = loaded_items
        .iter()
        .map(|(_, _, exposure, gains)| exposure.as_secs_f32() * (gains / reference_iso))
        .collect();
    let mut frames: Vec<image::Rgb32FImage> = loaded_items
        .iter()
        .map(|(_, img, _, _)| img.to_rgb32f())
        .collect();

    let _ = app_handle.emit("hdr-progress", "Aligning frames...");
    let shifts = crate::hdr_merge::align_frames(&mut frames, &exposures);
    for ((path, _, _, _), (dx, dy)) in loaded_items.iter().zip(&shifts) {
        if *dx != 0 || *dy != 0 {
            log::info!(
                "[hdr] aligned '{}' by ({dx}, {dy}) px",
                Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
        }
    }

    let _ = app_handle.emit("hdr-progress", "Merging exposures...");
    log::info!("Starting HDR merge of {} images", frames.len());
    let merged_linear = crate::hdr_merge::merge_frames(
        &frames,
        &exposures,
        &crate::hdr_merge::MergeOptions::default(),
    )?;
    // 16-bit output: an 8-bit PNG threw away most of the range the merge
    // just recovered, leaving nothing for the editor's tonal controls.
    let hdr_merged = apply_linear_to_srgb(DynamicImage::ImageRgb32F(merged_linear));
    let hdr_merged = DynamicImage::ImageRgb16(hdr_merged.to_rgb16());
    log::info!("HDR merge completed");

    let mut buf = Cursor::new(Vec::new());
    if let Err(e) = hdr_merged.to_rgb8().write_to(&mut buf, ImageFormat::Png) {
        return Err(format!("Failed to encode hdr preview: {}", e));
    }

    let base64_str = general_purpose::STANDARD.encode(buf.get_ref());
    let final_base64 = format!("data:image/png;base64,{}", base64_str);

    let _ = app_handle.emit("hdr-progress", "Creating preview...");

    *hdr_result_handle.lock().unwrap() = Some(hdr_merged);

    let _ = app_handle.emit(
        "hdr-complete",
        serde_json::json!({
            "base64": final_base64,
        }),
    );
    Ok(())
}

#[tauri::command]
async fn save_hdr(
    first_path_str: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let hdr_image = state.hdr_result.lock().unwrap().take().ok_or_else(|| {
        "No hdr image found in memory to save. It might have already been saved.".to_string()
    })?;

    let (first_path, _) = parse_virtual_path(&first_path_str);
    let parent_dir = first_path
        .parent()
        .ok_or_else(|| "Could not determine parent directory of the first image.".to_string())?;
    let stem = first_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("hdr");

    let (output_filename, image_to_save): (String, DynamicImage) = if hdr_image.color().has_alpha()
    {
        (
            format!("{}_Hdr.png", stem),
            DynamicImage::ImageRgba8(hdr_image.to_rgba8()),
        )
    } else if hdr_image.as_rgb32f().is_some() || hdr_image.as_rgb16().is_some() {
        // TIFF keeps the merged bit depth; PNG-8 would discard it.
        (format!("{}_Hdr.tiff", stem), hdr_image)
    } else {
        (
            format!("{}_Hdr.png", stem),
            DynamicImage::ImageRgb8(hdr_image.to_rgb8()),
        )
    };

    let output_path = parent_dir.join(output_filename);

    image_to_save
        .save(&output_path)
        .map_err(|e| format!("Failed to save hdr image: {}", e))?;

    let (real_path, _) = crate::file_management::parse_virtual_path(&first_path_str);
    let _ =
        crate::exif_processing::write_rrexif_sidecar(&real_path.to_string_lossy(), &output_path);

    Ok(output_path.to_string_lossy().to_string())
}

#[tauri::command]
async fn save_collage(base64_data: String, first_path_str: String) -> Result<String, String> {
    let data_url_prefix = "data:image/png;base64,";
    if !base64_data.starts_with(data_url_prefix) {
        return Err("Invalid base64 data format".to_string());
    }
    let encoded_data = &base64_data[data_url_prefix.len()..];

    let decoded_bytes = general_purpose::STANDARD
        .decode(encoded_data)
        .map_err(|e| format!("Failed to decode base64: {}", e))?;

    let (first_path, _) = parse_virtual_path(&first_path_str);
    let parent_dir = first_path
        .parent()
        .ok_or_else(|| "Could not determine parent directory of the first image.".to_string())?;
    let stem = first_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("collage");

    let output_filename = format!("{}_Collage.png", stem);
    let output_path = parent_dir.join(output_filename);

    fs::write(&output_path, &decoded_bytes)
        .map_err(|e| format!("Failed to save collage image: {}", e))?;

    Ok(output_path.to_string_lossy().to_string())
}

#[tauri::command]
fn generate_preview_for_path(
    path: String,
    js_adjustments: Value,
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let render_adjustments_cow = render_adjustments_for_empty(&js_adjustments);
    let render_adjustments = render_adjustments_cow.as_ref();

    color_engine::application::preview_bytes(&context, &state, &path, render_adjustments, 1920)
        .map(Response::new)
}

fn setup_logging(app_handle: &tauri::AppHandle) {
    let log_dir = match app_handle.path().app_log_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Failed to get app log directory: {}", e);
            return;
        }
    };

    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("Failed to create log directory at {:?}: {}", log_dir, e);
    }

    let log_file_path = log_dir.join("app.log");

    let log_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&log_file_path)
        .ok();

    let var = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let level: log::LevelFilter = var.parse().unwrap_or(log::LevelFilter::Info);

    let mut dispatch = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                message
            ))
        })
        .level(level)
        .chain(std::io::stderr());

    if let Some(file) = log_file {
        dispatch = dispatch.chain(file);
    } else {
        eprintln!(
            "Failed to open log file at {:?}. Logging to console only.",
            log_file_path
        );
    }

    if let Err(e) = dispatch.apply() {
        eprintln!("Failed to apply logger configuration: {}", e);
    }

    panic::set_hook(Box::new(|info| {
        let message = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            format!("{:?}", info.payload())
        };
        let location = info.location().map_or_else(
            || "at an unknown location".to_string(),
            |loc| format!("at {}:{}:{}", loc.file(), loc.line(), loc.column()),
        );
        log::error!("PANIC! {} - {}", location, message.trim());
    }));

    log::info!(
        "Logger initialized successfully. Log file at: {:?}",
        log_file_path
    );
}

#[tauri::command]
fn get_log_file_path(app_handle: tauri::AppHandle) -> Result<String, String> {
    let log_dir = app_handle.path().app_log_dir().map_err(|e| e.to_string())?;
    let log_file_path = log_dir.join("app.log");
    Ok(log_file_path.to_string_lossy().to_string())
}

#[tauri::command]
fn frontend_log(level: String, message: String) -> Result<(), String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let log_line = |line: &str| match level.to_lowercase().as_str() {
        "error" => log::error!("[frontend] {}", line),
        "warn" => log::warn!("[frontend] {}", line),
        "debug" => log::debug!("[frontend] {}", line),
        "trace" => log::trace!("[frontend] {}", line),
        _ => log::info!("[frontend] {}", line),
    };

    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        log_line(line);
    }

    Ok(())
}

fn handle_file_open(app_handle: &tauri::AppHandle, path: PathBuf) {
    if let Some(path_str) = path.to_str()
        && let Err(e) = app_handle.emit("open-with-file", path_str)
    {
        log::error!("Failed to emit open-with-file event: {}", e);
    }
}

#[tauri::command]
fn frontend_ready(
    app_handle: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<AppState>,
) -> Result<(), String> {
    let is_first_run = !state
        .window_setup_complete
        .swap(true, std::sync::atomic::Ordering::Relaxed);
    #[cfg(target_os = "android")]
    let _ = (is_first_run, &window);

    #[cfg(not(target_os = "android"))]
    {
        // Only windows/linux restore these; elsewhere they stay `false`.
        #[allow(unused_mut)]
        let mut should_maximize = false;
        #[allow(unused_mut)]
        let mut should_fullscreen = false;

        if is_first_run && let Ok(config_dir) = app_handle.path().app_config_dir() {
            let path = config_dir.join("window_state.json");

            if let Ok(contents) = std::fs::read_to_string(&path)
                && let Ok(saved_state) = serde_json::from_str::<WindowState>(&contents)
            {
                #[cfg(any(windows, target_os = "linux"))]
                {
                    should_maximize = saved_state.maximized;
                    should_fullscreen = saved_state.fullscreen;
                }
                #[cfg(not(any(windows, target_os = "linux")))]
                let _ = &saved_state;

                if (should_maximize || should_fullscreen)
                    && let Some(monitor) = window
                        .current_monitor()
                        .ok()
                        .flatten()
                        .or_else(|| window.primary_monitor().ok().flatten())
                        .or_else(|| {
                            window
                                .available_monitors()
                                .ok()
                                .and_then(|m| m.into_iter().next())
                        })
                {
                    let monitor_size = monitor.size();
                    let monitor_pos = monitor.position();
                    let default_width = 1280i32;
                    let default_height = 720i32;
                    let center_x = monitor_pos.x + (monitor_size.width as i32 - default_width) / 2;
                    let center_y =
                        monitor_pos.y + (monitor_size.height as i32 - default_height) / 2;

                    let _ = window.set_size(tauri::PhysicalSize::new(
                        default_width as u32,
                        default_height as u32,
                    ));
                    let _ = window.set_position(tauri::PhysicalPosition::new(center_x, center_y));
                }
            }
        }

        if let Err(e) = window.show() {
            log::error!("Failed to show window: {}", e);
        }
        if let Err(e) = window.set_focus() {
            log::error!("Failed to focus window: {}", e);
        }
        if is_first_run {
            if should_maximize {
                let _ = window.maximize();
            }
            if should_fullscreen {
                let _ = window.set_fullscreen(true);
            }
        }
    }

    if let Some(path) = state.initial_file_path.lock().unwrap().take() {
        log::info!(
            "Frontend is ready, emitting open-with-file for initial path: {}",
            &path
        );
        handle_file_open(&app_handle, PathBuf::from(path));
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            log::info!(
                "New instance launched with args: {:?}. Focusing main window.",
                argv
            );
            if let Some(window) = app.get_webview_window("main") {
                if let Err(e) = window.unminimize() {
                    log::error!("Failed to unminimize window: {}", e);
                }
                if let Err(e) = window.set_focus() {
                    log::error!("Failed to set focus on window: {}", e);
                }
            }

            if argv.len() > 1 {
                let path_str = &argv[1];
                if let Err(e) = app.emit("open-with-file", path_str) {
                    log::error!(
                        "Failed to emit open-with-file from single-instance handler: {}",
                        e
                    );
                }
            }
        }));
    }

    builder
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(PinchZoomDisablePlugin)
        .on_window_event(|window, event| if let tauri::WindowEvent::Resized(size) = event {
            let state = window.state::<AppState>();
            if let Some(ctx) = state.gpu_context.lock().unwrap().as_ref()
                && let Ok(mut display_lock) = ctx.display.try_lock()
                    && let Some(display) = display_lock.as_mut() {
                        display.config.width = size.width.max(1);
                        display.config.height = size.height.max(1);
                        display.surface.configure(&ctx.device, &display.config);
                        display.render(&ctx.device, &ctx.queue);
                    }
        })
        .setup(|app| {
            #[cfg(any(windows, target_os = "linux"))]
            {
                if let Some(arg) = std::env::args().nth(1) {
                     let state = app.state::<AppState>();
                     log::info!("Windows/Linux initial open: Storing path {} for later.", &arg);
                     *state.initial_file_path.lock().unwrap() = Some(arg);
                }
            }

            let app_handle = app.handle().clone();
            let config_dir = app_handle.path().app_config_dir().expect("Failed to get config dir");
            let crash_flag_path = config_dir.join(".gpu_init_crash_flag");

            {
                let state = app.state::<AppState>();
                *state.gpu_crash_flag_path.lock().unwrap() = Some(crash_flag_path.clone());
            }

            let mut settings: AppSettings = load_settings(app_handle.clone()).unwrap_or_default();

            {
                let state = app.state::<AppState>();
                let cache_size = settings.image_cache_size.unwrap_or(5) as usize;
                state.decoded_image_cache.lock().unwrap().set_capacity(cache_size);
            }

            if crash_flag_path.exists() {
                log::warn!("GPU Driver crash detected on last run! Falling back to OpenGL backend.");
                settings.processing_backend = Some("gl".to_string());
                let _ = crate::save_settings(settings.clone(), app_handle.clone());
                let _ = std::fs::remove_file(&crash_flag_path);
            }

            let lens_db = lens_correction::load_lensfun_db(&app_handle);
            let state = app.state::<AppState>();
            *state.lens_db.lock().unwrap() = Some(Arc::new(lens_db));

            unsafe {
                if let Some(backend) = &settings.processing_backend
                    && backend != "auto" {
                        std::env::set_var("WGPU_BACKEND", backend);
                    }

                if settings.linux_gpu_optimization.unwrap_or(true) {
                    #[cfg(target_os = "linux")]
                    {
                        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
                        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
                        std::env::set_var("NODEVICE_SELECT", "1");
                    }
                }

                #[cfg(not(target_os = "android"))]
                {
                    let resource_path = app_handle
                        .path()
                        .resolve("resources", tauri::path::BaseDirectory::Resource)
                        .expect("failed to resolve resource directory");

                    let ort_library_name = {
                        #[cfg(target_os = "windows")]
                        { "onnxruntime.dll" }
                        #[cfg(target_os = "linux")]
                        { "libonnxruntime.so" }
                        #[cfg(target_os = "macos")]
                        { "libonnxruntime.dylib" }
                        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
                        { "libonnxruntime.so" }
                    };
                    let ort_library_path = resource_path.join(ort_library_name);
                    std::env::set_var("ORT_DYLIB_PATH", &ort_library_path);
                    println!("Set ORT_DYLIB_PATH to: {}", ort_library_path.display());
                }
            }

            setup_logging(&app_handle);

            if let Some(backend) = &settings.processing_backend
                && backend != "auto" {
                    log::info!("Applied processing backend setting: {}", backend);
                }
            if settings.linux_gpu_optimization.unwrap_or(false) {
                #[cfg(target_os = "linux")]
                {
                    log::info!("Applied Linux GPU optimizations.");
                }
            }

            start_preview_worker(app_handle.clone());
            start_analytics_worker(app_handle.clone());
            file_management::start_thumbnail_workers(app_handle.clone());
            jxl_oxide::integration::register_image_decoding_hook();

            let window_cfg = app.config().app.windows.first().unwrap().clone();
            let decorations = settings.decorations.unwrap_or(window_cfg.decorations);
            #[cfg(target_os = "android")]
            let _ = decorations;

            let main_window_cfg = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .expect("Main window config not found")
                .clone();

            let mut window_builder =
                tauri::WebviewWindowBuilder::from_config(app.handle(), &main_window_cfg)
                    .unwrap();

            #[cfg(not(target_os = "android"))]
            {
                window_builder = window_builder.decorations(decorations).visible(false);
            }

            let window = window_builder.build().expect("Failed to build window");

            #[cfg(target_os = "android")]
            android_integration::initialize_android(&window);

            #[cfg(not(target_os = "android"))]
            {
                let app_state = app.state::<AppState>();
                if let Err(error) = get_or_init_gpu_context(&app_state, app.handle()) {
                    log::warn!(
                        "GPU pre-initialization failed (editing and thumbnails may be degraded): {}",
                        error
                    );
                }

                // A rendering transform captured from this machine's Resolve,
                // installed by dropping the .cube here. Resolved once, and
                // said out loud: what renders the picture should never be a
                // silent consequence of a file existing.
                if let Ok(data_dir) = app.path().app_data_dir() {
                    *app.state::<AppState>().v3_asset_dir.lock().unwrap() = Some(data_dir.join("color-v3-assets"));
                    for (name, slot) in [
                        ("output-transform.cube", 0usize),
                        ("input-transform.cube", 1usize),
                        ("input-transform-p3.cube", 2usize),
                        ("output-transform-p3.cube", 3usize),
                    ] {
                        let cube = data_dir.join(name);
                        if !cube.is_file() {
                            continue;
                        }
                        match crate::color_engine::cube::CubeLut::load(&cube) {
                            Ok(lut) => {
                                log::info!(
                                    "Color v3 uses the captured {} (size {}, {})",
                                    name,
                                    lut.size,
                                    &lut.digest[..12]
                                );
                                let state = app.state::<AppState>();
                                let mut held = match slot {
                                    0 => state.output_transform.lock().unwrap(),
                                    1 => state.input_transform.lock().unwrap(),
                                    2 => state.input_transform_p3.lock().unwrap(),
                                    _ => state.output_transform_p3.lock().unwrap(),
                                };
                                *held = Some(cube);
                            }
                            Err(error) => log::error!(
                                "Ignoring {}: {error}. Color v3 keeps its built-in rendering.",
                                cube.display()
                            ),
                        }
                    }
                    // An input transform without its output transform cannot
                    // be used: rendered photos would become scene data with
                    // nothing to render them. Say so once, here, rather than
                    // failing every render.
                    let state = app.state::<AppState>();
                    let has_output = state.output_transform.lock().unwrap().is_some();
                    let mut input = state.input_transform.lock().unwrap();
                    let mut input_p3 = state.input_transform_p3.lock().unwrap();
                    if !has_output && (input.is_some() || input_p3.is_some()) {
                        log::error!(
                            "Color v3: input-transform.cube is installed without output-transform.cube; ignoring the input transform. Install the matching output capture."
                        );
                        *input = None;
                        *input_p3 = None;
                    }
                    if input.is_none() && input_p3.is_some() {
                        log::error!(
                            "Color v3: input-transform-p3.cube needs input-transform.cube (the sRGB capture) alongside it; ignoring the P3 capture."
                        );
                        *input_p3 = None;
                    }
                    if let (Some(srgb), Some(p3)) = (input.as_ref(), input_p3.as_ref())
                        && let (Ok(srgb), Ok(p3)) = (
                            crate::color_engine::cube::CubeLut::load(srgb),
                            crate::color_engine::cube::CubeLut::load(p3),
                        )
                        && let Err(error) = crate::color_engine::cube::p3_capture_matches(&srgb, &p3)
                    {
                        log::error!("Color v3: ignoring input-transform-p3.cube: {error}");
                        *input_p3 = None;
                    }
                    // The P3 output capture renders the same greys as the sRGB
                    // one, or it was captured with the wrong output settings.
                    let output = state.output_transform.lock().unwrap().clone();
                    let mut output_p3 = state.output_transform_p3.lock().unwrap();
                    if output_p3.is_some() {
                        let checked = output.as_ref().ok_or_else(|| anyhow::anyhow!(
                            "it needs output-transform.cube (the sRGB capture) alongside it"
                        )).and_then(|srgb| {
                            let srgb = crate::color_engine::cube::CubeLut::load(srgb)?;
                            let p3 = crate::color_engine::cube::CubeLut::load(output_p3.as_ref().unwrap())?;
                            crate::color_engine::cube::p3_output_matches(&srgb, &p3)
                        });
                        if let Err(error) = checked {
                            log::error!("Color v3: ignoring output-transform-p3.cube: {error}");
                            *output_p3 = None;
                        }
                    }
                    drop(output_p3);
                    // The editor preview and exports: Display P3 unless the
                    // settings choose sRGB.
                    let space = crate::color_engine::config::OutputSpace::from_setting(
                        crate::app_settings::load_settings(app.handle().clone())
                            .ok()
                            .and_then(|s| s.output_color_space)
                            .as_deref(),
                    );
                    *state.output_space.lock().unwrap() = space;
                    log::info!(
                        "Color v3 output colour space: {:?}{}",
                        space,
                        if space == crate::color_engine::config::OutputSpace::DisplayP3
                            && state.output_transform_p3.lock().unwrap().is_none()
                        {
                            " (no Display P3 output capture installed: sRGB colours stored as P3)"
                        } else {
                            ""
                        }
                    );
                }

                if let Ok(config_dir) = app.path().app_config_dir() {
                    let path = config_dir.join("window_state.json");
                    if let Ok(contents) = std::fs::read_to_string(&path) {
                        if let Ok(state) = serde_json::from_str::<WindowState>(&contents) {
                            if state.width >= 800  && state.height >= 600 {
                                let _ = window.set_size(tauri::Size::Physical(
                                    tauri::PhysicalSize::new(state.width, state.height),
                                ));
                                let _ = window.set_position(tauri::Position::Physical(
                                    tauri::PhysicalPosition::new(state.x, state.y),
                                ));
                            } else {
                                log::warn!(
                                    "Saved window state had unreasonable dimensions ({}x{}), centering instead.",
                                    state.width,
                                    state.height
                                );
                                let _ = window.center();
                            }
                        } else {
                            let _ = window.center();
                        }
                    } else {
                        let _ = window.center();
                    }
                } else {
                    let _ = window.center();
                }

                let window_failsafe = window.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    if let Ok(false) = window_failsafe.is_visible() {
                        log::warn!(
                            "Frontend failed to report ready within timeout. Forcing window visibility."
                        );
                        let _ = window_failsafe.show();
                        let _ = window_failsafe.set_focus();
                    }
                });

                let pending_window_state = Arc::new(Mutex::new(None::<WindowState>));
                let pending_state_for_saver = pending_window_state.clone();
                let app_handle_for_saver = app.handle().clone();

                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;

                        let state_to_save = {
                            let mut lock = pending_state_for_saver.lock().unwrap();
                            lock.take()
                        };

                        if let Some(state) = state_to_save
                            && let Ok(config_dir) =
                                app_handle_for_saver.path().app_config_dir()
                        {
                            let path = config_dir.join("window_state.json");
                            let _ = std::fs::create_dir_all(&config_dir);
                            if let Ok(json) = serde_json::to_string(&state) {
                                let _ = std::fs::write(&path, json);
                            }
                        }
                    }
                });

                let window_for_handler = window.clone();
                let pending_state_for_handler = pending_window_state.clone();

                window.on_window_event(move |event| match event {
                    tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_) => {
                        #[cfg(any(windows, target_os = "linux"))]
                        let maximized = window_for_handler.is_maximized().unwrap_or(false);
                        #[cfg(not(any(windows, target_os = "linux")))]
                        let maximized = false;

                        #[cfg(any(windows, target_os = "linux"))]
                        let fullscreen = window_for_handler.is_fullscreen().unwrap_or(false);
                        #[cfg(not(any(windows, target_os = "linux")))]
                        let fullscreen = false;

                        if window_for_handler.is_minimized().unwrap_or(false) {
                            return;
                        }

                        let mut state = WindowState {
                            width: 1280,
                            height: 720,
                            x: 0,
                            y: 0,
                            maximized,
                            fullscreen,
                        };

                        if let Ok(position) = window_for_handler.outer_position() {
                            state.x = position.x;
                            state.y = position.y;
                        }

                        if !maximized
                            && !fullscreen
                            && let Ok(size) = window_for_handler.outer_size()
                            && size.width >= 800
                            && size.height >= 600
                        {
                            state.width = size.width;
                            state.height = size.height;
                        }

                        *pending_state_for_handler.lock().unwrap() = Some(state);
                    }
                    _ => {}
                });
            }

            crate::register_exit_handler();
            Ok(())
        })
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            color_engine::application::auto_color_v3,
            color_engine::application::pin_color_v3,
            color_engine::selection::inspect_color_v3,
            apply_adjustments,
            generate_preview_for_path,
            generate_original_transformed_preview,
            generate_preset_preview,
            generate_uncropped_preview,
            preview_geometry_transform,
            get_log_file_path,
            frontend_log,
            save_collage,
            merge_hdr,
            save_hdr,
            lut_processing::load_and_parse_lut,
            lut_processing::list_luts,
            lut_processing::import_luts,
            lut_processing::remove_lut,
            lut_processing::generate_lut_previews,
            fetch_community_presets,
            generate_all_community_previews,
            save_temp_file,
            get_image_dimensions,
            frontend_ready,
            cancel_thumbnail_generation,
            update_wgpu_transform,
            android_integration::resolve_android_content_uri_name,
            cache_utils::clear_session_caches,
            cache_utils::clear_image_caches,
            app_settings::load_settings,
            app_settings::save_settings,
            app_settings::set_output_color_space,
            ai_commands::generate_ai_subject_mask,
            ai_commands::generate_ai_auto_subject_mask,
            ai_commands::generate_ai_paint_mask,
            ai_commands::sample_image_color,
            ai_commands::match_white_balance,
            ai_commands::respot_enhance,
            ai_commands::reblend_replacement,
            ai_commands::apply_clone_patch,
            ai_commands::list_managed_luts,
            merge_discovery::discover_merge_candidates,
            flat_field::create_flat_profile,
            flat_field::list_flat_profiles,
            flat_field::delete_flat_profile,
            ai_commands::precompute_ai_subject_mask,
            ai_commands::generate_ai_foreground_mask,
            ai_commands::generate_ai_sky_mask,
            ai_commands::generate_ai_depth_mask,
            ai_commands::check_ai_connector_status,
            ai_commands::test_ai_connector_connection,
            ai_commands::invoke_generative_replace_with_mask_def,
            model_registry::list_registered_models,
            model_registry::rescan_model_registry,
            model_registry::test_model_round_trip,
            model_library::get_model_library,
            ai_commands::invoke_spot_enhance_with_mask_def,
            comfy_engine::get_engine_status,
            comfy_engine::list_engine_loras,
            comfy_engine::install_ai_engine,
            model_library::download_library_model,
            model_library::delete_library_model,
            model_library::add_model_from_url,
            model_library::add_model_from_file,
            model_library::search_remote_models,
            model_library::install_remote_model,
            enhancement::apply_enhancement,
            enhancement::get_enhancement_overview,
            enhancement::preview_enhancement,
            enhancement::save_enhanced_image,
            expansion::apply_expansion,
            expansion::save_expanded_image,
            denoising::apply_denoising,
            denoising::batch_denoise_images,
            denoising::save_denoised_image,
            image_loader::load_image,
            image_loader::is_image_cached,
            panorama_stitching::stitch_panorama,
            panorama_stitching::save_panorama,
            export_processing::export_images,
            export_processing::cancel_export,
            export_processing::estimate_export_sizes,
            image_processing::calculate_auto_adjustments,
            auto_level::auto_level,
            mask_generation::generate_mask_overlay,
            file_management::update_exif_fields,
            file_management::get_supported_file_types,
            file_management::load_video_info,
            file_management::video_stream_url,
            file_management::save_video_frame,
            sky_commands::list_sky_plates,
            sky_commands::prepare_sky_replacement,
            sky_commands::preview_sky_replacement,
            sky_commands::apply_sky_replacement,
            file_management::read_exif_for_paths,
            file_management::list_images_in_dir,
            file_management::list_images_recursive,
            file_management::get_folder_tree,
            file_management::get_folder_children,
            file_management::get_pinned_folder_trees,
            file_management::update_thumbnail_queue,
            file_management::create_folder,
            file_management::delete_folder,
            file_management::copy_files,
            file_management::move_files,
            file_management::rename_folder,
            file_management::rename_files,
            file_management::duplicate_file,
            file_management::show_in_finder,
            file_management::delete_files_from_disk,
            file_management::delete_files_with_associated,
            file_management::save_metadata_and_update_thumbnail,
            file_management::apply_adjustments_to_paths,
            file_management::load_metadata,
            file_management::load_presets,
            file_management::save_presets,
            file_management::get_or_create_internal_library_root,
            file_management::reset_adjustments_for_paths,
            file_management::apply_auto_adjustments_to_paths,
            file_management::handle_import_presets_from_file,
            file_management::handle_import_legacy_presets_from_file,
            file_management::handle_export_presets_to_file,
            file_management::save_community_preset,
            file_management::clear_all_sidecars,
            file_management::clear_thumbnail_cache,
            file_management::set_color_label_for_paths,
            file_management::set_rating_for_paths,
            file_management::import_files,
            file_management::create_virtual_copy,
            file_management::get_albums,
            file_management::save_albums,
            file_management::add_to_album,
            file_management::get_album_images,
            tagging::start_background_indexing,
            tagging::clear_ai_tags,
            tagging::clear_all_tags,
            tagging::add_tag_for_paths,
            tagging::remove_tag_for_paths,
            culling::cull_images,
            lens_correction::get_lensfun_makers,
            lens_correction::get_lensfun_lenses_for_maker,
            lens_correction::autodetect_lens,
            lens_correction::get_lens_distortion_params,
            negative_conversion::preview_negative_conversion,
            negative_conversion::convert_negatives,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(#[allow(unused_variables)] |app_handle, event| {
            match event {
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Opened { urls } => {
                    if let Some(url) = urls.first()
                        && let Ok(path) = url.to_file_path()
                        && let Some(path_str) = path.to_str()
                    {
                        let state = app_handle.state::<AppState>();
                        *state.initial_file_path.lock().unwrap() = Some(path_str.to_string());
                        log::info!("macOS initial open: Stored path {} for later.", path_str);
                    }
                }
                tauri::RunEvent::ExitRequested { api, .. } => {
                    api.prevent_exit();

                    // The app force-exits below; make sure the managed
                    // generative engine doesn't outlive it.
                    comfy_engine::stop(&app_handle.state::<AppState>().comfy_process);

                    #[cfg(target_os = "macos")]
                    unsafe { libc::_exit(0); }

                    #[cfg(not(target_os = "macos"))]
                    std::process::exit(0);
                }
                tauri::RunEvent::Exit => {
                    comfy_engine::stop(&app_handle.state::<AppState>().comfy_process);

                    #[cfg(target_os = "macos")]
                    unsafe { libc::_exit(0); }

                    #[cfg(not(target_os = "macos"))]
                    std::process::exit(0);
                }
                _ => {}
            }
        });
}
