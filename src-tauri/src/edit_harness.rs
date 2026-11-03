//! Headless entry points into the editor's own tools, for
//! `examples/edit_session.rs`: opening a photo the way the editor holds one,
//! the AI masks and Sky Replace exactly as their panels invoke them, and
//! renders as an export makes them. Nothing here reimplements a tool. Not
//! used by the app itself.

use std::sync::Arc;

use serde_json::Value;
use tauri::Manager;

use crate::app_settings::AppSettings;
use crate::app_state::{AppState, LoadedImage};

/// Makes the photo at `path` the open photo, as opening it in the editor
/// does (a RAW decoded when a tool first asks). Returns its size as the
/// tools see it.
pub fn open(app: &tauri::AppHandle, path: &str) -> Result<(u32, u32), String> {
    let state = app.state::<AppState>();
    *state.original_image.lock().unwrap() = None;
    *state.full_warped_cache.lock().unwrap() = None;
    *state.full_transformed_cache.lock().unwrap() = None;
    *state.sky_session.lock().unwrap() = None;
    state.patch_cache.lock().unwrap().clear();
    state.geometry_cache.lock().unwrap().clear();
    let is_raw = crate::formats::is_raw_file(path);
    let pixels = if is_raw {
        None
    } else {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Some(Arc::new(
            crate::image_loader::load_base_image_from_bytes(
                &bytes,
                path,
                false,
                &AppSettings::default(),
                None,
            )
            .map_err(|e| e.to_string())?,
        ))
    };
    let loaded = LoadedImage::new(
        path.to_string(),
        pixels,
        AppSettings::default(),
        is_raw,
        false,
    );
    let size = {
        use image::GenericImageView;
        loaded.pixels()?.dimensions()
    };
    *state.original_image.lock().unwrap() = Some(loaded);
    Ok(size)
}

/// The Masks panel's Sky.
pub async fn sky_mask(app: &tauri::AppHandle, edits: Value) -> Result<Value, String> {
    let p = crate::ai_commands::generate_ai_sky_mask(
        edits,
        0.0,
        false,
        false,
        0,
        app.state::<AppState>(),
        app.clone(),
    )
    .await?;
    serde_json::to_value(p).map_err(|e| e.to_string())
}

/// The Masks panel's Subject, whole-subject (no box drawn).
pub async fn foreground_mask(
    app: &tauri::AppHandle,
    path: &str,
    edits: Value,
) -> Result<Value, String> {
    let p = crate::ai_commands::generate_ai_foreground_mask(
        edits,
        path.to_string(),
        None,
        0.0,
        false,
        false,
        0,
        app.state::<AppState>(),
        app.clone(),
    )
    .await?;
    serde_json::to_value(p).map_err(|e| e.to_string())
}

/// The Masks panel's Subject with a box drawn from `start` to `end`, in the
/// photo's pixels.
pub async fn subject_mask(
    app: &tauri::AppHandle,
    path: &str,
    edits: Value,
    start: (f64, f64),
    end: (f64, f64),
) -> Result<Value, String> {
    let p = crate::ai_commands::generate_ai_subject_mask(
        edits,
        path.to_string(),
        start,
        end,
        0.0,
        false,
        false,
        0,
        None,
        None,
        None,
        app.state::<AppState>(),
        app.clone(),
    )
    .await?;
    serde_json::to_value(p).map_err(|e| e.to_string())
}

/// Sky Replace: detect, hone the sky with the panel's brush `strokes` (if
/// any), then apply `plate` at full size with `options`. Returns the patch
/// data, as the panel puts it in the sky patch, and the sky mask it used (a
/// PNG data URL).
pub async fn sky_replace(
    app: &tauri::AppHandle,
    path: &str,
    plate: &str,
    options: Value,
    strokes: Value,
) -> Result<(Value, String), String> {
    let found = crate::sky_commands::prepare_sky_replacement(
        path.to_string(),
        0,
        false,
        false,
        Some(false),
        app.state::<AppState>(),
        app.clone(),
    )
    .await?
    .ok_or("No sky found")?;
    let mut mask = serde_json::to_value(&found).map_err(|e| e.to_string())?["mask"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let strokes: Vec<crate::sky_commands::SkyStroke> =
        serde_json::from_value(strokes).map_err(|e| e.to_string())?;
    if !strokes.is_empty() {
        let refined =
            crate::sky_commands::refine_sky_mask(strokes, 1, app.state::<AppState>()).await?;
        if let Some(r) = refined {
            mask = serde_json::to_value(&r).map_err(|e| e.to_string())?["mask"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
    }
    let options = serde_json::from_value(options).map_err(|e| e.to_string())?;
    let patch = crate::sky_commands::apply_sky_replacement(
        plate.to_string(),
        options,
        app.state::<AppState>(),
        app.clone(),
    )
    .await?;
    Ok((patch, mask))
}
