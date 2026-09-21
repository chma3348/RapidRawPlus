//! Sky Replace, as the app reaches it.
//!
//! `sky_replace.rs` is the compositor: un-mixing the old sky out of fine
//! edges, placing the plate on the horizon, matching and grading its light.
//! This is what connects it to an edit: the plate library, a fast preview
//! while you choose, and a full-resolution result delivered as a patch.
//!
//! **Why a patch.** Patches already composite in both engines, sit before
//! geometry and every adjustment, and already have visibility, opacity,
//! feather and undo. A replaced sky is exactly that kind of thing: pixels
//! produced once, then edited on like the rest of the photograph.
//!
//! **Which pixels it replaces.** The compositor relights the foreground
//! toward the new sky's colour, and adds haze near the horizon, so with
//! either on the whole frame changes and the patch covers the whole frame.
//! With both off only the sky and its edge band change, and the patch is
//! limited to them. A whole-frame patch is stored as JPEG at quality 95
//! rather than PNG: at 24 megapixels a PNG runs to tens of megabytes in the
//! sidecar, which is rewritten on every edit.
//!
//! **RAW.** Patches from float sources are stored through a 1/2.4 curve, as
//! the fill tool does, so the compositor works on that encoding of the scene
//! and the patch is tagged `gamma`.

use crate::app_state::AppState;
use crate::model_registry::{TaskType, mask_subtype_filter, resolve_and_prepare};
use crate::sky_replace::{SkyReplaceOptions, replace_sky};
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GrayImage, ImageFormat, Rgb, RgbImage, imageops};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;

const PREVIEW_EDGE: u32 = 960;
const THUMB_EDGE: u32 = 320;
/// The curve patches from float sources are stored through.
const STORED_GAMMA: f32 = crate::ai_processing::LAMA_GAMMA;

/// Everything a sky replacement needs about the current photograph, worked
/// out once — the sky mask is the slow part — and reused while you choose a
/// plate and move sliders.
pub struct SkySession {
    path: String,
    orientation: (u8, bool, bool),
    gamma: bool,
    base: Arc<RgbImage>,
    alpha: Arc<GrayImage>,
    preview_base: Arc<RgbImage>,
    preview_alpha: Arc<GrayImage>,
    plate: Option<(String, Arc<DynamicImage>)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkyPlate {
    file: String,
    look: String,
    title: String,
    author: String,
    licence: String,
    source: String,
    width: u32,
    height: u32,
    thumbnail: String,
}

#[derive(Deserialize)]
struct Library {
    plates: Vec<LibraryPlate>,
}

#[derive(Deserialize)]
struct LibraryPlate {
    file: String,
    look: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    licence: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkyPreparation {
    /// Share of the frame the mask calls sky.
    coverage: f32,
    raw: bool,
}

fn skies_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app.path().app_data_dir().map_err(|e| e.to_string())?.join("skies"))
}

/// The plate library, with small thumbnails made on first use and kept.
#[tauri::command]
pub async fn list_sky_plates(app_handle: tauri::AppHandle) -> Result<Vec<SkyPlate>, String> {
    let dir = skies_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || {
        let library: Library = serde_json::from_slice(
            &std::fs::read(dir.join("library.json"))
                .map_err(|e| format!("The sky library is not installed ({e})"))?,
        )
        .map_err(|e| format!("The sky library index is unreadable: {e}"))?;
        let thumbs = dir.join(".thumbs");
        std::fs::create_dir_all(&thumbs).map_err(|e| e.to_string())?;
        use rayon::prelude::*;
        let plates = library
            .plates
            .into_par_iter()
            .filter_map(|p| {
                let full = dir.join(&p.file);
                if !full.is_file() {
                    return None;
                }
                let thumb = thumbs.join(format!("{}.jpg", p.file));
                if !thumb.is_file() {
                    let image = image::open(&full).ok()?;
                    image.thumbnail(THUMB_EDGE, THUMB_EDGE).to_rgb8().save(&thumb).ok()?;
                }
                Some(SkyPlate {
                    file: p.file,
                    look: p.look,
                    title: p.title,
                    author: p.author,
                    licence: p.licence,
                    source: p.source,
                    width: p.width,
                    height: p.height,
                    thumbnail: thumb.to_string_lossy().into_owned(),
                })
            })
            .collect();
        Ok(plates)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Find the sky in the current photograph and hold everything a replacement
/// needs. Slow the first time for a photograph; free after that.
#[tauri::command]
pub async fn prepare_sky_replacement(
    path: String,
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<SkyPreparation, String> {
    let orientation = (orientation_steps, flip_horizontal, flip_vertical);
    if let Some(s) = state.sky_session.lock().unwrap().as_ref()
        && s.path == path
        && s.orientation == orientation
    {
        return Ok(SkyPreparation {
            coverage: coverage(&s.alpha),
            raw: s.gamma,
        });
    }
    let (image, is_raw) = crate::get_full_image_for_processing(&state)?;
    // The mask model wants something that looks like a photograph, which a
    // linear RAW does not until it has had a default development.
    let mut for_mask = image.clone();
    if is_raw {
        crate::image_processing::apply_cpu_default_raw_processing(&mut for_mask);
    }
    let o = crate::scene_masks::Orientation {
        steps: orientation_steps,
        flip_horizontal,
        flip_vertical,
    };
    let (registry, model) = resolve_and_prepare(
        &app_handle,
        &state.model_registry,
        TaskType::Mask,
        "mask_scene",
        mask_subtype_filter("scene"),
    )
    .await
    .map_err(|e| format!("Sky Replace needs the scene model: {e}"))?;
    let session = registry
        .get_session(&model.manifest.id, None)
        .map_err(|e| e.to_string())?;
    let found = tauri::async_runtime::spawn_blocking(move || {
        crate::scene_masks::sky_mask_scene(&for_mask, &session, o)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?
    .ok_or("No sky was found in this photograph.")?;
    let base = if is_raw {
        let rgb = image.to_rgb32f();
        RgbImage::from_fn(rgb.width(), rgb.height(), |x, y| {
            let p = rgb.get_pixel(x, y);
            Rgb(p.0.map(|v| (v.clamp(0.0, 1.0).powf(1.0 / STORED_GAMMA) * 255.0).round() as u8))
        })
    } else {
        image.to_rgb8()
    };
    let (w, h) = base.dimensions();
    let (pw, ph) = fit(w, h, PREVIEW_EDGE);
    let preview_base = imageops::resize(&base, pw, ph, imageops::FilterType::Triangle);
    let preview_alpha = imageops::resize(&found.mask, pw, ph, imageops::FilterType::Triangle);
    let result = SkyPreparation {
        coverage: found.coverage,
        raw: is_raw,
    };
    *state.sky_session.lock().unwrap() = Some(SkySession {
        path,
        orientation,
        gamma: is_raw,
        base: Arc::new(base),
        alpha: Arc::new(found.mask),
        preview_base: Arc::new(preview_base),
        preview_alpha: Arc::new(preview_alpha),
        plate: None,
    });
    Ok(result)
}

/// A quick look at a plate in the photograph, for choosing. Returns a JPEG
/// data URL of the photograph as shot with the new sky — before any of the
/// edit's adjustments, which apply once the sky is in.
#[tauri::command]
pub async fn preview_sky_replacement(
    plate: String,
    options: SkyReplaceOptions,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let (base, alpha, gamma, plate_image) = session_inputs(&state, &app_handle, &plate, true)?;
    tauri::async_runtime::spawn_blocking(move || {
        let result = replace_sky(&DynamicImage::ImageRgb8((*base).clone()), &alpha, &plate_image, &options)
            .map_err(|e| e.to_string())?
            .to_rgb8();
        // A RAW's stored encoding is not a display encoding; show it as one
        // for choosing, which is all this is for.
        let shown = if gamma { gamma_to_display(&result) } else { result };
        let mut bytes = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
            .encode_image(&shown)
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "data:image/jpeg;base64,{}",
            general_purpose::STANDARD.encode(bytes.get_ref())
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The replacement at full resolution, as patch data for `aiPatches`.
#[tauri::command]
pub async fn apply_sky_replacement(
    plate: String,
    options: SkyReplaceOptions,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let (base, alpha, gamma, plate_image) = session_inputs(&state, &app_handle, &plate, false)?;
    tauri::async_runtime::spawn_blocking(move || {
        let result = replace_sky(&DynamicImage::ImageRgb8((*base).clone()), &alpha, &plate_image, &options)
            .map_err(|e| e.to_string())?
            .to_rgb8();
        encode_patch(&base, &result, &alpha, &options, gamma)
    })
    .await
    .map_err(|e| e.to_string())?
}

type Inputs = (Arc<RgbImage>, Arc<GrayImage>, bool, Arc<DynamicImage>);

fn session_inputs(
    state: &tauri::State<'_, AppState>,
    app: &tauri::AppHandle,
    plate: &str,
    preview: bool,
) -> Result<Inputs, String> {
    // A plate name is a file in the library, never a path.
    if plate.contains('/') || plate.contains('\\') || plate.starts_with('.') {
        return Err("Not a sky plate".into());
    }
    let mut guard = state.sky_session.lock().unwrap();
    let session = guard
        .as_mut()
        .ok_or("Prepare the sky first: open Sky Replace on a photograph.")?;
    let plate_image = match &session.plate {
        Some((name, image)) if name == plate => image.clone(),
        _ => {
            let image = Arc::new(
                image::open(skies_dir(app)?.join(plate)).map_err(|e| format!("Could not open {plate}: {e}"))?,
            );
            session.plate = Some((plate.to_string(), image.clone()));
            image
        }
    };
    Ok(if preview {
        (session.preview_base.clone(), session.preview_alpha.clone(), session.gamma, plate_image)
    } else {
        (session.base.clone(), session.alpha.clone(), session.gamma, plate_image)
    })
}

/// Patch data in the shape the fill tool writes.
fn encode_patch(
    base: &RgbImage,
    result: &RgbImage,
    alpha: &GrayImage,
    options: &SkyReplaceOptions,
    gamma: bool,
) -> Result<serde_json::Value, String> {
    let (w, h) = base.dimensions();
    let whole_frame = options.relight > 0.001 || options.haze > 0.001;
    let (color_bytes, mask) = if whole_frame {
        let mut bytes = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(result)
            .map_err(|e| e.to_string())?;
        (bytes.into_inner(), GrayImage::from_pixel(w, h, image::Luma([255])))
    } else {
        // Only the sky and its edge band changed. Widen the mask a little so
        // the un-mixed rim — foreground pixels that lost their old-sky tint —
        // is carried by the patch too.
        let mask = grow(alpha, ((w.max(h) as f32) * 0.004).ceil() as u32);
        let colour = RgbImage::from_fn(w, h, |x, y| {
            if mask.get_pixel(x, y)[0] > 0 { *result.get_pixel(x, y) } else { Rgb([0, 0, 0]) }
        });
        let mut bytes = Cursor::new(Vec::new());
        colour.write_to(&mut bytes, ImageFormat::Png).map_err(|e| e.to_string())?;
        (bytes.into_inner(), mask)
    };
    let mut mask_bytes = Cursor::new(Vec::new());
    mask.write_to(&mut mask_bytes, ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "color": general_purpose::STANDARD.encode(&color_bytes),
        "mask": general_purpose::STANDARD.encode(mask_bytes.get_ref()),
        "encoding": if gamma { "gamma" } else { "linear" },
    }))
}

fn coverage(alpha: &GrayImage) -> f32 {
    alpha.pixels().filter(|p| p[0] > 127).count() as f32 / alpha.pixels().len().max(1) as f32
}

fn fit(w: u32, h: u32, edge: u32) -> (u32, u32) {
    let s = (edge as f32 / w.max(h) as f32).min(1.0);
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

/// Any coverage within `radius` pixels becomes full coverage.
fn grow(alpha: &GrayImage, radius: u32) -> GrayImage {
    let binary = GrayImage::from_fn(alpha.width(), alpha.height(), |x, y| {
        image::Luma([if alpha.get_pixel(x, y)[0] > 0 { 255 } else { 0 }])
    });
    imageproc::morphology::dilate(&binary, imageproc::distance_transform::Norm::LInf, radius.min(255) as u8)
}

fn gamma_to_display(stored: &RgbImage) -> RgbImage {
    RgbImage::from_fn(stored.width(), stored.height(), |x, y| {
        Rgb(stored.get_pixel(x, y).0.map(|v| {
            let linear = (v as f32 / 255.0).powf(STORED_GAMMA);
            let encoded = if linear <= 0.0031308 {
                12.92 * linear
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sky_photo() -> (RgbImage, GrayImage) {
        let photo = RgbImage::from_fn(64, 48, |_, y| {
            if y < 24 { Rgb([200, 210, 220]) } else { Rgb([60, 80, 40]) }
        });
        let alpha = GrayImage::from_fn(64, 48, |_, y| image::Luma([if y < 24 { 255 } else { 0 }]));
        (photo, alpha)
    }

    fn decode(v: &serde_json::Value, key: &str) -> DynamicImage {
        image::load_from_memory(&general_purpose::STANDARD.decode(v[key].as_str().unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn a_sky_only_patch_leaves_the_foreground_out() {
        let (photo, alpha) = sky_photo();
        let result = RgbImage::from_pixel(64, 48, Rgb([10, 20, 200]));
        let options = SkyReplaceOptions { relight: 0.0, haze: 0.0, ..SkyReplaceOptions::as_shot() };
        let patch = encode_patch(&photo, &result, &alpha, &options, false).unwrap();
        let mask = decode(&patch, "mask").to_luma8();
        assert_eq!(mask.get_pixel(10, 5)[0], 255, "sky must be covered");
        assert_eq!(mask.get_pixel(10, 45)[0], 0, "deep foreground must not be");
        assert_eq!(patch["encoding"], "linear");
    }

    #[test]
    fn relighting_makes_a_whole_frame_patch() {
        let (photo, alpha) = sky_photo();
        let result = RgbImage::from_pixel(64, 48, Rgb([10, 20, 200]));
        let patch = encode_patch(&photo, &result, &alpha, &SkyReplaceOptions::auto_match(), true).unwrap();
        let mask = decode(&patch, "mask").to_luma8();
        assert!(mask.pixels().all(|p| p[0] == 255), "relight changes the foreground too");
        assert_eq!(patch["encoding"], "gamma");
        let colour = decode(&patch, "color").to_rgb8();
        assert_eq!(colour.dimensions(), (64, 48));
    }

    #[test]
    fn the_edge_band_is_carried() {
        let (_, alpha) = sky_photo();
        let grown = grow(&alpha, 3);
        assert_eq!(grown.get_pixel(10, 26)[0], 255, "rim below the horizon should be included");
        assert_eq!(grown.get_pixel(10, 40)[0], 0);
    }
}
