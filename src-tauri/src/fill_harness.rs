//! Headless entry points into the app's real fill path, for
//! `examples/fill_testset.rs`. Nothing here reimplements a fill: it opens a
//! photo the way the editor holds one and calls the same Tauri command the
//! Retouch panel invokes. Not used by the app itself.

use std::sync::Arc;

use image::DynamicImage;
use serde_json::Value;
use tauri::Manager;

use crate::app_settings::AppSettings;
use crate::app_state::{AppState, LoadedImage};

/// Makes `image` the open photo, as opening it in the editor does.
pub fn open_photo(app: &tauri::AppHandle, path: &str, image: DynamicImage) {
    let state = app.state::<AppState>();
    *state.full_warped_cache.lock().unwrap() = None;
    *state.original_image.lock().unwrap() = Some(LoadedImage::new(
        path.to_string(),
        Some(Arc::new(image)),
        AppSettings::default(),
        false,
        false,
    ));
}

/// Runs `invoke_generative_replace_with_mask_def` exactly as the Retouch
/// panel does. `patch` is the patch as the frontend sends it (camelCase).
pub async fn run_fill(
    app: &tauri::AppHandle,
    path: &str,
    patch: Value,
    current_adjustments: Value,
    use_fast_inpaint: bool,
    fill_engine: Option<&str>,
) -> Result<String, String> {
    let patch_definition =
        serde_json::from_value(patch).map_err(|e| format!("bad patch definition: {e}"))?;
    crate::ai_commands::invoke_generative_replace_with_mask_def(
        path.to_string(),
        patch_definition,
        current_adjustments,
        use_fast_inpaint,
        None,
        fill_engine.map(str::to_string),
        app.clone(),
        app.state::<AppState>(),
    )
    .await
}
