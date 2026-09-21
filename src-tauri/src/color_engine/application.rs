//! The one application entry point shared by v3 previews and file exports.
use super::{
    ColorEngine, RenderedFrame, config::*, controls::Controls, input::DecodedFrame,
    plan::RenderPlan,
};
use crate::{AppState, image_processing::GpuContext};
use std::path::PathBuf;
use anyhow::{Context, Result, ensure};
use image::DynamicImage;
use serde_json::Value;
use std::{sync::Arc, time::SystemTime};

pub struct SourceCache {
    path: std::path::PathBuf,
    length: u64,
    modified: SystemTime,
    pub frame: Arc<DecodedFrame>,
}
pub struct EngineCache {
    device: Arc<wgpu::Device>,
    engine: ColorEngine,
}
pub struct PreparedCache {
    source: Arc<DecodedFrame>,
    transform: u64,
    dimension: Option<u32>,
    image: Arc<DynamicImage>,
    offset: (f32, f32),
    scale: f32,
}

pub fn enabled(edits: &Value) -> bool {
    edits["processVersion"].as_u64() == Some(3)
}

pub fn source(state: &AppState, path: &str) -> Result<Arc<DecodedFrame>> {
    let (path, _) = crate::file_management::parse_virtual_path(path);
    let meta = std::fs::metadata(&path)?;
    let modified = meta.modified()?;
    let mut cache = state
        .v3_source
        .lock()
        .map_err(|_| anyhow::anyhow!("V3 source cache unavailable"))?;
    if let Some(c) = &*cache {
        if c.path == path && c.length == meta.len() && c.modified == modified {
            return Ok(c.frame.clone());
        }
    }
    let bytes = std::fs::read(&path)?;
    let frame = if crate::formats::is_raw_file(&path) {
        super::raw::decode_raw(&bytes, false, || Ok(()))?
    } else {
        super::input::decode_profiled_photo(&bytes)?
    };
    let frame = Arc::new(frame);
    *cache = Some(SourceCache {
        path,
        length: meta.len(),
        modified,
        frame: frame.clone(),
    });
    Ok(frame)
}

pub fn controls(edits: &Value) -> Result<Controls> {
    let controls: Controls = if edits["v3"].is_null() {
        Controls::default()
    } else {
        serde_json::from_value(edits["v3"].clone()).context("Invalid v3 settings")?
    };
    controls.validate()?;
    Ok(controls)
}

pub fn validate_features(edits: &Value) -> Result<()> {
    ensure!(
        !edits["aiPatches"]
            .as_array()
            .is_some_and(|a| a.iter().any(|p| p["visible"].as_bool() != Some(false))),
        "V3 does not yet support AI image patches. Hide the patches or return to the previous engine."
    );
    ensure!(
        edits["flatFieldProfile"].is_null(),
        "V3 flat-field profiles need a color-space adapter; return to the previous engine."
    );
    // A legacy look must not be silently evaluated in a different color space.
    ensure!(
        edits["lutPath"].as_str().is_none_or(|s| s.is_empty()),
        "Remove the legacy LUT before enabling v3; its input/output color-space contract is not defined for this engine."
    );
    Ok(())
}

/// The rendering step, and where a captured transform displaces it.
///
/// `output_transform` is the cube captured from this machine's Resolve, when
/// one has been installed. It replaces the built-in rendering entirely rather
/// than running after it — two rendering transforms in series is the mistake
/// the whole pipeline is arranged to avoid.
fn plan(
    color: SourceColor,
    controls: Controls,
    output_transform: Option<PathBuf>,
) -> Result<RenderPlan> {
    let output_rendering = match (&output_transform, color.reference) {
        (Some(_), _) => OutputRendering::ResolveCubeV1,
        (None, ReferenceDomain::Scene) => OutputRendering::SceneLuminanceV2,
        (None, ReferenceDomain::Display) => OutputRendering::DisplayGamutV2,
    };
    RenderPlan::build(PipelineConfig {
        process_version: 3,
        source: color,
        working_space: Primaries::DavinciWideGamut,
        output_rendering,
        output_lut: output_transform,
        controls,
    })
}

/// The captured transform this session renders through, if any.
pub fn output_transform(state: &AppState) -> Option<PathBuf> {
    state.output_transform.lock().unwrap().clone()
}

/// `max_dimension` only changes spatial sampling; source decode and all color
/// operators are identical for interactive, settled and full-size export.
pub fn render_file(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    max_dimension: Option<u32>,
) -> Result<RenderedFrame> {
    render_file_with_capture(context, state, path, edits, max_dimension, false)
}

pub(crate) fn render_file_with_capture(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    max_dimension: Option<u32>,
    capture: bool,
) -> Result<RenderedFrame> {
    ensure!(enabled(edits), "Expected v3 edits");
    validate_features(edits)?;
    let controls = controls(edits)?;
    let source = source(state, path)?;
    let transform = crate::cache_utils::calculate_transform_hash(edits);
    let mut prepared = state
        .v3_prepared
        .lock()
        .map_err(|_| anyhow::anyhow!("V3 preview cache unavailable"))?;
    let hit = prepared.as_ref().is_some_and(|p| {
        Arc::ptr_eq(&p.source, &source) && p.transform == transform && p.dimension == max_dimension
    });
    if !hit {
        let base = DynamicImage::ImageRgba32F(source.pixels.clone());
        let (transformed, offset) = crate::apply_all_transformations(&base, edits);
        let full_width = transformed.width();
        let image = if let Some(dim) = max_dimension {
            ensure!((16..=16384).contains(&dim), "Invalid v3 preview dimensions");
            crate::image_processing::downscale_f32_image(&transformed, dim, dim)
        } else {
            transformed.into_owned()
        };
        let scale = image.width() as f32 / full_width as f32;
        *prepared = Some(PreparedCache {
            source: source.clone(),
            transform,
            dimension: max_dimension,
            image: Arc::new(image),
            offset,
            scale,
        });
    }
    let p = prepared.as_ref().unwrap();
    let (image, offset, scale) = (p.image.clone(), p.offset, p.scale);
    drop(prepared);
    let masks: Vec<crate::mask_generation::MaskDefinition> =
        serde_json::from_value(edits.get("masks").cloned().unwrap_or(serde_json::json!([])))
            .context("Invalid mask definitions")?;
    let active: Vec<_> = masks
        .iter()
        .filter(|m| m.visible && m.opacity > 0. && !m.sub_masks.is_empty())
        .collect();
    for m in &active {
        ensure!(
            !m.requires_warped_image(),
            "V3 color/luminance range masks require a new sampling contract. Use a brush/gradient/bitmap mask or the previous engine."
        );
    }
    let mut cache = state
        .v3_engine
        .lock()
        .map_err(|_| anyhow::anyhow!("V3 renderer unavailable"))?;
    if cache
        .as_ref()
        .is_none_or(|c| !Arc::ptr_eq(&c.device, &context.device))
    {
        *cache = Some(EngineCache {
            device: context.device.clone(),
            engine: ColorEngine::new(context.clone())?,
        });
    }
    let engine = &cache.as_ref().unwrap().engine;
    let initial = engine.render(
        &image.to_rgba32f(),
        &plan(source.color.clone(), controls, output_transform(state))?,
        capture || !active.is_empty(),
    )?;
    if active.is_empty() {
        return Ok(initial);
    }
    let mut working = initial
        .stages
        .context("Missing local-adjustment stage")?
        .graded;
    let working_color = SourceColor {
        primaries: Primaries::DavinciWideGamut,
        transfer: Transfer::Linear,
        reference: source.color.reference,
    };
    for mask in active {
        let local = self::controls(&mask.adjustments)?;
        if local.is_neutral() {
            continue;
        }
        let bitmap = crate::mask_generation::generate_mask_bitmap(
            mask,
            image.width(),
            image.height(),
            scale,
            (offset.0 * scale, offset.1 * scale),
            None,
        )
        .context("Could not generate v3 mask")?;
        let adjusted = engine
            .render(&working, &plan(working_color.clone(), local, None)?, true)?
            .stages
            .context("Missing mask stage")?
            .graded;
        for ((base, edited), alpha) in working
            .pixels_mut()
            .zip(adjusted.pixels())
            .zip(bitmap.pixels())
        {
            let a = alpha[0] as f32 / 255.;
            for c in 0..3 {
                base[c] += (edited[c] - base[c]) * a;
            }
        }
    }
    // Only this last pass produces output, so only it renders through the
    // captured transform.
    engine.render(
        &working,
        &plan(working_color, Controls::default(), output_transform(state))?,
        false,
    )
}

#[tauri::command]
pub async fn prepare_color_v3(path: String, app_handle: tauri::AppHandle) -> Result<Value, String> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        source(&app_handle.state::<AppState>(),&path).map(|f|serde_json::json!({"source":f.color,"provenance":f.provenance,"width":f.pixels.width(),"height":f.pixels.height()})).map_err(|e|e.to_string())
    }).await.map_err(|e|e.to_string())?
}

pub fn preview_bytes(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    dimension: u32,
) -> Result<Vec<u8>, String> {
    let frame =
        render_file(context, state, path, edits, Some(dimension)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    frame
        .write_srgb_png(&mut bytes, false)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}
