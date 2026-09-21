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
pub struct DetailCache {
    source: Arc<DecodedFrame>,
    key: u64,
    image: Arc<DynamicImage>,
}
pub struct PreparedCache {
    source: Arc<DecodedFrame>,
    transform: u64,
    patches: u64,
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
    let mut frame = if crate::formats::is_raw_file(&path) {
        super::raw::decode_raw(&bytes, false, || Ok(()))?
    } else {
        super::input::decode_profiled_photo(&bytes)?
    };
    // A captured input transform turns a rendered photograph into scene data,
    // undoing whatever rendering it already carries, exactly as Resolve does
    // on import. After it the source is indistinguishable from a RAW's, so
    // everything downstream — including the captured output transform — is
    // already right for it. Done once here rather than per render: the
    // decoded source is cached.
    if frame.color.reference == ReferenceDomain::Display
        && let Some(path) = input_transform(state)
    {
        match super::cube::CubeLut::load(&path) {
            Ok(cube) => {
                super::cube::apply_input_transform(&cube, &mut frame.pixels);
                frame.color = SourceColor {
                    primaries: Primaries::DavinciWideGamut,
                    transfer: Transfer::Linear,
                    reference: ReferenceDomain::Scene,
                };
            }
            Err(error) => log::error!("Ignoring the captured input transform: {error}"),
        }
    }
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
    // A captured output transform maps *scene* values to a display, so it
    // belongs only on scene-referred sources. A rendered photograph has
    // already been through someone's rendering: putting it through a second
    // one tone-maps it twice and darkens everything — measured at mid grey,
    // sRGB 0.461 in, 0.351 out. `source` fixes that at the other end, by
    // running an installed input transform over rendered photographs so they
    // arrive here as scene data. What is still Display-referred by this point
    // has no input transform installed, and keeps the built-in rendering.
    let captured = match color.reference {
        ReferenceDomain::Scene => output_transform,
        ReferenceDomain::Display => None,
    };
    let output_rendering = match (&captured, color.reference) {
        (Some(_), _) => OutputRendering::ResolveCubeV1,
        (None, ReferenceDomain::Scene) => OutputRendering::SceneLuminanceV2,
        (None, ReferenceDomain::Display) => OutputRendering::DisplayGamutV2,
    };
    RenderPlan::build(PipelineConfig {
        process_version: 3,
        source: color,
        working_space: Primaries::DavinciWideGamut,
        output_rendering,
        output_lut: captured,
        controls,
    })
}

/// The captured transform this session renders through, if any.
pub fn output_transform(state: &AppState) -> Option<PathBuf> {
    state.output_transform.lock().unwrap().clone()
}

/// The captured transform that brings rendered photographs into scene data.
pub fn input_transform(state: &AppState) -> Option<PathBuf> {
    state.input_transform.lock().unwrap().clone()
}

/// A mask's bitmap, cached. The key is the mask's own definition — minus its
/// adjustments, which change what the mask *does* but not where it is — the
/// output size and framing, and the picture a range mask samples.
#[allow(clippy::too_many_arguments)]
fn mask_bitmap(
    state: &AppState,
    mask: &crate::mask_generation::MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    offset: (f32, f32),
    sampled: Option<&Arc<DynamicImage>>,
    picture: (&str, u64, u64),
) -> Result<Arc<image::GrayImage>> {
    use std::hash::{Hash, Hasher};
    let mut shape = serde_json::to_value(mask)?;
    shape["adjustments"] = Value::Null;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    shape.to_string().hash(&mut hasher);
    (width, height, scale.to_bits(), offset.0.to_bits(), offset.1.to_bits()).hash(&mut hasher);
    // Range masks depend on the picture too; shape masks do not.
    if mask.requires_warped_image() {
        picture.hash(&mut hasher);
    }
    let key = hasher.finish();
    if let Some(hit) = state.v3_masks.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Ok(hit);
    }
    let bitmap = Arc::new(
        crate::mask_generation::generate_mask_bitmap(
            mask,
            width,
            height,
            scale,
            offset,
            sampled.map(|s| s.as_ref()),
        )
        .context("Could not generate v3 mask")?,
    );
    if let Ok(mut cache) = state.v3_masks.lock() {
        // A handful of masks at one or two preview sizes is all an edit uses;
        // anything beyond that is stale.
        if cache.len() >= 24 {
            cache.clear();
        }
        cache.insert(key, bitmap.clone());
    }
    Ok(bitmap)
}

/// The prepared image with detail applied, cached against everything that
/// determines it, so dragging any other slider does not redo a spatial pass.
#[allow(clippy::too_many_arguments)]
fn detailed(
    state: &AppState,
    source: &Arc<DecodedFrame>,
    image: Arc<DynamicImage>,
    detail: &super::detail::Detail,
    transform: u64,
    patches: u64,
    dimension: Option<u32>,
    scale: f32,
) -> Result<Arc<DynamicImage>> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(detail)?.hash(&mut hasher);
    transform.hash(&mut hasher);
    patches.hash(&mut hasher);
    dimension.hash(&mut hasher);
    scale.to_bits().hash(&mut hasher);
    let key = hasher.finish();
    if let Ok(cache) = state.v3_detail.lock()
        && let Some(c) = cache.as_ref()
        && Arc::ptr_eq(&c.source, source)
        && c.key == key
    {
        return Ok(c.image.clone());
    }
    // Luminance in the source's own primaries: the prepared image has not
    // been converted to the working space yet.
    let y = super::spaces::rgb_to_xyz(source.color.primaries).row(1);
    let weights = [y.x as f32, y.y as f32, y.z as f32];
    let mut pixels = image.to_rgba32f();
    super::detail::apply(&mut pixels, detail, weights, scale);
    let result = Arc::new(DynamicImage::ImageRgba32F(pixels));
    if let Ok(mut cache) = state.v3_detail.lock() {
        *cache = Some(DetailCache {
            source: source.clone(),
            key,
            image: result.clone(),
        });
    }
    Ok(result)
}

/// What a colour or luminance range mask samples: the picture as it stands
/// before grading, at full resolution.
fn sampling_image(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    transform: u64,
    patches: u64,
) -> Result<Arc<DynamicImage>> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    transform.hash(&mut hasher);
    patches.hash(&mut hasher);
    let key = hasher.finish();
    if let Ok(cache) = state.v3_sampling.lock()
        && let Some((cached, image)) = cache.as_ref()
        && *cached == key
    {
        return Ok(image.clone());
    }
    // Neutral: the same engine, the same transforms, no controls.
    let mut neutral = edits.clone();
    neutral["v3"] = serde_json::json!({});
    neutral["masks"] = serde_json::json!([]);
    // The sampling render goes through the same prepared-image cache as the
    // preview. Put the preview's entry back afterwards, or the next slider
    // move pays to rebuild it from the full-resolution source.
    let preview = state.v3_prepared.lock().ok().and_then(|mut c| c.take());
    let frame = render_file(context, state, path, &neutral, None);
    if let (Some(entry), Ok(mut cache)) = (preview, state.v3_prepared.lock()) {
        *cache = Some(entry);
    }
    let image = Arc::new(DynamicImage::ImageRgba8(frame?.preview_rgba8()));
    if let Ok(mut cache) = state.v3_sampling.lock() {
        *cache = Some((key, image.clone()));
    }
    Ok(image)
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

/// Stage timings on stderr when `RAPIDRAW_V3_PROFILE` is set. Costs nothing
/// otherwise; kept because guessing where interactive time goes has been
/// wrong more than once.
struct Stopwatch(Option<std::time::Instant>);
impl Stopwatch {
    fn start() -> Self {
        Self(std::env::var_os("RAPIDRAW_V3_PROFILE").map(|_| std::time::Instant::now()))
    }
    fn lap(&mut self, stage: &str) {
        if let Some(t) = &mut self.0 {
            eprintln!("  v3 {stage:24} {:>6.1} ms", t.elapsed().as_secs_f64() * 1000.0);
            *t = std::time::Instant::now();
        }
    }
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
    let mut watch = Stopwatch::start();
    let controls = controls(edits)?;
    let source = source(state, path)?;
    watch.lap("source");
    let transform = crate::cache_utils::calculate_transform_hash(edits);
    // Patches are part of what the prepared image *is*, so they belong in its
    // key. Without this, hiding a patch would leave the old composite on
    // screen until some unrelated edit happened to invalidate the cache.
    let patches = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for patch in super::patches::visible(edits) {
            patch.to_string().hash(&mut hasher);
        }
        hasher.finish()
    };
    let mut prepared = state
        .v3_prepared
        .lock()
        .map_err(|_| anyhow::anyhow!("V3 preview cache unavailable"))?;
    let hit = prepared.as_ref().is_some_and(|p| {
        Arc::ptr_eq(&p.source, &source)
            && p.transform == transform
            && p.patches == patches
            && p.dimension == max_dimension
    });
    if !hit {
        // Patches join at the decoded-source stage, before geometry, because
        // the mask stored with a patch is in those coordinates.
        let patched = if super::patches::visible(edits).is_empty() {
            source.pixels.clone()
        } else {
            let cube = input_transform(state)
                .map(|p| super::cube::CubeLut::load(&p))
                .transpose()?;
            let mut pixels = source.pixels.clone();
            super::patches::composite(&mut pixels, edits, &source.color, cube.as_ref())?;
            pixels
        };
        let base = DynamicImage::ImageRgba32F(patched);
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
            patches,
            dimension: max_dimension,
            image: Arc::new(image),
            offset,
            scale,
        });
    }
    let p = prepared.as_ref().unwrap();
    let (image, offset, scale) = (p.image.clone(), p.offset, p.scale);
    drop(prepared);
    let image = if controls.detail.is_neutral() {
        image
    } else {
        detailed(state, &source, image, &controls.detail, transform, patches, max_dimension, scale)?
    };
    watch.lap("prepare + detail");
    let masks: Vec<crate::mask_generation::MaskDefinition> =
        serde_json::from_value(edits.get("masks").cloned().unwrap_or(serde_json::json!([])))
            .context("Invalid mask definitions")?;
    let active: Vec<_> = masks
        .iter()
        .filter(|m| m.visible && m.opacity > 0. && !m.sub_masks.is_empty())
        .collect();
    // Colour and luminance range masks need something to sample. The
    // previous engine hands them the geometrically-warped source before any
    // adjustment, so the mask does not move as you grade; v3 honours the same
    // contract, rendered through its own pipeline at neutral so what the mask
    // measures is what the picture is before grading — not a second opinion
    // about colour from a different set of transforms.
    //
    // Full resolution, because the mask generator maps its coordinates
    // against the warped image's own dimensions. It is cached against
    // everything that changes the picture before grading, so it is built once
    // per geometry or patch change rather than per render.
    let sampled = if active.iter().any(|m| m.requires_warped_image()) {
        Some(sampling_image(
            context,
            state,
            path,
            edits,
            transform,
            patches,
        )?)
    } else {
        None
    };
    watch.lap("mask sampling");
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
    let grain = super::controls::Effects {
        vignette_amount: 0.,
        ..controls.effects.clone()
    };
    let mut initial_plan = plan(source.color.clone(), controls, output_transform(state))?;
    initial_plan.set_render_scale(scale);
    if active.is_empty() {
        return engine.render(&image.to_rgba32f(), &initial_plan, capture);
    }
    // With masks, the first pass only feeds the local adjustments, which read
    // the graded stage alone.
    let mut working = if capture {
        engine
            .render(&image.to_rgba32f(), &initial_plan, true)?
            .stages
            .context("Missing local-adjustment stage")?
            .graded
    } else {
        engine.render_graded(&image.to_rgba32f(), &initial_plan)?
    };
    watch.lap("first pass");
    let working_color = SourceColor {
        primaries: Primaries::DavinciWideGamut,
        transfer: Transfer::Linear,
        reference: source.color.reference,
    };
    let working_luminance = {
        let y = super::spaces::rgb_to_xyz(Primaries::DavinciWideGamut).row(1);
        [y.x as f32, y.y as f32, y.z as f32]
    };
    for mask in active {
        let local = self::controls(&mask.adjustments)?;
        // Vignette and grain describe the whole frame; a mask carrying them
        // would be applying a frame effect to part of a frame.
        ensure!(
            local.effects.is_neutral(),
            "Vignette and grain apply to the whole photo, not inside a mask."
        );
        // `is_neutral` is about the pointwise pass; detail is its own stage.
        if local.is_neutral() && local.detail.is_neutral() {
            continue;
        }
        let bitmap = mask_bitmap(
            state,
            mask,
            image.width(),
            image.height(),
            scale,
            (offset.0 * scale, offset.1 * scale),
            sampled.as_ref(),
            (path, transform, patches),
        )?;
        watch.lap("mask bitmap");
        // Local detail runs on the working image as it stands at this mask —
        // after the global grade and any earlier masks — like every other
        // local control. Luminance is the same physical quantity whichever
        // primaries it is measured in, so this does to the picture what the
        // global stage would: a full-coverage mask matches global detail.
        let detailed;
        let input = if local.detail.is_neutral() {
            &working
        } else {
            let mut copy = working.clone();
            super::detail::apply(&mut copy, &local.detail, working_luminance, scale);
            detailed = copy;
            &detailed
        };
        let adjusted = if local.is_neutral() {
            input.clone()
        } else {
            engine.render_graded(input, &plan(working_color.clone(), local, None)?)?
        };
        watch.lap("mask pass");
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
    watch.lap("mask blends");
    // Only this last pass produces output, so only it renders through the
    // captured transform.
    // The vignette is already in `working`; grain belongs on the finished
    // image, which is this pass's, so it carries the grain settings.
    let mut final_plan = plan(
        working_color,
        Controls {
            effects: grain,
            ..Controls::default()
        },
        output_transform(state),
    )?;
    final_plan.set_render_scale(scale);
    let frame = engine.render(&working, &final_plan, false);
    watch.lap("final pass");
    frame
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
