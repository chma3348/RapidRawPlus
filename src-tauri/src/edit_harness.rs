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
/// photo's pixels, then honed by `clicks` (photo pixels; `true` includes, as
/// a click does, `false` excludes, as a Shift-click does).
pub async fn subject_mask(
    app: &tauri::AppHandle,
    path: &str,
    edits: Value,
    start: (f64, f64),
    end: (f64, f64),
    clicks: &[(f64, f64, bool)],
) -> Result<Value, String> {
    use crate::subject_selection::SubjectPoint;
    // As the panel sends a box with clicks: its corners (labels 2 and 3),
    // then each click.
    let points = (!clicks.is_empty()).then(|| {
        let mut points = vec![
            SubjectPoint {
                x: start.0,
                y: start.1,
                label: 2,
            },
            SubjectPoint {
                x: end.0,
                y: end.1,
                label: 3,
            },
        ];
        points.extend(clicks.iter().map(|&(x, y, include)| SubjectPoint {
            x,
            y,
            label: include as u8,
        }));
        points
    });
    let p = crate::ai_commands::generate_ai_subject_mask(
        edits,
        path.to_string(),
        start,
        end,
        0.0,
        false,
        false,
        0,
        points,
        None,
        None,
        app.state::<AppState>(),
        app.clone(),
    )
    .await?;
    serde_json::to_value(p).map_err(|e| e.to_string())
}

/// Mask number `index` of `edits` exactly as renders place it, over the
/// whole photo as shown (uncropped, at its own pixels).
pub fn mask_bitmap(
    app: &tauri::AppHandle,
    edits: &Value,
    index: usize,
) -> Result<image::GrayImage, String> {
    use crate::mask_generation::{MaskDefinition, Orientation};
    use image::GenericImageView;
    let state = app.state::<AppState>();
    let masks: Vec<MaskDefinition> =
        serde_json::from_value(edits["masks"].clone()).map_err(|e| e.to_string())?;
    let mask = masks.get(index).ok_or("No such mask")?;
    let photo = Orientation::of_photo(edits);
    let (w, h) = {
        let image = state.original_image.lock().unwrap();
        image
            .as_ref()
            .ok_or("No photo open")?
            .pixels()?
            .dimensions()
    };
    let (w, h) = if photo.orientation_steps % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    };
    let warped = crate::mask_generation::resolve_warped_image_for_masks(
        &state,
        edits,
        std::slice::from_ref(mask),
    );
    crate::mask_generation::generate_photo_mask_bitmap(
        mask,
        w,
        h,
        1.0,
        (0.0, 0.0),
        warped.as_deref(),
        photo,
    )
    .ok_or_else(|| "The mask is hidden or empty".to_string())
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

/// Every Subject matte saved in `edits`, re-fitted to the photo's edges as
/// new Subject masks now are (`subject_selection::snap_saved_matte`). Returns
/// how many distinct mattes were refitted.
pub fn snap_saved_subjects(app: &tauri::AppHandle, edits: &mut Value) -> Result<usize, String> {
    use base64::Engine as _;
    let state = app.state::<AppState>();
    let neutral = serde_json::json!({"processVersion": 3, "toneMapper": "resolve"});
    let guide = crate::get_cached_full_warped_image(&state, &neutral)?.to_rgb8();
    let engine = base64::engine::general_purpose::STANDARD;
    let mut done: std::collections::HashMap<String, String> = Default::default();
    let masks = edits["masks"].as_array_mut().ok_or("No masks")?;
    for part in masks
        .iter_mut()
        .filter_map(|m| m["subMasks"].as_array_mut())
        .flatten()
        .filter(|p| p["type"] == "ai-subject")
    {
        let Some(old) = part["parameters"]["maskDataBase64"]
            .as_str()
            .map(str::to_string)
        else {
            continue;
        };
        if !done.contains_key(&old) {
            let bytes = engine
                .decode(old.split_once(',').map_or(old.as_str(), |(_, b)| b))
                .map_err(|e| e.to_string())?;
            let mut matte = image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())?
                .to_luma8();
            if matte.dimensions() != guide.dimensions() {
                return Err(format!(
                    "Matte {:?} doesn't match the photo {:?}",
                    matte.dimensions(),
                    guide.dimensions()
                ));
            }
            crate::subject_selection::snap_saved_matte(&mut matte, &guide);
            let mut png = std::io::Cursor::new(Vec::new());
            matte
                .write_to(&mut png, image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            let url = format!("data:image/png;base64,{}", engine.encode(png.get_ref()));
            done.insert(old.clone(), url);
        }
        part["parameters"]["maskDataBase64"] = Value::String(done[&old].clone());
    }
    Ok(done.len())
}
