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
//! limited to them. Colour is stored as JPEG at quality 95 either way — a
//! half-sky 33-megapixel frame came to 32 MB as PNG, in a sidecar that is
//! rewritten on every edit — and the mask as lossless PNG.
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
/// Long edge of the sky shown on the photograph itself while choosing: about
/// what the editor's view shows, so switching skies stays quick. Apply makes
/// the full-size one.
const CANVAS_EDGE: u32 = 2560;
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
    /// The sky as detected, before any brushing.
    detected: Arc<GrayImage>,
    /// The sky as replaced: the detection with the brush strokes on it.
    alpha: Arc<GrayImage>,
    /// The strokes `alpha` has on it, so the next one paints on top.
    strokes: Vec<SkyStroke>,
    /// The newest brushing written to `alpha`: a slower, older one finishing
    /// late never overwrites it.
    refine_seq: u64,
    preview_base: Arc<RgbImage>,
    preview_alpha: Arc<GrayImage>,
    canvas_base: Arc<RgbImage>,
    canvas_alpha: Arc<GrayImage>,
    /// Ids of the latest skies shown on the photo, newest last: older ones'
    /// pixels are dropped from the patch cache as more are tried.
    shown_on_photo: Vec<String>,
    plate: Option<(String, Arc<DynamicImage>)>,
    /// For a RAW, how its light comes out on screen (to bring a plate in).
    look: Option<Arc<RawLook>>,
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
    /// The sky mask, full size in the photo's stored orientation, as a PNG
    /// data URL: what an AI sky mask carries, so the editor can show it in
    /// red the same way.
    mask: String,
}

fn mask_data_url(mask: &GrayImage) -> Result<String, String> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    // Fast compression: it is sent again after every brush stroke, and a
    // mask is mostly flat, so it stays small either way.
    let mut buf = Cursor::new(Vec::new());
    PngEncoder::new_with_quality(&mut buf, CompressionType::Fast, FilterType::Sub)
        .write_image(
            mask.as_raw(),
            mask.width(),
            mask.height(),
            image::ExtendedColorType::L8,
        )
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(buf.get_ref())
    ))
}

fn skies_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("skies"))
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
        // Not a hidden folder: the web view may only load files outside
        // hidden folders (which keeps it out of the likes of ~/.ssh).
        let thumbs = dir.join("thumbnails");
        std::fs::create_dir_all(&thumbs).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(dir.join(".thumbs"));
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
                    image
                        .thumbnail(THUMB_EDGE, THUMB_EDGE)
                        .to_rgb8()
                        .save(&thumb)
                        .ok()?;
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
    cached_only: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Option<SkyPreparation>, String> {
    let orientation = (orientation_steps, flip_horizontal, flip_vertical);
    if let Some(s) = state.sky_session.lock().unwrap().as_ref()
        && s.path == path
        && s.orientation == orientation
    {
        return Ok(Some(SkyPreparation {
            coverage: coverage(&s.alpha),
            raw: s.gamma,
            mask: mask_data_url(&s.alpha)?,
        }));
    }
    // Reopening the panel: pick up a sky already found, never start looking.
    if cached_only == Some(true) {
        return Ok(None);
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
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    log::info!(
        "sky: detecting in {name} (turns {orientation_steps}, flips {flip_horizontal}/{flip_vertical})"
    );
    let found = tauri::async_runtime::spawn_blocking(move || {
        crate::scene_masks::sky_mask_scene(&for_mask, &session, o)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?
    .ok_or(
        "No sky was found in this photograph. If the photo is sideways or upside down, \
         straighten it first (Crop & Rotate): the sky has to be at the top.",
    )?;
    let base = if is_raw {
        let rgb = image.to_rgb32f();
        RgbImage::from_fn(rgb.width(), rgb.height(), |x, y| {
            let p = rgb.get_pixel(x, y);
            Rgb(p
                .0
                .map(|v| (v.clamp(0.0, 1.0).powf(1.0 / STORED_GAMMA) * 255.0).round() as u8))
        })
    } else {
        image.to_rgb8()
    };
    // How this RAW's light comes out on screen, from a quick neutral render,
    // so sky plates can be brought into it.
    let look = if is_raw {
        let context = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle)?;
        let shown = crate::color_engine::application::render_for_output(
            &context,
            &state,
            &path,
            &serde_json::json!({"processVersion": 3, "v3": {}}),
            Some(1024),
        )
        .map_err(|e| format!("Could not render the photo to match the sky to it: {e:#}"))?;
        let shown = DynamicImage::ImageRgba8(shown.preview_rgba8()).to_rgb8();
        Some(Arc::new(RawLook::learn(&base, &shown)))
    } else {
        None
    };
    let (w, h) = base.dimensions();
    let (pw, ph) = fit(w, h, PREVIEW_EDGE);
    let preview_base = imageops::resize(&base, pw, ph, imageops::FilterType::Triangle);
    let preview_alpha = imageops::resize(&found.mask, pw, ph, imageops::FilterType::Triangle);
    let (cw, ch) = fit(w, h, CANVAS_EDGE);
    let canvas_base = imageops::resize(&base, cw, ch, imageops::FilterType::Triangle);
    let canvas_alpha = imageops::resize(&found.mask, cw, ch, imageops::FilterType::Triangle);
    let result = SkyPreparation {
        coverage: found.coverage,
        raw: is_raw,
        mask: mask_data_url(&found.mask)?,
    };
    *state.sky_session.lock().unwrap() = Some(SkySession {
        path,
        orientation,
        gamma: is_raw,
        base: Arc::new(base),
        detected: Arc::new(found.mask.clone()),
        alpha: Arc::new(found.mask),
        strokes: Vec::new(),
        refine_seq: 0,
        preview_base: Arc::new(preview_base),
        preview_alpha: Arc::new(preview_alpha),
        canvas_base: Arc::new(canvas_base),
        canvas_alpha: Arc::new(canvas_alpha),
        shown_on_photo: Vec::new(),
        plate: None,
        look,
    });
    Ok(Some(result))
}

/// One stroke of the sky brush.
#[derive(Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkyStroke {
    /// Takes sky away rather than adding it.
    #[serde(default)]
    erase: bool,
    /// Brush diameter, as a share of the photo's long edge.
    size: f32,
    /// 0 is a hard edge; 1 is soft all the way to the centre.
    #[serde(default)]
    feather: f32,
    /// Where it went, as fractions of the photo as it is shown — turned,
    /// flipped and straightened, before the crop — with the way up it was
    /// painted under, so turning the photo later keeps it in place.
    points: Vec<StrokePoint>,
    #[serde(default)]
    orientation_steps: u32,
    #[serde(default)]
    flip_horizontal: bool,
    #[serde(default)]
    flip_vertical: bool,
    #[serde(default)]
    rotation: f64,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
pub struct StrokePoint {
    x: f64,
    y: f64,
}

/// The detected sky with the brush strokes painted on, in order: adding
/// covers toward full sky, erasing takes coverage away, each as soft as its
/// feather. In the photo's stored orientation, as the mask is.
pub fn paint_strokes(detected: &GrayImage, strokes: &[SkyStroke]) -> GrayImage {
    let mut alpha = detected.clone();
    paint_onto(&mut alpha, strokes);
    alpha
}

fn paint_onto(alpha: &mut GrayImage, strokes: &[SkyStroke]) {
    let (w, h) = alpha.dimensions();
    let long = w.max(h) as f64;
    for stroke in strokes {
        let r = (stroke.size as f64 * long / 2.0).max(0.5);
        let core = r * (1.0 - stroke.feather.clamp(0.0, 1.0) as f64);
        let steps = stroke.orientation_steps;
        let (shown_w, shown_h) = if steps % 2 == 1 {
            (h as f64, w as f64)
        } else {
            (w as f64, h as f64)
        };
        let points: Vec<(f64, f64)> = stroke
            .points
            .iter()
            .map(|p| {
                crate::ai_commands::display_to_image_point(
                    p.x * shown_w,
                    p.y * shown_h,
                    w as f64,
                    h as f64,
                    steps,
                    stroke.flip_horizontal,
                    stroke.flip_vertical,
                    stroke.rotation,
                )
            })
            .collect();
        if points.is_empty() {
            continue;
        }
        // Dabs along the path, close enough that the edge stays smooth.
        let spacing = (r * 0.25).max(0.5);
        let mut dabs = vec![points[0]];
        for pair in points.windows(2) {
            let ((ax, ay), (bx, by)) = (pair[0], pair[1]);
            let n = ((bx - ax).hypot(by - ay) / spacing).ceil() as usize;
            for i in 1..=n {
                let t = i as f64 / n as f64;
                dabs.push((ax + (bx - ax) * t, ay + (by - ay) * t));
            }
        }
        let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in &dabs {
            (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(x), lo_y.min(y), hi_x.max(x), hi_y.max(y));
        }
        let x0 = (lo_x - r).floor().max(0.0) as u32;
        let y0 = (lo_y - r).floor().max(0.0) as u32;
        let x1 = ((hi_x + r).ceil() as i64).min(w as i64 - 1);
        let y1 = ((hi_y + r).ceil() as i64).min(h as i64 - 1);
        if x1 < x0 as i64 || y1 < y0 as i64 {
            continue;
        }
        let (x1, y1) = (x1 as u32, y1 as u32);
        let bw = (x1 - x0 + 1) as usize;
        let mut cover = vec![0f32; bw * (y1 - y0 + 1) as usize];
        for &(cx, cy) in &dabs {
            let ya = ((cy - r).floor().max(y0 as f64) as u32).max(y0);
            let yb = ((cy + r).ceil().min(y1 as f64)) as u32;
            let xa = ((cx - r).floor().max(x0 as f64) as u32).max(x0);
            let xb = ((cx + r).ceil().min(x1 as f64)) as u32;
            for y in ya..=yb {
                for x in xa..=xb {
                    let d = (x as f64 + 0.5 - cx).hypot(y as f64 + 0.5 - cy);
                    if d >= r {
                        continue;
                    }
                    let v = if d <= core {
                        1.0
                    } else {
                        let t = (r - d) / (r - core);
                        t * t * (3.0 - 2.0 * t)
                    } as f32;
                    let c = &mut cover[(y - y0) as usize * bw + (x - x0) as usize];
                    *c = c.max(v);
                }
            }
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                let c = cover[(y - y0) as usize * bw + (x - x0) as usize];
                if c <= 0.0 {
                    continue;
                }
                let p = alpha.get_pixel_mut(x, y);
                let a = p[0] as f32 / 255.0;
                let a = if stroke.erase {
                    a * (1.0 - c)
                } else {
                    a + (1.0 - a) * c
                };
                p[0] = (a * 255.0).round() as u8;
            }
        }
    }
}

/// The detected sky with the brush strokes on it, held as the sky to
/// replace. A list that only adds strokes paints just the new ones on the
/// last result; anything else (an undo, a clear) starts again from the
/// detection. Returns the new mask to show, or nothing
/// when a newer brushing (or a new detection) came first.
#[tauri::command]
pub async fn refine_sky_mask(
    strokes: Vec<SkyStroke>,
    seq: u64,
    state: tauri::State<'_, AppState>,
) -> Result<Option<SkyPreparation>, String> {
    let (detected, start, done, preview_size, canvas_size) = {
        let guard = state.sky_session.lock().unwrap();
        let s = guard.as_ref().ok_or("Detect the sky first.")?;
        let on_top = !s.strokes.is_empty() && strokes.starts_with(&s.strokes);
        let (start, done) = if on_top {
            (s.alpha.clone(), s.strokes.len())
        } else {
            (s.detected.clone(), 0)
        };
        (
            s.detected.clone(),
            start,
            done,
            s.preview_alpha.dimensions(),
            s.canvas_alpha.dimensions(),
        )
    };
    let (alpha, preview, canvas, mask, strokes) = tauri::async_runtime::spawn_blocking(move || {
        let mut alpha = (*start).clone();
        paint_onto(&mut alpha, &strokes[done..]);
        let small = |(w, h)| imageops::resize(&alpha, w, h, imageops::FilterType::Triangle);
        let (preview, canvas) = (small(preview_size), small(canvas_size));
        let mask = mask_data_url(&alpha)?;
        Ok::<_, String>((alpha, preview, canvas, mask, strokes))
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut guard = state.sky_session.lock().unwrap();
    let Some(s) = guard.as_mut() else {
        return Ok(None);
    };
    if seq < s.refine_seq || !Arc::ptr_eq(&s.detected, &detected) {
        return Ok(None);
    }
    s.refine_seq = seq;
    let result = SkyPreparation {
        coverage: coverage(&alpha),
        raw: s.gamma,
        mask,
    };
    s.alpha = Arc::new(alpha);
    s.strokes = strokes;
    s.preview_alpha = Arc::new(preview);
    s.canvas_alpha = Arc::new(canvas);
    Ok(Some(result))
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
    log::info!("sky: preview with {plate}");
    let (base, alpha, gamma, plate_image, look) =
        session_inputs(&state, &app_handle, &plate, Size::Panel)
            .inspect_err(|e| log::warn!("sky: preview refused: {e}"))?;
    tauri::async_runtime::spawn_blocking(move || {
        let plate_image = match &look {
            Some(look) => plate_in_raw_light(&plate_image, look),
            None => (*plate_image).clone(),
        };
        let result = replace_sky(
            &DynamicImage::ImageRgb8((*base).clone()),
            &alpha,
            &plate_image,
            &options,
        )
        .map_err(|e| e.to_string())?
        .to_rgb8();
        // A RAW's stored encoding is not a display encoding; show it as one
        // for choosing, which is all this is for.
        // A RAW shown as its rendering shows it, for choosing.
        let shown = match &look {
            Some(look) => look.to_display(&result),
            None if gamma => gamma_to_display(&result),
            None => result,
        };
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
    log::info!("sky: applying {plate} at full size");
    sky_patch(plate, options, Size::Full, state, app_handle).await
}

/// The replacement at about screen size, as patch data, to show on the
/// photograph in the editor (through the whole edit) while choosing. The
/// engine scales a patch to the photograph, so it goes in like any other.
#[tauri::command]
pub async fn preview_sky_on_photo(
    plate: String,
    options: SkyReplaceOptions,
    id: String,
    quick: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    // While the sky is dragged: small and quick, to follow the hand; the
    // sharp one follows when it is let go.
    let quick = quick == Some(true);
    if !quick {
        log::info!("sky: on the photo with {plate}");
    }
    // Keep the last few (one may still be on screen while the next renders).
    const KEEP: usize = 3;
    if let Some(session) = state.sky_session.lock().unwrap().as_mut() {
        session.shown_on_photo.push(id);
        let excess = session.shown_on_photo.len().saturating_sub(KEEP);
        let old: Vec<String> = session.shown_on_photo.drain(..excess).collect();
        let mut cache = state.patch_cache.lock().unwrap();
        for id in old {
            cache.remove(&id);
        }
    }
    let size = if quick { Size::Panel } else { Size::Canvas };
    sky_patch(plate, options, size, state, app_handle).await
}

async fn sky_patch(
    plate: String,
    options: SkyReplaceOptions,
    size: Size,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let started = std::time::Instant::now();
    let (base, alpha, gamma, plate_image, look) = session_inputs(&state, &app_handle, &plate, size)
        .inspect_err(|e| log::warn!("sky: refused: {e}"))?;
    tauri::async_runtime::spawn_blocking(move || {
        let plate_image = match &look {
            Some(look) => plate_in_raw_light(&plate_image, look),
            None => (*plate_image).clone(),
        };
        let result = replace_sky(
            &DynamicImage::ImageRgb8((*base).clone()),
            &alpha,
            &plate_image,
            &options,
        )
        .map_err(|e| e.to_string())?
        .to_rgb8();
        encode_patch(&base, &result, &alpha, &options, gamma)
    })
    .await
    .map_err(|e| e.to_string())?
    .inspect(|patch| {
        log::info!(
            "sky: patch made in {:.1?}, {} KB",
            started.elapsed(),
            patch.to_string().len() / 1024
        )
    })
    .inspect_err(|e| log::warn!("sky: patch failed: {e}"))
}

type Inputs = (
    Arc<RgbImage>,
    Arc<GrayImage>,
    bool,
    Arc<DynamicImage>,
    Option<Arc<RawLook>>,
);

#[derive(Clone, Copy)]
enum Size {
    /// The panel's small preview.
    Panel,
    /// On the photograph in the editor, while choosing.
    Canvas,
    /// Applied.
    Full,
}

fn session_inputs(
    state: &tauri::State<'_, AppState>,
    app: &tauri::AppHandle,
    plate: &str,
    size: Size,
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
                image::open(skies_dir(app)?.join(plate))
                    .map_err(|e| format!("Could not open {plate}: {e}"))?,
            );
            session.plate = Some((plate.to_string(), image.clone()));
            image
        }
    };
    let (base, alpha) = match size {
        Size::Panel => (&session.preview_base, &session.preview_alpha),
        Size::Canvas => (&session.canvas_base, &session.canvas_alpha),
        Size::Full => (&session.base, &session.alpha),
    };
    Ok((
        base.clone(),
        alpha.clone(),
        session.gamma,
        plate_image,
        session.look.clone(),
    ))
}

/// How a RAW's light, as the sky tool holds it (linear, scaled to fit,
/// stored through the 1/2.4 curve), comes out on screen: learnt by matching
/// luminance quantiles between that picture and a neutral render of it. A
/// rendering's tone curve never reverses, so this needs no pixel alignment.
/// Pairs of (display linear luminance, the tool's linear luminance).
pub struct RawLook(Vec<(f32, f32)>);

fn srgb_to_linear(v: u8) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn luma(p: [f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

impl RawLook {
    pub fn learn(base: &RgbImage, shown: &RgbImage) -> Self {
        let sorted = |v: Vec<f32>| {
            let mut v = v;
            v.sort_by(f32::total_cmp);
            v
        };
        let base_l = sorted(
            base.pixels()
                .step_by(5)
                .map(|p| luma(p.0.map(|v| (v as f32 / 255.0).powf(STORED_GAMMA))))
                .collect(),
        );
        let shown_l = sorted(
            shown
                .pixels()
                .map(|p| luma(p.0.map(srgb_to_linear)))
                .collect(),
        );
        let at = |v: &[f32], q: f32| v[((v.len() - 1) as f32 * q).round() as usize];
        let mut pairs: Vec<(f32, f32)> = (0..=200)
            .map(|i| {
                let q = i as f32 / 200.0;
                (at(&shown_l, q), at(&base_l, q))
            })
            .collect();
        // Strictly increasing in display luminance, for interpolation.
        pairs.dedup_by(|b, a| b.0 <= a.0);
        Self(pairs)
    }

    /// A picture in the tool's light, as the photo's rendering shows it.
    pub fn to_display(&self, stored: &RgbImage) -> RgbImage {
        let encode = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            let e = if v <= 0.0031308 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (e * 255.0).round() as u8
        };
        RgbImage::from_fn(stored.width(), stored.height(), |x, y| {
            let lin = stored
                .get_pixel(x, y)
                .0
                .map(|v| (v as f32 / 255.0).powf(STORED_GAMMA));
            let l = luma(lin);
            let gain = if l > 1e-7 {
                self.display_for(l) / l
            } else {
                0.0
            };
            Rgb(lin.map(|v| encode(v * gain)))
        })
    }

    /// The display luminance for the tool's luminance.
    fn display_for(&self, base: f32) -> f32 {
        let p = &self.0;
        if p.len() < 2 {
            return base;
        }
        let (first, last) = (p[0], p[p.len() - 1]);
        if base <= first.1 {
            return first.0 * base / first.1.max(1e-9);
        }
        if base >= last.1 {
            return last.0 * base / last.1.max(1e-9);
        }
        let i = p.partition_point(|q| q.1 < base).max(1);
        let (a, b) = (p[i - 1], p[i]);
        a.0 + (b.0 - a.0) * (base - a.1) / (b.1 - a.1).max(1e-12)
    }

    /// The tool's luminance for a display luminance.
    fn base_for(&self, display: f32) -> f32 {
        let p = &self.0;
        if p.len() < 2 {
            return display;
        }
        let first = p[0];
        let last = p[p.len() - 1];
        if display <= first.0 {
            return first.1 * display / first.0.max(1e-6);
        }
        if display >= last.0 {
            return last.1 * display / last.0.max(1e-6);
        }
        let i = p.partition_point(|q| q.0 < display).max(1);
        let (a, b) = (p[i - 1], p[i]);
        a.1 + (b.1 - a.1) * (display - a.0) / (b.0 - a.0).max(1e-9)
    }
}

/// A sky plate is an ordinary display picture; a RAW is worked on in its
/// own light. Bring the plate into that light so that, rendered, it looks as
/// it does on its own (as it would pasted into a JPEG): each pixel scaled so
/// its brightness lands where the photo's rendering puts that brightness,
/// its colour kept.
pub fn plate_in_raw_light(plate: &DynamicImage, look: &RawLook) -> DynamicImage {
    let rgb = plate.to_rgb8();
    let converted = RgbImage::from_fn(rgb.width(), rgb.height(), |x, y| {
        let lin = rgb.get_pixel(x, y).0.map(srgb_to_linear);
        let shown = luma(lin);
        let gain = if shown > 1e-6 {
            look.base_for(shown) / shown
        } else {
            0.0
        };
        Rgb(lin
            .map(|v| ((v * gain).clamp(0.0, 1.0).powf(1.0 / STORED_GAMMA) * 255.0).round() as u8))
    });
    DynamicImage::ImageRgb8(converted)
}

/// Patch data in the shape the fill tool writes. Public for the end-to-end
/// probe that puts it back through both engines.
pub fn encode_patch(
    base: &RgbImage,
    result: &RgbImage,
    alpha: &GrayImage,
    options: &SkyReplaceOptions,
    gamma: bool,
) -> Result<serde_json::Value, String> {
    let (w, h) = base.dimensions();
    let whole_frame = options.relight > 0.001 || options.haze > 0.001;
    let mask = if whole_frame {
        GrayImage::from_pixel(w, h, image::Luma([255]))
    } else {
        // Only the sky and its edge band changed. Widen the mask a little so
        // the un-mixed rim — foreground pixels that lost their old-sky tint —
        // is carried by the patch too.
        grow(alpha, ((w.max(h) as f32) * 0.004).ceil() as u32)
    };
    // JPEG at quality 95 either way. A sky covering half of a 33-megapixel
    // frame came to 32 MB as PNG, and the sidecar holding it is rewritten on
    // every edit. Outside the mask the colour is ignored, so it keeps the
    // real image rather than black: no hard edge for JPEG to ring against
    // where the mask is partial. The mask itself stays lossless.
    let mut color = Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut color, 95)
        .encode_image(result)
        .map_err(|e| e.to_string())?;
    let color_bytes = color.into_inner();
    let mut mask_bytes = Cursor::new(Vec::new());
    mask.write_to(&mut mask_bytes, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
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
    (
        ((w as f32 * s).round() as u32).max(1),
        ((h as f32 * s).round() as u32).max(1),
    )
}

/// Any coverage within `radius` pixels becomes full coverage.
fn grow(alpha: &GrayImage, radius: u32) -> GrayImage {
    let binary = GrayImage::from_fn(alpha.width(), alpha.height(), |x, y| {
        image::Luma([if alpha.get_pixel(x, y)[0] > 0 { 255 } else { 0 }])
    });
    imageproc::morphology::dilate(
        &binary,
        imageproc::distance_transform::Norm::LInf,
        radius.min(255) as u8,
    )
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
            if y < 24 {
                Rgb([200, 210, 220])
            } else {
                Rgb([60, 80, 40])
            }
        });
        let alpha = GrayImage::from_fn(64, 48, |_, y| image::Luma([if y < 24 { 255 } else { 0 }]));
        (photo, alpha)
    }

    fn decode(v: &serde_json::Value, key: &str) -> DynamicImage {
        image::load_from_memory(
            &general_purpose::STANDARD
                .decode(v[key].as_str().unwrap())
                .unwrap(),
        )
        .unwrap()
    }

    fn stroke(erase: bool, points: &[(f64, f64)]) -> SkyStroke {
        SkyStroke {
            erase,
            size: 0.1,
            feather: 0.0,
            points: points.iter().map(|&(x, y)| StrokePoint { x, y }).collect(),
            orientation_steps: 0,
            flip_horizontal: false,
            flip_vertical: false,
            rotation: 0.0,
        }
    }

    #[test]
    fn brushing_adds_and_takes_away_sky_in_order() {
        let (_, detected) = sky_photo();
        // Add a patch of sky in the ground, then take some of the sky away.
        let add = stroke(false, &[(0.2, 0.8), (0.4, 0.8)]);
        let erase = stroke(true, &[(0.7, 0.2)]);
        let painted = paint_strokes(&detected, &[add.clone(), erase.clone()]);
        assert_eq!(painted.get_pixel(19, 38)[0], 255, "brushed ground is sky");
        assert_eq!(painted.get_pixel(5, 40)[0], 0, "unbrushed ground is not");
        assert_eq!(painted.get_pixel(44, 9)[0], 0, "erased sky is gone");
        assert_eq!(
            painted.get_pixel(10, 5)[0],
            255,
            "the rest of the sky stays"
        );
        // Order matters: adding back over an erase restores it.
        let again = paint_strokes(&detected, &[erase, stroke(false, &[(0.7, 0.2)])]);
        assert_eq!(again.get_pixel(44, 9)[0], 255);
    }

    #[test]
    fn a_feathered_brush_fades_out() {
        let (_, detected) = sky_photo();
        let mut soft = stroke(false, &[(0.5, 0.85)]);
        soft.size = 0.4;
        soft.feather = 1.0;
        let painted = paint_strokes(&detected, &[soft]);
        let centre = painted.get_pixel(32, 41)[0];
        let rim = painted.get_pixel(32 + 10, 41)[0];
        assert!(centre > 240, "centre {centre}");
        assert!(rim > 0 && rim < centre, "rim {rim} should be partial");
    }

    #[test]
    fn strokes_follow_the_way_the_photo_is_turned() {
        let (_, detected) = sky_photo();
        // Turned a quarter clockwise, the shown top-left is the stored
        // bottom-left: brushing there adds sky at the stored bottom-left.
        let mut turned = stroke(false, &[(0.05, 0.05)]);
        turned.orientation_steps = 1;
        let painted = paint_strokes(&detected, &[turned]);
        assert_eq!(painted.get_pixel(2, 46)[0], 255);
        assert_eq!(painted.get_pixel(61, 46)[0], 0);
    }

    #[test]
    fn a_sky_only_patch_leaves_the_foreground_out() {
        let (photo, alpha) = sky_photo();
        let result = RgbImage::from_pixel(64, 48, Rgb([10, 20, 200]));
        let options = SkyReplaceOptions {
            relight: 0.0,
            haze: 0.0,
            ..SkyReplaceOptions::as_shot()
        };
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
        let patch = encode_patch(
            &photo,
            &result,
            &alpha,
            &SkyReplaceOptions::auto_match(),
            true,
        )
        .unwrap();
        let mask = decode(&patch, "mask").to_luma8();
        assert!(
            mask.pixels().all(|p| p[0] == 255),
            "relight changes the foreground too"
        );
        assert_eq!(patch["encoding"], "gamma");
        let colour = decode(&patch, "color").to_rgb8();
        assert_eq!(colour.dimensions(), (64, 48));
    }

    #[test]
    fn the_edge_band_is_carried() {
        let (_, alpha) = sky_photo();
        let grown = grow(&alpha, 3);
        assert_eq!(
            grown.get_pixel(10, 26)[0],
            255,
            "rim below the horizon should be included"
        );
        assert_eq!(grown.get_pixel(10, 40)[0], 0);
    }
}
