//! The one application entry point shared by v3 previews and file exports.
use super::{
    ColorEngine, RenderedFrame,
    config::*,
    controls::Controls,
    input::DecodedFrame,
    plan::{Look, LookSpace, RenderPlan},
    spaces,
};
use crate::{AppState, image_processing::GpuContext};
use anyhow::{Context, Result, ensure};
use image::DynamicImage;
use serde_json::Value;
use std::path::PathBuf;
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
pub struct NeighbourhoodCache {
    source: Arc<DecodedFrame>,
    key: u64,
    blurs: Arc<Vec<[f32; 4]>>,
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
    if let Some(c) = &*cache
        && c.path == path
        && c.length == meta.len()
        && c.modified == modified
    {
        return Ok(c.frame.clone());
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

/// The shared settings of the previous engine that v3 reads as controls.
/// Clearing these, with `v3`, masks and the LUT, gives the picture before
/// any grading: what range masks and auto adjustment measure.
const SHARED_CONTROLS: &[&str] = &[
    "exposure",
    "brightness",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
];

fn ungraded(edits: &Value) -> Value {
    let mut neutral = edits.clone();
    neutral["v3"] = serde_json::json!({});
    neutral["masks"] = serde_json::json!([]);
    neutral["lutPath"] = Value::Null;
    for key in SHARED_CONTROLS {
        neutral[*key] = serde_json::json!(0);
    }
    neutral
}

pub fn controls(edits: &Value) -> Result<Controls> {
    controls_for(edits, false)
}

/// V3's own settings from the `v3` namespace, plus the controls it shares
/// with the previous engine, read by that engine's own parser so they are
/// scaled exactly as there. A mask's settings are parsed as a mask's.
fn controls_for(edits: &Value, mask: bool) -> Result<Controls> {
    let mut controls: Controls = if edits["v3"].is_null() {
        Controls::default()
    } else {
        serde_json::from_value(edits["v3"].clone()).context("Invalid v3 settings")?
    };
    controls.tone = if mask {
        let m = crate::image_processing::get_mask_adjustments_from_json(edits);
        super::controls::Tone {
            exposure: m.exposure,
            brightness: m.brightness,
            contrast: m.contrast,
            // A mask's pivot is stored relative to the classic centre.
            pivot: 0.5 + m.contrast_pivot,
            highlights: m.highlights,
            shadows: m.shadows,
            whites: m.whites,
            blacks: m.blacks,
        }
    } else {
        let g = crate::image_processing::get_global_adjustments_from_json(edits, true, None);
        super::controls::Tone {
            exposure: g.exposure,
            brightness: g.brightness,
            contrast: g.contrast,
            pivot: g.contrast_pivot,
            highlights: g.highlights,
            shadows: g.shadows,
            whites: g.whites,
            blacks: g.blacks,
        }
    };
    controls.validate()?;
    Ok(controls)
}

pub fn validate_features(_edits: &Value) -> Result<()> {
    // Every feature the previous engine applies to the whole picture now has
    // a v3 definition; kept as the one place a future refusal would go.
    Ok(())
}

/// The creative LUT the edits ask for, with the input space it was made for.
/// The same settings the previous engine reads, so film simulations, presets
/// and copy/paste carry over; what is new is that the space decides where in
/// the pipeline the LUT goes (see `LookSpace`).
fn look(state: &AppState, edits: &Value) -> Result<Option<Look>> {
    let Some(path) = edits["lutPath"].as_str().filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    // Hiding the effects section hides its LUT, as it always has.
    if edits["sectionVisibility"]["effects"].as_bool() == Some(false) {
        return Ok(None);
    }
    let cached = state
        .lut_cache
        .lock()
        .ok()
        .and_then(|c| c.get(path).cloned());
    let lut = match cached {
        Some(lut) => lut,
        None => {
            let lut = Arc::new(
                crate::lut_processing::parse_lut_file(path)
                    .with_context(|| format!("Could not load the LUT {path}"))?,
            );
            if let Ok(mut cache) = state.lut_cache.lock() {
                cache.insert(path.to_string(), lut.clone());
            }
            lut
        }
    };
    Ok(Some(Look {
        lut,
        space: LookSpace::from_setting(edits["lutInputSpace"].as_str()),
        intensity: edits["lutIntensity"].as_f64().unwrap_or(100.) as f32 / 100.,
        exposure: edits["lutSimExposure"].as_f64().unwrap_or(0.) as f32,
    }))
}

/// The rendering step, and where a captured transform displaces it.
///
/// `output_transform` is the cube captured from this machine's Resolve, when
/// one has been installed. It replaces the built-in rendering entirely rather
/// than running after it — two rendering transforms in series is the mistake
/// the whole pipeline is arranged to avoid.
/// The Tone Mapper switch. "resolve", v3's own, renders through the
/// captured Resolve transform when one is installed (the built-in rendering
/// otherwise); the others are the previous engine's, kept selectable.
fn tone_mapper(edits: &Value) -> Option<OutputRendering> {
    match edits["toneMapper"].as_str() {
        Some("basic") => Some(OutputRendering::PreviousBasic),
        Some("agx") => Some(OutputRendering::PreviousAgx),
        Some("filmic") => Some(OutputRendering::PreviousFilmic),
        _ => None,
    }
}

fn plan(
    color: SourceColor,
    controls: Controls,
    output_transform: Option<PathBuf>,
    previous: Option<OutputRendering>,
) -> Result<RenderPlan> {
    if let Some(output_rendering) = previous {
        return RenderPlan::build(PipelineConfig {
            process_version: 3,
            source: color,
            working_space: Primaries::DavinciWideGamut,
            output_rendering,
            output_lut: None,
            controls,
        });
    }
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
    (
        width,
        height,
        scale.to_bits(),
        offset.0.to_bits(),
        offset.1.to_bits(),
    )
        .hash(&mut hasher);
    // Range masks depend on the picture too; shape masks do not.
    if mask.requires_warped_image() {
        picture.hash(&mut hasher);
    }
    let key = hasher.finish();
    if let Some(hit) = state
        .v3_masks
        .lock()
        .ok()
        .and_then(|c| c.get(&key).cloned())
    {
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

/// The prepared image with the spatial stages applied — chromatic
/// aberration correction, detail, then glow, halation and flare — cached
/// against everything that determines them, so dragging any other slider does
/// not redo a spatial pass.
#[allow(clippy::too_many_arguments)]
fn spatial(
    state: &AppState,
    source: &Arc<DecodedFrame>,
    image: Arc<DynamicImage>,
    controls: &Controls,
    transform: u64,
    patches: u64,
    dimension: Option<u32>,
    scale: f32,
) -> Result<Arc<DynamicImage>> {
    use std::hash::{Hash, Hasher};
    let effects = &controls.effects;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&controls.detail)?.hash(&mut hasher);
    [effects.ca_red_cyan, effects.ca_blue_yellow]
        .map(f32::to_bits)
        .hash(&mut hasher);
    // The light effects' thresholds follow exposure; nothing else here does,
    // so exposure is only part of the key when they are on.
    if !super::optics::light_is_neutral(effects) {
        [
            effects.glow_amount,
            effects.halation_amount,
            effects.flare_amount,
            controls.tone.exposure,
        ]
        .map(f32::to_bits)
        .hash(&mut hasher);
    }
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
    super::optics::correct_chromatic_aberration(&mut pixels, effects);
    super::detail::apply(&mut pixels, &controls.detail, weights, scale);
    if let Some(clarity) = super::optics::centre_clarity(effects) {
        let mut clarified = pixels.clone();
        super::detail::apply(&mut clarified, &clarity, weights, scale);
        super::optics::blend_centre(&mut pixels, &clarified);
    }
    super::optics::add_light(
        &mut pixels,
        effects,
        controls.tone.exposure,
        source.color.primaries,
    );
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

/// The previous engine's tonal and structure blurs of the unedited picture,
/// which its local tone controls — contrast, shadows, whites, blacks,
/// highlights — read. Same radii (3.5 and 40 pixels, scaled by the short edge
/// over 1080, sigma half the radius), in the encoding it blurred in: linear
/// light for scene data, which it saw from RAW files, and sRGB code values
/// for display-referred pictures. Values are in linear sRGB primaries, where
/// those functions work. Two entries per pixel, tonal then structure.
fn neighbourhood(
    state: &AppState,
    source: &Arc<DecodedFrame>,
    image: &DynamicImage,
    key: u64,
) -> Arc<Vec<[f32; 4]>> {
    if let Ok(cache) = state.v3_neighbourhood.lock()
        && let Some(c) = cache.as_ref()
        && Arc::ptr_eq(&c.source, source)
        && c.key == key
    {
        return c.blurs.clone();
    }
    let pixels = image.to_rgba32f();
    let (w, h) = (pixels.width() as usize, pixels.height() as usize);
    let m = spaces::conversion(source.color.primaries, Primaries::Srgb)
        .transpose()
        .to_cols_array_2d()
        .map(|r| r.map(|v| v as f32));
    let scene = source.color.reference == ReferenceDomain::Scene;
    let encode = |v: f32| {
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    };
    let mut planes = vec![vec![0f32; w * h]; 3];
    for (i, p) in pixels.pixels().enumerate() {
        for (c, plane) in planes.iter_mut().enumerate() {
            let v = (m[c][0] * p[0] + m[c][1] * p[1] + m[c][2] * p[2]).clamp(0., 65504.);
            plane[i] = if scene { v } else { encode(v) };
        }
    }
    let scale = w.min(h) as f32 / 1080.;
    let blurred = |base: f32| -> Vec<Vec<f32>> {
        let radius = (base * scale).ceil().max(1.) as usize;
        planes
            .iter()
            .map(|p| {
                // Its exact truncated kernel where that is affordable — at
                // small radii the three-box approximation rounds to no blur
                // at all — and the approximation for the wide one, where it
                // is indistinguishable and the exact kernel would cost
                // hundreds of taps per pixel.
                if radius <= 24 {
                    exact_gaussian(p, w, h, radius)
                } else {
                    let g = super::detail::Gaussian::new(radius as f32 / 2.);
                    super::detail::blur(p, w, h, g)
                }
            })
            .collect()
    };
    let (tonal, structure) = (blurred(3.5), blurred(40.));
    let mut blurs = Vec::with_capacity(w * h * 2);
    for i in 0..w * h {
        blurs.push([tonal[0][i], tonal[1][i], tonal[2][i], 0.]);
        blurs.push([structure[0][i], structure[1][i], structure[2][i], 0.]);
    }
    let blurs = Arc::new(blurs);
    if let Ok(mut cache) = state.v3_neighbourhood.lock() {
        *cache = Some(NeighbourhoodCache {
            source: source.clone(),
            key,
            blurs: blurs.clone(),
        });
    }
    blurs
}

/// The previous engine's blur: a Gaussian of sigma radius/2 truncated at
/// `radius`, edges repeated, rows then columns.
fn exact_gaussian(plane: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    use rayon::prelude::*;
    let sigma = radius as f32 / 2.;
    let kernel: Vec<f32> = (0..=2 * radius)
        .map(|i| {
            let x = i as f32 - radius as f32;
            (-(x * x) / (2. * sigma * sigma)).exp()
        })
        .collect();
    let total: f32 = kernel.iter().sum();
    let r = radius as isize;
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let src = &plane[y * w..(y + 1) * w];
        for (x, o) in out.iter_mut().enumerate() {
            let mut sum = 0.;
            for (k, weight) in kernel.iter().enumerate() {
                let sx = (x as isize + k as isize - r).clamp(0, w as isize - 1) as usize;
                sum += src[sx] * weight;
            }
            *o = sum / total;
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, dst)| {
        for (x, o) in dst.iter_mut().enumerate() {
            let mut sum = 0.;
            for (k, weight) in kernel.iter().enumerate() {
                let sy = (y as isize + k as isize - r).clamp(0, h as isize - 1) as usize;
                sum += rows[sy * w + x] * weight;
            }
            *o = sum / total;
        }
    });
    out
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
    let neutral = ungraded(edits);
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
            eprintln!(
                "  v3 {stage:24} {:>6.1} ms",
                t.elapsed().as_secs_f64() * 1000.0
            );
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
            super::patches::composite(
                &mut pixels,
                edits,
                &source.color,
                source.source_profile.as_deref(),
                cube.as_ref(),
            )?;
            pixels
        };
        // Flat-field correction divides linear light in the unwarped frame,
        // before geometry, as the previous engine's does — but on linear
        // values, so the shared geometry step is handed edits without it.
        let mut patched = patched;
        let to_srgb = spaces::conversion(source.color.primaries, Primaries::Srgb);
        let rows = |m: glam::DMat3| {
            m.transpose()
                .to_cols_array_2d()
                .map(|r| r.map(|v| v as f32))
        };
        let flattened = crate::flat_field::apply_flat_field_linear(
            &mut patched,
            edits,
            rows(to_srgb),
            rows(to_srgb.inverse()),
        )?;
        let geometry_edits;
        let geometry_edits = if flattened || !edits["flatFieldProfile"].is_null() {
            let mut e = edits.clone();
            e["flatFieldProfile"] = Value::Null;
            geometry_edits = e;
            &geometry_edits
        } else {
            edits
        };
        let base = DynamicImage::ImageRgba32F(patched);
        let (transformed, offset) = crate::apply_all_transformations(&base, geometry_edits);
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
    // Built lazily: only a pass whose tone controls move needs it.
    let neighbourhood_key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            transform,
            patches,
            max_dimension,
            image.width(),
            image.height(),
        )
            .hash(&mut hasher);
        hasher.finish()
    };
    let unedited = image.clone();
    let neighbourhood_for = |tone: &super::controls::Tone| {
        (!tone.is_neutral()).then(|| neighbourhood(state, &source, &unedited, neighbourhood_key))
    };
    let image = if controls.detail.is_neutral() && super::optics::is_neutral(&controls.effects) {
        image
    } else {
        spatial(
            state,
            &source,
            image,
            &controls,
            transform,
            patches,
            max_dimension,
            scale,
        )?
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
            context, state, path, edits, transform, patches,
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
        grain_amount: controls.effects.grain_amount,
        grain_size: controls.effects.grain_size,
        grain_roughness: controls.effects.grain_roughness,
        ..Default::default()
    };
    let look = look(state, edits)?;
    let initial_blurs = neighbourhood_for(&controls.tone);
    let mut initial_plan = plan(
        source.color.clone(),
        controls,
        output_transform(state),
        tone_mapper(edits),
    )?;
    initial_plan.set_render_scale(scale);
    if let Some(blurs) = initial_blurs {
        initial_plan.set_neighbourhood(blurs);
    }
    if active.is_empty() {
        if let Some(look) = &look {
            initial_plan.set_look(look)?;
        }
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
        let local = controls_for(&mask.adjustments, true)?;
        // Vignette, grain and the lens effects describe the whole frame; a
        // mask carrying them would be applying a frame effect to part of a
        // frame.
        // Glow and halation are the exception, as they were in the previous
        // engine: light spilling from one part of the picture is a local
        // idea, and they run on the working image like local detail.
        let local_light = super::controls::Effects {
            glow_amount: local.effects.glow_amount,
            halation_amount: local.effects.halation_amount,
            ..Default::default()
        };
        ensure!(
            super::controls::Effects {
                glow_amount: 0.,
                halation_amount: 0.,
                ..local.effects.clone()
            }
            .is_neutral(),
            "Vignette, grain, flare, Centre and lens corrections apply to the whole photo, not inside a mask."
        );
        let light_is_neutral = super::optics::light_is_neutral(&local_light);
        // `is_neutral` is about the pointwise pass; detail is its own stage.
        if local.is_neutral() && local.detail.is_neutral() && light_is_neutral {
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
        let input = if local.detail.is_neutral() && light_is_neutral {
            &working
        } else {
            let mut copy = working.clone();
            super::detail::apply(&mut copy, &local.detail, working_luminance, scale);
            // The working image is already exposed, so no further gain.
            super::optics::add_light(&mut copy, &local_light, 0., Primaries::DavinciWideGamut);
            detailed = copy;
            &detailed
        };
        let adjusted = if local.is_neutral() {
            input.clone()
        } else {
            let blurs = neighbourhood_for(&local.tone);
            let mut local_plan = plan(working_color.clone(), local, None, None)?;
            if let Some(blurs) = blurs {
                local_plan.set_neighbourhood(blurs);
            }
            engine.render_graded(input, &local_plan)?
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
        tone_mapper(edits),
    )?;
    final_plan.set_render_scale(scale);
    if let Some(look) = &look {
        final_plan.set_look(look)?;
    }
    let frame = engine.render(&working, &final_plan, false);
    watch.lap("final pass");
    frame
}

/// Automatic exposure, white balance, highlights and shadows for v3,
/// measured on the picture's scene data rather than its display pixels:
///
/// - exposure brings the log-average luminance (Reinhard's key, over the
///   1st–99th percentiles so a lamp or a black border does not decide it)
///   toward mid grey — nothing within half a stop, 60% of the rest, within
///   ±2 stops, because most high- and low-key pictures are meant that way;
/// - white balance is a grey-world estimate over the midtones at a third of
///   its strength, within ±25, because a sunset or a bar is not supposed to
///   be grey;
/// - highlights and shadows pull in what still clips or crushes once that
///   exposure is applied.
///
/// These are corrections, not a look: nothing creative is touched.
pub fn auto_controls(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
) -> Result<Value> {
    let neutral = ungraded(edits);
    let frame = render_file_with_capture(context, state, path, &neutral, Some(512), true)?;
    let graded = frame.stages.context("Missing analysis stage")?.graded;
    let y = spaces::rgb_to_xyz(Primaries::DavinciWideGamut).row(1);
    let weights = [y.x as f32, y.y as f32, y.z as f32];
    let mut lum: Vec<f32> = graded
        .pixels()
        .map(|p| (p[0] * weights[0] + p[1] * weights[1] + p[2] * weights[2]).max(1e-6))
        .collect();
    ensure!(!lum.is_empty(), "Nothing to analyse");
    lum.sort_by(f32::total_cmp);
    let (lo, hi) = (lum[lum.len() / 100], lum[lum.len() * 99 / 100]);
    let body: Vec<f32> = lum
        .iter()
        .copied()
        .filter(|v| (lo..=hi).contains(v))
        .collect();
    let key = (body.iter().map(|v| v.ln()).sum::<f32>() / body.len().max(1) as f32).exp();
    // High-key and low-key pictures are usually meant that way: within half
    // a stop of mid grey nothing moves, and beyond it only part of the way.
    let error = (0.18 / key).log2();
    let exposure = (error.signum() * (error.abs() - 0.5).max(0.) * 0.6).clamp(-2., 2.);

    // Grey world, in cone-ish RGB, over pixels within two stops of the key.
    let (mut sum, mut n) = ([0f64; 3], 0usize);
    for p in graded.pixels() {
        let l = p[0] * weights[0] + p[1] * weights[1] + p[2] * weights[2];
        if l > key / 4. && l < key * 4. && p.0[..3].iter().all(|v| *v > 0.) {
            for c in 0..3 {
                sum[c] += (p[c] as f64).ln();
            }
            n += 1;
        }
    }
    let (temperature, tint) = if n > 100 {
        let [r, g, b] = sum.map(|v| v / n as f64);
        let ln2 = std::f64::consts::LN_2;
        let t = -((r - b) / ln2) / 0.012 * 0.35;
        let k = ((g - (r + b) / 2.) / ln2) / 0.006 * 0.35;
        (t.clamp(-25., 25.) as f32, k.clamp(-25., 25.) as f32)
    } else {
        (0., 0.)
    };

    // What clips or crushes once exposed. The shared EV shift slider is the
    // previous engine's, which divides by 0.8.
    let ev_shift = exposure * 0.8;
    let mut exposed = neutral.clone();
    exposed["exposure"] = serde_json::json!(ev_shift);
    exposed["v3"] = serde_json::json!({"temperature": temperature, "tint": tint});
    let shown = render_file(context, state, path, &exposed, Some(512))?.encoded_srgb;
    let total = (shown.width() * shown.height()).max(1) as f32;
    let clipped = shown
        .pixels()
        .filter(|p| p.0[..3].iter().any(|v| *v > 0.99))
        .count() as f32
        / total;
    let crushed = shown
        .pixels()
        .filter(|p| p.0[..3].iter().all(|v| *v < 0.02))
        .count() as f32
        / total;
    let highlights = -(clipped * 1500.).clamp(0., 60.);
    let shadows = (crushed * 1000.).clamp(0., 50.);
    let round = |v: f32, places: i32| {
        let k = 10f64.powi(places);
        (v as f64 * k).round() / k + 0.0
    };
    // Shared controls at the top level, v3's own under `v3`, as saved.
    Ok(serde_json::json!({
        "exposure": round(ev_shift, 2),
        "highlights": round(highlights, 0),
        "shadows": round(shadows, 0),
        "v3": {
            "temperature": round(temperature, 0),
            "tint": round(tint, 0),
        },
    }))
}

#[tauri::command]
pub async fn auto_color_v3(
    path: String,
    edits: Value,
    app_handle: tauri::AppHandle,
) -> Result<Value, String> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let context = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle)?;
        auto_controls(&context, &state, &path, &edits).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
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
