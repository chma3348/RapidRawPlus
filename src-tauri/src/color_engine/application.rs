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
use image::{DynamicImage, GenericImageView};
use serde_json::Value;
use std::path::PathBuf;
use std::{
    sync::{Arc, Mutex},
    time::SystemTime,
};

pub struct SourceCache {
    path: std::path::PathBuf,
    length: u64,
    modified: SystemTime,
    input_digest: Option<String>,
    source_digest: String,
    recovery: super::raw::Recovery,
    quality: Quality,
    pub frame: Arc<DecodedFrame>,
}

/// How much of the source to develop. Thumbnails never share a cache with
/// the editor, so this cannot leak a fast demosaic into an edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Full demosaic; what the editor and exports use.
    Full,
    /// Speed demosaic for RAW, for pictures that will be shown small.
    Thumbnail,
}

/// Everything a render remembers between calls, for one consumer. The
/// editor's set lives in `AppState`; thumbnails, size estimates and other
/// side jobs get a fresh set, so browsing the library cannot evict the
/// decoded photo the editor is working on.
/// A cache keeping its two most recently used entries. The editor alternates
/// sizes — a fast one while a slider moves, the full one when it stops, and
/// zoomed in, the size the visible region is shown at — and with one slot
/// each switch rebuilt the prepared image and its neighbourhood (a second at
/// full size).
/// What a cache entry costs to keep, so the caches can hold to a budget.
pub trait Weigh {
    fn bytes(&self) -> usize;
}

pub struct Slots<T>(Vec<T>);

impl<T> Default for Slots<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T: Weigh> Slots<T> {
    /// Keep only the smallest entry (the picture on screen), letting go of
    /// a full-size one.
    pub fn keep_smallest(&mut self) {
        if let Some(smallest) = (0..self.0.len()).min_by_key(|&i| self.0[i].bytes()) {
            let keep = self.0.swap_remove(smallest);
            self.0.clear();
            self.0.push(keep);
        }
    }

    const KEEP: usize = 2;
    /// Two entries are kept (a zoomed-in editor alternates between the
    /// whole picture on screen and the full-size region), but never more than
    /// this between them unless one alone is larger.
    const BUDGET: usize = 768 << 20;

    /// The entry `hit` accepts, now the most recently used.
    pub fn find(&mut self, hit: impl Fn(&T) -> bool) -> Option<&T> {
        let i = self.0.iter().position(hit)?;
        let entry = self.0.remove(i);
        self.0.push(entry);
        self.0.last()
    }

    pub fn put(&mut self, entry: T) {
        self.0.push(entry);
        let total = |v: &Vec<T>| v.iter().map(Weigh::bytes).sum::<usize>();
        while self.0.len() > Self::KEEP || (self.0.len() > 1 && total(&self.0) > Self::BUDGET) {
            self.0.remove(0);
        }
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

/// A cached stage's picture at half precision: half the memory of 32-bit
/// floats, and plenty for scene-linear light (about 0.05% steps). Every
/// render uses the rounded picture, cached or freshly made, so the two can
/// never differ.
pub struct HalfImage {
    width: u32,
    height: u32,
    data: Vec<half::f16>,
}

impl HalfImage {
    fn new(image: &DynamicImage) -> Self {
        use rayon::prelude::*;
        let pixels = float_pixels(image);
        Self {
            width: pixels.width(),
            height: pixels.height(),
            data: pixels
                .as_raw()
                .par_iter()
                .map(|&v| half::f16::from_f32(v))
                .collect(),
        }
    }

    fn picture(&self) -> Arc<DynamicImage> {
        use rayon::prelude::*;
        let data: Vec<f32> = self.data.par_iter().map(|v| v.to_f32()).collect();
        Arc::new(DynamicImage::ImageRgba32F(
            image::Rgba32FImage::from_raw(self.width, self.height, data)
                .expect("a cached picture keeps its own size"),
        ))
    }
}

impl Weigh for PreparedCache {
    fn bytes(&self) -> usize {
        self.image.data.len() * 2
    }
}
impl Weigh for DetailCache {
    fn bytes(&self) -> usize {
        self.image.data.len() * 2
    }
}
impl Weigh for NeighbourhoodCache {
    fn bytes(&self) -> usize {
        self.blurs.len() * 16
    }
}

#[derive(Default)]
pub struct V3Caches {
    pub source: Mutex<Option<SourceCache>>,
    pub prepared: Mutex<Slots<PreparedCache>>,
    pub detail: Mutex<Slots<DetailCache>>,
    pub neighbourhood: Mutex<Slots<NeighbourhoodCache>>,
    pub sampling: Mutex<Option<(u64, Arc<DynamicImage>)>>,
    pub masks: Mutex<std::collections::HashMap<u64, Arc<image::GrayImage>>>,
    /// The photograph's own colour noise (detail::photo_colour_noise),
    /// measured once so every render of it, preview, zoomed region or
    /// export, reduces it alike.
    pub noise: Mutex<Option<(Arc<DecodedFrame>, f32)>>,
}

impl V3Caches {
    /// Zoomed back out: the full-size stages behind the sharp region are no
    /// longer needed; the whole picture on screen keeps its own.
    pub fn release_full_size(&self) {
        if let Ok(mut c) = self.prepared.lock() {
            c.keep_smallest();
        }
        if let Ok(mut c) = self.detail.lock() {
            c.keep_smallest();
        }
        if let Ok(mut c) = self.neighbourhood.lock() {
            c.keep_smallest();
        }
    }

    /// Let go of what can be rebuilt, when the system is short of memory:
    /// every derived stage, and under critical pressure the decoded photo
    /// too (it is decoded again on the next render).
    pub fn trim(&self, critical: bool) {
        if critical && let Ok(mut c) = self.source.lock() {
            *c = None;
        }
        if let Ok(mut c) = self.prepared.lock() {
            c.clear();
        }
        if let Ok(mut c) = self.detail.lock() {
            c.clear();
        }
        if let Ok(mut c) = self.neighbourhood.lock() {
            c.clear();
        }
        if let Ok(mut c) = self.sampling.lock() {
            *c = None;
        }
        if let Ok(mut c) = self.masks.lock() {
            c.clear();
        }
    }

    /// Forget everything: a different photo is being opened.
    pub fn clear(&self) {
        if let Ok(mut c) = self.noise.lock() {
            *c = None;
        }
        if let Ok(mut c) = self.source.lock() {
            *c = None;
        }
        if let Ok(mut c) = self.prepared.lock() {
            c.clear();
        }
        if let Ok(mut c) = self.detail.lock() {
            c.clear();
        }
        if let Ok(mut c) = self.neighbourhood.lock() {
            c.clear();
        }
        if let Ok(mut c) = self.sampling.lock() {
            *c = None;
        }
        if let Ok(mut c) = self.masks.lock() {
            c.clear();
        }
    }
}
pub struct EngineCache {
    device: Arc<wgpu::Device>,
    engine: ColorEngine,
}
pub struct DetailCache {
    source: Arc<DecodedFrame>,
    key: u64,
    image: HalfImage,
}
pub struct NeighbourhoodCache {
    source: Arc<DecodedFrame>,
    key: u64,
    blurs: Arc<Vec<[f32; 4]>>,
    tones: super::plan::PhotoTones,
}
pub struct PreparedCache {
    /// The photograph's size after geometry, before any preview downscale.
    full: (u32, u32),
    source: Arc<DecodedFrame>,
    transform: u64,
    patches: u64,
    dimension: Option<u32>,
    image: HalfImage,
    offset: (f32, f32),
    scale: f32,
}

fn float_pixels(image: &DynamicImage) -> std::borrow::Cow<'_, image::Rgba32FImage> {
    match image.as_rgba32f() {
        Some(pixels) => std::borrow::Cow::Borrowed(pixels),
        None => std::borrow::Cow::Owned(image.to_rgba32f()),
    }
}

pub fn source(state: &AppState, path: &str) -> Result<Arc<DecodedFrame>> {
    source_with_edits(state, path, &serde_json::json!({}))
}

pub fn source_with_edits(state: &AppState, path: &str, edits: &Value) -> Result<Arc<DecodedFrame>> {
    source_for(
        &state.v3,
        path,
        &super::identity::resolve(state, edits)?,
        Quality::Full,
    )
}

/// The installed input captures for this render, if any.
fn captured_input(pair: &super::identity::Resolved) -> Result<Option<super::cube::CapturedInput>> {
    let Some(srgb) = &pair.input else {
        return Ok(None);
    };
    Ok(Some(super::cube::CapturedInput {
        srgb: super::cube::CubeLut::load(srgb)
            .context("Could not load the installed v3 input transform")?,
        p3: pair
            .input_p3
            .as_ref()
            .map(|p| super::cube::CubeLut::load(p))
            .transpose()
            .context("Could not load the installed v3 Display P3 input transform")?,
    }))
}

fn source_for(
    caches: &V3Caches,
    path: &str,
    pair: &super::identity::Resolved,
    quality: Quality,
) -> Result<Arc<DecodedFrame>> {
    let (path, _) = crate::file_management::parse_virtual_path(path);
    let meta = std::fs::metadata(&path)?;
    let modified = meta.modified()?;
    let (source_digest, bytes) = super::file_version::digest(&path, false)?;
    // Decode depends on the installed input transforms too, not just the
    // photo's filename. Do not silently keep old pixels after replacing one.
    let input = captured_input(pair)?;
    let input_digest = input.as_ref().map(|c| c.digest());
    let hit = |c: &SourceCache| {
        c.path == path
            && c.length == meta.len()
            && c.modified == modified
            && c.input_digest == input_digest
            && c.source_digest == source_digest
            && c.recovery == pair.recovery
            && c.quality == quality
    };
    // The lock is held only to look, never while decoding: a decode takes
    // hundreds of milliseconds and must not block another render.
    if let Some(frame) = caches
        .source
        .lock()
        .ok()
        .and_then(|c| c.as_ref().filter(|c| hit(c)).map(|c| c.frame.clone()))
    {
        return Ok(frame);
    }
    let bytes = match bytes {
        Some(bytes) => bytes,
        None => std::fs::read(&path)?,
    };
    let is_raw = crate::formats::is_raw_file(&path);
    let mut frame = if is_raw {
        super::raw::decode_raw(&bytes, quality == Quality::Thumbnail, || Ok(()))?
    } else {
        super::input::decode_profiled_photo(&bytes)?
    };
    pair.recovery.apply(&mut frame);
    // A RAW opens as Lightroom's default rendering shows it (raw_look.rs);
    // a rendered photograph already does.
    if is_raw {
        super::raw_look::apply(&mut frame.pixels);
        frame
            .provenance
            .interpretation
            .push_str(" + raw_look_lightroom_default_1");
    }
    // A captured input transform turns a rendered photograph into scene data,
    // undoing whatever rendering it already carries, exactly as Resolve does
    // on import. After it the source is indistinguishable from a RAW's, so
    // everything downstream — including the captured output transform — is
    // already right for it. Done once here rather than per render: the
    // decoded source is cached.
    if frame.color.reference == ReferenceDomain::Display
        && let Some(captured) = &input
    {
        ensure!(
            pair.output.is_some(),
            "A captured v3 input transform requires its matching output transform"
        );
        // The sRGB capture covers sRGB 0..1 only. A Display P3 photo can
        // legitimately lie outside it: it takes the P3 capture when one is
        // installed, and is otherwise compressed into sRGB first. Either
        // way it stays on Resolve's path; before, one saturated pixel sent
        // the whole photo to the built-in rendering instead.
        let domain = captured.domain_for(&frame.pixels);
        captured.apply_as(domain, &mut frame.pixels);
        frame.color = SourceColor {
            primaries: Primaries::DavinciWideGamut,
            transfer: Transfer::Linear,
            reference: ReferenceDomain::Scene,
        };
        frame.rendered_origin = true;
        frame.input_domain = Some(domain);
        frame.provenance.transform_decision = match domain {
            super::cube::InputDomain::Srgb => "captured_input_to_linear_dwg_scene",
            super::cube::InputDomain::DisplayP3 => "captured_p3_input_to_linear_dwg_scene",
            super::cube::InputDomain::SrgbCompressed => {
                "compressed_into_srgb_then_captured_input_to_linear_dwg_scene"
            }
        }
        .into();
        frame
            .provenance
            .interpretation
            .push_str(" + captured_input_transform");
        if domain == super::cube::InputDomain::SrgbCompressed {
            let warning = "This photo has colours beyond sRGB and only an sRGB input transform is installed: its most saturated colours were compressed into sRGB before the transform. A Display P3 capture (input-transform-p3.cube) would keep them.";
            log::warn!("{warning}");
            frame.provenance.warnings.push(warning.into());
        }
        frame.provenance.warnings.push(format!(
            "Captured input transform digest: {}",
            captured.digest()
        ));
    }
    let frame = Arc::new(frame);
    if let Ok(mut cache) = caches.source.lock() {
        *cache = Some(SourceCache {
            path,
            length: meta.len(),
            modified,
            input_digest,
            source_digest,
            recovery: pair.recovery,
            quality,
            frame: frame.clone(),
        });
    }
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

pub(crate) fn ungraded(edits: &Value) -> Value {
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
    // The legacy parser ignores cube domains. V3 must refuse unsupported
    // domains instead of silently rendering the wrong colours, and must
    // notice when a LUT is replaced at the same path.
    let strict_cube = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cube"));
    let cached = state
        .lut_cache
        .lock()
        .ok()
        .and_then(|c| c.get(path).cloned());
    let lut = if strict_cube {
        let cube = super::cube::CubeLut::load(std::path::Path::new(path))?;
        // Converted once per cube version, not per render.
        Arc::new(crate::lut_processing::Lut {
            size: cube.size,
            data: cube
                .entries
                .iter()
                .flat_map(|p| p[..3].iter().copied())
                .collect(),
        })
    } else {
        match cached {
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
/// The rendering is always v3's own: the captured Resolve transform when
/// one is installed, the built-in rendering otherwise. The previous
/// engine's tone mappers (Basic, AgX, Filmic) are no longer offered; a
/// photo that saved one of them renders with v3 like every other.
fn tone_mapper(_edits: &Value) -> Option<OutputRendering> {
    None
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

/// The captured transforms, when a rendered picture needs its controls run
/// on display values (see `RenderPlan::set_display_domain`).
fn display_domain(
    pair: &super::identity::Resolved,
    source: &DecodedFrame,
) -> Result<Option<(Arc<super::cube::CubeLut>, Arc<super::cube::CubeLut>)>> {
    if !source.rendered_origin {
        return Ok(None);
    }
    match (&pair.input, &pair.output) {
        (Some(input), Some(output)) => Ok(Some((
            super::cube::CubeLut::load(input)?,
            super::cube::CubeLut::load(output)?,
        ))),
        _ => anyhow::bail!("The captured input/output transform pair is incomplete"),
    }
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
    caches: &V3Caches,
    mask: &crate::mask_generation::MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    offset: (f32, f32),
    sampled: Option<&Arc<DynamicImage>>,
    picture: (&str, u64, u64, Option<u64>),
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
    if let Some(hit) = caches.masks.lock().ok().and_then(|c| c.get(&key).cloned()) {
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
    if let Ok(mut cache) = caches.masks.lock() {
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
    caches: &V3Caches,
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
    [effects.ca_red_cyan, effects.ca_blue_yellow, effects.centre]
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
    if let Ok(mut cache) = caches.detail.lock()
        && let Some(c) = cache.find(|c| Arc::ptr_eq(&c.source, source) && c.key == key)
    {
        return Ok(c.image.picture());
    }
    // Luminance in the source's own primaries: the prepared image has not
    // been converted to the working space yet.
    let y = super::spaces::rgb_to_xyz(source.color.primaries).row(1);
    let weights = [y.x as f32, y.y as f32, y.z as f32];
    let noise = (controls.detail.color_noise > 0.).then(|| photo_noise(caches, source));
    let mut pixels = image.to_rgba32f();
    drop(image);
    super::optics::correct_chromatic_aberration(&mut pixels, effects);
    super::detail::apply(&mut pixels, &controls.detail, weights, scale, noise);
    if let Some(clarity) = super::optics::centre_clarity(effects) {
        let mut clarified = pixels.clone();
        super::detail::apply(&mut clarified, &clarity, weights, scale, noise);
        super::optics::blend_centre(&mut pixels, &clarified);
    }
    super::optics::add_light(
        &mut pixels,
        effects,
        controls.tone.exposure,
        source.color.primaries,
    );
    let stored = HalfImage::new(&DynamicImage::ImageRgba32F(pixels));
    let result = stored.picture();
    if let Ok(mut cache) = caches.detail.lock() {
        cache.put(DetailCache {
            source: source.clone(),
            key,
            image: stored,
        });
    }
    Ok(result)
}

/// The photograph's colour noise, measured once on the decoded source.
fn photo_noise(caches: &V3Caches, source: &Arc<DecodedFrame>) -> f32 {
    if let Ok(cache) = caches.noise.lock()
        && let Some((frame, noise)) = cache.as_ref()
        && Arc::ptr_eq(frame, source)
    {
        return *noise;
    }
    let noise = super::detail::photo_colour_noise(&source.pixels);
    if let Ok(mut cache) = caches.noise.lock() {
        *cache = Some((source.clone(), noise));
    }
    noise
}

/// The previous engine's tonal and structure blurs of the unedited picture,
/// which its local tone controls — contrast, shadows, whites, blacks,
/// highlights — read. Same radii (3.5 and 40 pixels, scaled by the short edge
/// over 1080, sigma half the radius), in the encoding it blurred in: linear
/// light for scene data, which it saw from RAW files, and sRGB code values
/// for display-referred pictures. Values are in linear sRGB primaries, where
/// those functions work. Two entries per pixel, tonal then structure.
fn neighbourhood(
    caches: &V3Caches,
    source: &Arc<DecodedFrame>,
    image: &DynamicImage,
    key: u64,
    display: Option<&super::cube::CubeLut>,
) -> (Arc<Vec<[f32; 4]>>, super::plan::PhotoTones) {
    if let Ok(mut cache) = caches.neighbourhood.lock()
        && let Some(c) = cache.find(|c| Arc::ptr_eq(&c.source, source) && c.key == key)
    {
        return (c.blurs.clone(), c.tones);
    }
    let pixels = float_pixels(image);
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
    // The tone zones' key: the working values' brightness in DaVinci
    // Intermediate, whatever the previous engine's planes are encoded as.
    let to_working = spaces::conversion(source.color.primaries, Primaries::DavinciWideGamut)
        .transpose()
        .to_cols_array_2d()
        .map(|r| r.map(|v| v as f32));
    // Brightness by positive weights (see `shadow_key` in the shader): the
    // working space's luminance reads deep blues as nearly black.
    let luminance = [0.2126f32, 0.7152, 0.0722];
    let rec709 = [0.2126f32, 0.7152, 0.0722];
    let mut planes = vec![vec![0f32; w * h]; 3];
    let mut zone_key = vec![0f32; w * h];
    // The detail a Shadows lift brings out is measured against the Rec.709
    // luma of the working values in Intermediate, blurred at its own radius
    // (see `shadows_finish` in the shader).
    let mut detail = vec![0f32; w * h];
    // Every pixel on its own, so in parallel, in chunks of the same range of
    // each output.
    {
        use rayon::prelude::*;
        const CHUNK: usize = 1 << 14;
        let raw = pixels.as_raw();
        let [p0, p1, p2] = &mut planes[..] else {
            unreachable!()
        };
        p0.par_chunks_mut(CHUNK)
            .zip(p1.par_chunks_mut(CHUNK))
            .zip(p2.par_chunks_mut(CHUNK))
            .zip(zone_key.par_chunks_mut(CHUNK))
            .zip(detail.par_chunks_mut(CHUNK))
            .enumerate()
            .for_each(|(chunk, ((((o0, o1), o2), keys), details))| {
                for j in 0..o0.len() {
                    let i = chunk * CHUNK + j;
                    let p = &raw[i * 4..i * 4 + 3];
                    let working: [f32; 3] = std::array::from_fn(|c| {
                        to_working[c][0] * p[0] + to_working[c][1] * p[1] + to_working[c][2] * p[2]
                    });
                    let y: f32 = (0..3)
                        .map(|c| luminance[c] * working[c].max(0.))
                        .sum::<f32>()
                        .max(0.);
                    keys[j] = spaces::encode_intermediate(y as f64) as f32;
                    details[j] = (0..3)
                        .map(|c| {
                            rec709[c]
                                * spaces::encode_intermediate(working[c].max(0.) as f64) as f32
                        })
                        .sum();
                    // A rendered picture's controls see its display values:
                    // the code values Resolve's rendering gives the scene
                    // data, already encoded.
                    let values: [f32; 3] = if let Some(output) = display {
                        let logged = [p[0], p[1], p[2]]
                            .map(|v| spaces::encode_intermediate(v as f64) as f32);
                        output.sample(logged).map(|v| v.clamp(0., 1.))
                    } else {
                        std::array::from_fn(|c| {
                            let v = (m[c][0] * p[0] + m[c][1] * p[1] + m[c][2] * p[2])
                                .clamp(0., 65504.);
                            if scene { v } else { encode(v) }
                        })
                    };
                    o0[j] = values[0];
                    o1[j] = values[1];
                    o2[j] = values[2];
                }
            });
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
    // Where the photo's tones sit, for the sliders that adapt to it as
    // Lightroom's do (Highlights to its middle, Whites to its brightest).
    let tones = super::plan::PhotoTones::of(&zone_key);
    // The tone zones judge a region, not a pixel, so texture inside a
    // shadow lifts with it; an edge-aware average, so a dark subject against
    // a bright sky is its own region and lifts without a halo. Differences
    // under about 1.4 stops count as texture (eps), stronger ones as edges.
    let zone_key = guided_filter(
        &zone_key,
        w,
        h,
        (ZONE_KEY_RADIUS * w.min(h) as f32).round().max(1.) as usize,
        ZONE_KEY_EPS,
    );
    // Sigma 0.8% of the short edge, fitted on Resolve's Shadows exports.
    let detail = {
        let radius = (DETAIL_BLUR_BASE * scale).ceil().max(1.) as usize;
        if radius <= 24 {
            exact_gaussian(&detail, w, h, radius)
        } else {
            super::detail::blur(
                &detail,
                w,
                h,
                super::detail::Gaussian::new(radius as f32 / 2.),
            )
        }
    };
    let mut blurs = Vec::with_capacity(w * h * 2);
    for i in 0..w * h {
        blurs.push([tonal[0][i], tonal[1][i], tonal[2][i], 0.]);
        blurs.push([
            structure[0][i],
            structure[1][i],
            structure[2][i],
            pack_keys(zone_key[i], detail[i]),
        ]);
    }
    let blurs = Arc::new(blurs);
    if let Ok(mut cache) = caches.neighbourhood.lock() {
        cache.put(NeighbourhoodCache {
            source: source.clone(),
            key,
            blurs: blurs.clone(),
            tones,
        });
    }
    (blurs, tones)
}

/// The zone key's region size, as a fraction of the short edge, and how big a
/// difference in Intermediate (squared) counts as an edge rather than texture.
const ZONE_KEY_RADIUS: f32 = 0.02;
const ZONE_KEY_EPS: f32 = 0.01;

/// He, Sun and Tang's guided filter, guided by the image itself: an
/// edge-preserving average. Flat and textured areas are averaged; edges
/// whose variance exceeds `eps` are kept.
fn guided_filter(plane: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    use rayon::prelude::*;
    let squares: Vec<f32> = plane.par_iter().map(|v| v * v).collect();
    let (mean, mean_sq) = rayon::join(|| box_mean(plane, w, h, r), || box_mean(&squares, w, h, r));
    let (a, b): (Vec<f32>, Vec<f32>) = mean
        .par_iter()
        .zip(mean_sq.par_iter())
        .map(|(&m, &m2)| {
            let variance = (m2 - m * m).max(0.);
            let a = variance / (variance + eps);
            (a, m - a * m)
        })
        .unzip();
    let (mean_a, mean_b) = rayon::join(|| box_mean(&a, w, h, r), || box_mean(&b, w, h, r));
    plane
        .par_iter()
        .zip(mean_a.par_iter().zip(mean_b.par_iter()))
        .map(|(&i, (&ma, &mb))| ma * i + mb)
        .collect()
}

/// Mean over a (2r+1)-square window, clamped at the borders (divided by
/// the pixels actually inside), in time proportional to the pixel count
/// whatever the radius.
fn box_mean(plane: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    use rayon::prelude::*;
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let src = &plane[y * w..(y + 1) * w];
        let mut prefix = vec![0f64; w + 1];
        for x in 0..w {
            prefix[x + 1] = prefix[x] + src[x] as f64;
        }
        for (x, o) in out.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            *o = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
        }
    });
    // Columns: a running window of rows, contiguous across a band of the
    // width; bands run in parallel and are written back row by row.
    const BAND: usize = 256;
    let bands: Vec<(usize, Vec<f32>)> = (0..w.div_ceil(BAND))
        .into_par_iter()
        .map(|band| {
            let (x0, x1) = (band * BAND, ((band + 1) * BAND).min(w));
            let bw = x1 - x0;
            let mut out = vec![0f32; bw * h];
            let mut sum = vec![0f64; bw];
            let mut count = 0usize;
            let (mut lo, mut hi) = (0usize, 0usize);
            for y in 0..h {
                let (want_lo, want_hi) = (y.saturating_sub(r), (y + r + 1).min(h));
                while hi < want_hi {
                    for (s, v) in sum.iter_mut().zip(&rows[hi * w + x0..hi * w + x1]) {
                        *s += *v as f64;
                    }
                    hi += 1;
                    count += 1;
                }
                while lo < want_lo {
                    for (s, v) in sum.iter_mut().zip(&rows[lo * w + x0..lo * w + x1]) {
                        *s -= *v as f64;
                    }
                    lo += 1;
                    count -= 1;
                }
                for (o, s) in out[y * bw..(y + 1) * bw].iter_mut().zip(&sum) {
                    *o = (*s / count as f64) as f32;
                }
            }
            (x0, out)
        })
        .collect();
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x0, band) in &bands {
            let bw = band.len() / h;
            row[*x0..x0 + bw].copy_from_slice(&band[y * bw..(y + 1) * bw]);
        }
    });
    out
}

/// The Shadows detail blur, in the neighbourhood's units (radius at a 1080
/// pixel short edge; sigma is half the radius): sigma = 0.8% of the short edge.
const DETAIL_BLUR_BASE: f32 = 17.28;

/// Two keys in one slot at half precision (0.0005 in Intermediate, under a
/// hundredth of a stop): the structure entry's fourth component carries the
/// Shadows key and the detail base, unpacked by `unpack2x16float` in WGSL.
fn pack_keys(key: f32, detail: f32) -> f32 {
    let lo = half::f16::from_f32(key).to_bits() as u32;
    let hi = half::f16::from_f32(detail).to_bits() as u32;
    f32::from_bits(lo | (hi << 16))
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
#[allow(clippy::too_many_arguments)]
fn sampling_image(
    context: &GpuContext,
    state: &AppState,
    caches: &V3Caches,
    quality: Quality,
    path: &str,
    edits: &Value,
    transform: u64,
    patches: u64,
) -> Result<(u64, Arc<DynamicImage>)> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    transform.hash(&mut hasher);
    patches.hash(&mut hasher);
    let (real_path, _) = crate::file_management::parse_virtual_path(path);
    let meta = std::fs::metadata(&real_path)?;
    meta.len().hash(&mut hasher);
    meta.modified()?.hash(&mut hasher);
    super::file_version::digest(&real_path, false)?
        .0
        .hash(&mut hasher);
    let neutral = ungraded(edits);
    neutral.to_string().hash(&mut hasher);
    let pair = super::identity::resolve(state, edits)?;
    for transform in [pair.input, pair.input_p3, pair.output] {
        transform
            .map(|p| super::cube::CubeLut::load(&p).map(|c| c.digest.clone()))
            .transpose()?
            .hash(&mut hasher);
    }
    let key = hasher.finish();
    if let Ok(cache) = caches.sampling.lock()
        && let Some((cached, image)) = cache.as_ref()
        && *cached == key
    {
        return Ok((key, image.clone()));
    }
    // Neutral: the same engine, the same transforms, no controls.
    // The sampling render goes through the same prepared-image cache as the
    // preview. Put the preview's entry back afterwards, or the next slider
    // move pays to rebuild it from the full-resolution source.
    let preview = caches
        .prepared
        .lock()
        .ok()
        .map(|mut c| std::mem::take(&mut *c));
    // Masks sample colours in sRGB, whatever the output space.
    let frame = render(
        context,
        state,
        caches,
        quality,
        path,
        &neutral,
        None,
        false,
        OutputSpace::Srgb,
    );
    if let (Some(entries), Ok(mut cache)) = (preview, caches.prepared.lock()) {
        *cache = entries;
    }
    let image = Arc::new(DynamicImage::ImageRgba8(frame?.preview_rgba8()));
    if let Ok(mut cache) = caches.sampling.lock() {
        *cache = Some((key, image.clone()));
    }
    Ok((key, image))
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
    render(
        context,
        state,
        &state.v3,
        Quality::Full,
        path,
        edits,
        max_dimension,
        false,
        OutputSpace::Srgb,
    )
}

/// A library thumbnail: speed demosaic, and a cache set of its own so it
/// never evicts what the editor is working on.
pub fn render_thumbnail(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    max_dimension: u32,
) -> Result<RenderedFrame> {
    let caches = V3Caches::default();
    render(
        context,
        state,
        &caches,
        Quality::Thumbnail,
        path,
        edits,
        Some(max_dimension),
        false,
        OutputSpace::Srgb,
    )
}

/// A render that must not disturb the editor's caches (size estimates,
/// previews of other photos), at full quality.
pub fn render_aside(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    max_dimension: Option<u32>,
) -> Result<RenderedFrame> {
    let caches = V3Caches::default();
    render(
        context,
        state,
        &caches,
        Quality::Full,
        path,
        edits,
        max_dimension,
        false,
        OutputSpace::Srgb,
    )
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

/// Diagnostic variant of the shared render path. With capture enabled,
/// `working` is the initial working-space image and `graded` includes masks.
/// A frame rendered through the sRGB path, stored in Display P3 when that is
/// the requested space; a frame rendered through the P3 capture is P3 already.
fn finish_space(frame: &mut RenderedFrame, space: OutputSpace, native: bool) {
    if space == OutputSpace::DisplayP3 && !native {
        super::cube::srgb_encoded_to_p3(&mut frame.encoded_srgb);
    }
    frame.space = space;
}

/// The colour space the editor preview and exports use (the app setting).
pub fn output_space(state: &AppState) -> OutputSpace {
    state.output_space.lock().map(|s| *s).unwrap_or_default()
}

/// As `render_file`, in the chosen output space: for what a person looks at
/// in the editor and for exported files, which must always agree.
pub fn render_for_output(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    max_dimension: Option<u32>,
) -> Result<RenderedFrame> {
    render(
        context,
        state,
        &state.v3,
        Quality::Full,
        path,
        edits,
        max_dimension,
        false,
        output_space(state),
    )
}

pub fn render_file_with_capture(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    max_dimension: Option<u32>,
    capture: bool,
) -> Result<RenderedFrame> {
    render(
        context,
        state,
        &state.v3,
        Quality::Full,
        path,
        edits,
        max_dimension,
        capture,
        OutputSpace::Srgb,
    )
}

#[allow(clippy::too_many_arguments)]
fn render(
    context: &GpuContext,
    state: &AppState,
    caches: &V3Caches,
    quality: Quality,
    path: &str,
    edits: &Value,
    max_dimension: Option<u32>,
    capture: bool,
    space: OutputSpace,
) -> Result<RenderedFrame> {
    let normalized = super::migration::normalize(edits)?;
    let edits = normalized.as_ref();
    validate_features(edits)?;
    let mut watch = Stopwatch::start();
    let mut controls = controls(edits)?;
    if crate::formats::is_raw_file(path) {
        controls.detail = controls.detail.with_raw_defaults(&edits["v3"]["detail"]);
    }
    let pair = super::identity::resolve(state, edits)?;
    let source = source_for(caches, path, &pair, quality)?;
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
    let mut prepared = caches
        .prepared
        .lock()
        .map_err(|_| anyhow::anyhow!("V3 preview cache unavailable"))?;
    let found = prepared
        .find(|p| {
            Arc::ptr_eq(&p.source, &source)
                && p.transform == transform
                && p.patches == patches
                && p.dimension == max_dimension
        })
        .map(|p| (p.image.picture(), p.offset, p.scale, p.full));
    let (image, offset, scale, full) = if let Some(found) = found {
        found
    } else {
        // Patches join at the decoded-source stage, before geometry, because
        // the mask stored with a patch is in those coordinates.
        let patched = if super::patches::visible(edits).is_empty() {
            source.pixels.clone()
        } else {
            // Only rendered sources went through this transform. Native RAW
            // is scene-linear sRGB, not transformed DWG: do not reinterpret
            // a display-encoded patch as DWG and blend it into sRGB there.
            let captured = if source.rendered_origin {
                captured_input(&pair)?
            } else {
                None
            };
            let mut pixels = source.pixels.clone();
            super::patches::composite(
                &mut pixels,
                edits,
                &source.color,
                source.source_profile.as_deref(),
                captured.as_ref().zip(source.input_domain),
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
        // Handed over whole, so a frame with no geometry is not copied again.
        let (transformed, offset) =
            crate::apply_all_transformations(DynamicImage::ImageRgba32F(patched), geometry_edits);
        let full = transformed.dimensions();
        let full_width = full.0;
        let image = if let Some(dim) = max_dimension {
            ensure!((16..=16384).contains(&dim), "Invalid v3 preview dimensions");
            crate::image_processing::resample_f32_image(&transformed, dim, dim)
        } else {
            transformed.into_owned()
        };
        let scale = image.width() as f32 / full_width as f32;
        let stored = HalfImage::new(&image);
        drop(image);
        let image = stored.picture();
        prepared.put(PreparedCache {
            full,
            source: source.clone(),
            transform,
            patches,
            dimension: max_dimension,
            image: stored,
            offset,
            scale,
        });
        (image, offset, scale, full)
    };
    drop(prepared);
    // Built lazily: only a pass whose tone controls move needs it.
    let domain = display_domain(&pair, &source)?;
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
        domain
            .as_ref()
            .map(|(_, output)| &output.digest)
            .hash(&mut hasher);
        hasher.finish()
    };
    let unedited = image.clone();
    let neighbourhood_for = |tone: &super::controls::Tone| {
        (!tone.is_neutral()).then(|| {
            neighbourhood(
                caches,
                &source,
                &unedited,
                neighbourhood_key,
                domain.as_ref().map(|(_, output)| output.as_ref()),
            )
        })
    };
    let in_domain = |plan: &mut RenderPlan| {
        if let Some((input, output)) = &domain {
            plan.set_shared_display_domain(input, output);
        }
    };
    let image = if controls.detail.is_neutral() && super::optics::is_neutral(&controls.effects) {
        image
    } else {
        spatial(
            caches,
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
            context, state, caches, quality, path, edits, transform, patches,
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
    // Display P3 straight from Resolve's P3 output capture when the picture
    // ends in that capture: not through the previous engine's tone mappers,
    // and not into a creative LUT made for display sRGB. Otherwise the sRGB
    // rendering is stored as P3 at the end (`finish_space`), which looks the
    // same and keeps every other path exactly as it was.
    let native_p3 = space == OutputSpace::DisplayP3
        && pair.output_p3.is_some()
        && tone_mapper(edits).is_none()
        && !look
            .as_ref()
            .is_some_and(|l| l.space == super::plan::LookSpace::Display);
    let output = if native_p3 {
        pair.output_p3.clone()
    } else {
        pair.output.clone()
    };
    let controls_dehaze = controls.detail.dehaze;
    let initial_blurs = neighbourhood_for(&controls.tone);
    let initial_native = native_p3 && source.color.reference == ReferenceDomain::Scene;
    let mut initial_plan = plan(
        source.color.clone(),
        controls,
        output.clone(),
        tone_mapper(edits),
    )?;
    initial_plan.set_render_scale(scale);
    in_domain(&mut initial_plan);
    let photo_tones = initial_blurs.as_ref().map(|(_, tones)| *tones);
    if let Some((blurs, tones)) = initial_blurs {
        initial_plan.set_neighbourhood(blurs, tones);
    }
    // Negative Dehaze's veil takes the photo's haze colour, a statistic of
    // the whole unedited picture.
    if controls_dehaze < 0.0 {
        initial_plan.set_haze(super::detail::airlight(float_pixels(&unedited).as_raw()));
    }
    if active.is_empty() {
        if let Some(look) = &look {
            initial_plan.set_look(look)?;
        }
        let mut frame = engine.render(&float_pixels(&image), &initial_plan, capture)?;
        frame.full_size = full;
        frame.tones = photo_tones;
        finish_space(&mut frame, space, initial_native);
        return Ok(frame);
    }
    // With masks, the first pass only feeds the local adjustments, which read
    // the graded stage alone.
    let mut original_working = None;
    let mut working = if capture {
        let stages = engine
            .render(&float_pixels(&image), &initial_plan, true)?
            .stages
            .context("Missing local-adjustment stage")?;
        original_working = Some(stages.working);
        stages.graded
    } else {
        engine.render_graded(&float_pixels(&image), &initial_plan)?
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
            caches,
            mask,
            image.width(),
            image.height(),
            scale,
            (offset.0 * scale, offset.1 * scale),
            sampled.as_ref().map(|(_, image)| image),
            (
                path,
                transform,
                patches,
                sampled.as_ref().map(|(key, _)| *key),
            ),
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
            let noise = (local.detail.color_noise > 0.).then(|| photo_noise(caches, &source));
            super::detail::apply(&mut copy, &local.detail, working_luminance, scale, noise);
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
            in_domain(&mut local_plan);
            if let Some((blurs, tones)) = blurs {
                local_plan.set_neighbourhood(blurs, tones);
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
    let final_native = native_p3 && working_color.reference == ReferenceDomain::Scene;
    let mut final_plan = plan(
        working_color,
        Controls {
            effects: grain,
            ..Controls::default()
        },
        output.clone(),
        tone_mapper(edits),
    )?;
    final_plan.set_render_scale(scale);
    in_domain(&mut final_plan);
    if let Some(look) = &look {
        final_plan.set_look(look)?;
    }
    let mut frame = engine.render(&working, &final_plan, capture)?;
    frame.full_size = full;
    frame.tones = photo_tones;
    finish_space(&mut frame, space, final_native);
    if let (Some(stages), Some(original)) = (&mut frame.stages, original_working) {
        stages.working = original;
    }
    watch.lap("final pass");
    Ok(frame)
}

/// Machine-readable interpretation of this source with these exact edits.
/// This is an audit snapshot, not a portable preset or a second color policy.
pub fn input_report(state: &AppState, path: &str, edits: &Value) -> Result<Value> {
    interpretation_report(state, path, edits, true)
}

/// Routine sidecar diagnostics validate file versions but reuse content hashes.
/// Reference intake uses input_report instead, which explicitly rereads bytes.
pub(crate) fn input_snapshot(state: &AppState, path: &str, edits: &Value) -> Result<Value> {
    interpretation_report(state, path, edits, false)
}

fn interpretation_report(
    state: &AppState,
    path: &str,
    edits: &Value,
    fresh: bool,
) -> Result<Value> {
    let normalized = super::migration::normalize(edits)?;
    let edits = normalized.as_ref();
    if fresh {
        let (real_path, _) = crate::file_management::parse_virtual_path(path);
        // Recheck bytes before source_for, so a fresh hash cannot certify stale pixels.
        super::file_version::digest(&real_path, true)?;
    }
    let pair = super::identity::resolve(state, edits)?;
    let frame = source_for(&state.v3, path, &pair, Quality::Full)?;
    let digest = |p: &Option<PathBuf>| -> Result<Option<String>> {
        p.as_ref()
            .map(|p| Ok(super::file_version::digest(p, fresh)?.0))
            .transpose()
    };
    Ok(serde_json::json!({
        "schema": 1, "source": frame.color, "provenance": frame.provenance,
        "effective_input_policy": super::identity::INPUT_POLICY,
        "saved_input_policy": edits["v3Pipeline"]["input_policy"],
        "input_policy_mismatch": edits["v3Pipeline"]["input_policy"].as_str().is_some_and(|p| p != super::identity::INPUT_POLICY),
        "width": frame.pixels.width(), "height": frame.pixels.height(),
        "input_transform_hash": digest(&pair.input)?,
        "input_transform_p3_hash": digest(&pair.input_p3)?,
        "output_transform_hash": digest(&pair.output)?,
        "raw_recovery": pair.recovery,
        "stage_revision": super::contract::REVISION,
        "implementation_digest": super::contract::implementation_digest(),
        "stage_order": super::contract::ORDER,
        "output_encoding": "srgb_d65_full_range",
        "preview_policy": "downsample_before_spatial_and_grade_v1",
        "pipeline": edits.get("v3Pipeline"),
    }))
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

    // What clips or crushes once exposed. The Exposure slider reads in stops.
    let ev_shift = exposure;
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

/// Explicit adoption of this build, not an automatic migration on load/save.
#[tauri::command]
pub async fn pin_color_v3(
    path: String,
    edits: Value,
    app_handle: tauri::AppHandle,
) -> Result<Value, String> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || -> Result<Value, String> {
        let state = app_handle.state::<AppState>();
        let root = app_handle.path().app_data_dir().map_err(|e| e.to_string())?.join("color-v3-assets");
        *state.v3_asset_dir.lock().map_err(|_| "V3 asset store unavailable")? = Some(root);
        let mut pipeline = super::identity::pin(&state, &edits).map_err(|e| format!("{e:#}"))?;
        // Explicit UI adoption. Revision 2 preserves revision 1's default
        // pixels and adds a reversible RAW recovery choice. Keep its assets.
        pipeline.engine = super::identity::ENGINE_REVISION.into();
        let pinned = serde_json::json!({"v3Pipeline":pipeline});
        let pair = super::identity::resolve(&state, &pinned).map_err(|e| format!("{e:#}"))?;
        let frame = source_for(&state.v3, &path, &pair, Quality::Full).map_err(|e| format!("{e:#}"))?;
        Ok(serde_json::json!({"pipeline":pipeline,"source":frame.color,"provenance":frame.provenance}))
    }).await.map_err(|e| e.to_string())?
}

pub fn preview_bytes(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    dimension: u32,
) -> Result<Vec<u8>, String> {
    let frame = render_for_output(context, state, path, edits, Some(dimension))
        .map_err(|e| e.to_string())?;
    // Every caller shows it on screen (before/after, the crop view, preset
    // and LUT previews): the on-screen encoding, quick to make.
    let mut bytes = Vec::new();
    frame
        .write_display_png(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

#[cfg(test)]
mod zone_key_tests {
    use super::*;
    #[test]
    fn box_mean_matches_a_direct_average() {
        let (w, h, r) = (13usize, 9usize, 3usize);
        let plane: Vec<f32> = (0..w * h).map(|i| ((i * 37) % 11) as f32).collect();
        let fast = box_mean(&plane, w, h, r);
        for y in 0..h {
            for x in 0..w {
                let (mut sum, mut n) = (0f64, 0f64);
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                        sum += plane[yy * w + xx] as f64;
                        n += 1.;
                    }
                }
                assert!((fast[y * w + x] as f64 - sum / n).abs() < 1e-4);
            }
        }
    }
    #[test]
    fn guided_filter_keeps_edges_and_averages_texture() {
        // Left half dark with fine texture, right half bright and flat.
        let (w, h) = (80usize, 20usize);
        let plane: Vec<f32> = (0..w * h)
            .map(|i| {
                let x = i % w;
                if x < w / 2 {
                    0.2 + if (i / w + x) % 2 == 0 { 0.02 } else { -0.02 }
                } else {
                    0.7
                }
            })
            .collect();
        let out = guided_filter(&plane, w, h, 6, ZONE_KEY_EPS);
        let row = &out[10 * w..11 * w];
        // Texture averaged away on the dark side, the edge kept sharp: a
        // plain blur would put 0.45 at the edge. Within the radius each side
        // leans a little toward the other (the guided filter's known, mild
        // bleed), and not at all beyond twice the radius.
        assert!(
            row[10..30].iter().all(|v| (v - 0.2).abs() < 0.006),
            "{:?}",
            &row[10..30]
        );
        assert!(
            row[w / 2] - row[w / 2 - 1] > 0.35,
            "edge blurred: {:?}",
            &row[36..44]
        );
        assert!(row[w / 2..].iter().all(|v| (v - 0.7).abs() < 0.06));
        // Two averaging passes: the reach is twice the radius.
        assert!(row[w / 2 + 13..].iter().all(|v| (v - 0.7).abs() < 0.002));
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    fn cube_text(value: f32) -> String {
        format!(
            "LUT_3D_SIZE 2\n{}",
            format!("{value} {value} {value}\n").repeat(8)
        )
    }

    #[test]
    fn source_follows_transform_replacement_and_reports_broken_transforms() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.png");
        image::ImageBuffer::from_pixel(16, 16, image::Rgba([80u8, 90, 100, 255]))
            .save(&photo)
            .unwrap();
        let lut = dir.path().join("input.cube");
        std::fs::write(&lut, cube_text(0.2)).unwrap();
        let state = AppState::default();
        *state.input_transform.lock().unwrap() = Some(lut.clone());
        *state.output_transform.lock().unwrap() = Some(lut.clone());
        let path = photo.to_str().unwrap();
        let before = source(&state, path).unwrap();
        assert!(before.rendered_origin);
        std::fs::write(&lut, cube_text(0.35)).unwrap(); // distinct length, not dependent on timestamp precision
        let after = source(&state, path).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_ne!(before.pixels.get_pixel(0, 0), after.pixels.get_pixel(0, 0));
        std::fs::write(&lut, "invalid cube").unwrap();
        assert!(
            source(&state, path).is_err(),
            "must not silently render with stale or fallback data"
        );
    }

    fn p3_red(dir: &std::path::Path) -> std::path::PathBuf {
        use image::ImageEncoder;
        let photo = dir.join("p3.png");
        let mut encoder =
            image::codecs::png::PngEncoder::new(std::fs::File::create(&photo).unwrap());
        encoder
            .set_icc_profile(moxcms::ColorProfile::new_display_p3().encode().unwrap())
            .unwrap();
        encoder
            .write_image(
                &[255, 0, 0, 255, 128, 128, 128, 255],
                2,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        photo
    }

    #[test]
    fn wide_gamut_source_stays_on_the_captured_path_by_compression() {
        // Only an sRGB capture installed: the P3 red is compressed into sRGB
        // and still goes through the transform; the grey is untouched by the
        // compression and lands exactly where an sRGB photo's grey would.
        let dir = tempfile::tempdir().unwrap();
        let photo = p3_red(dir.path());
        let lut = dir.path().join("input.cube");
        std::fs::write(&lut, cube_text(0.2)).unwrap();
        let state = AppState::default();
        *state.input_transform.lock().unwrap() = Some(lut.clone());
        *state.output_transform.lock().unwrap() = Some(lut);
        let decoded = source(&state, photo.to_str().unwrap()).unwrap();
        assert!(decoded.rendered_origin);
        assert_eq!(decoded.color.reference, ReferenceDomain::Scene);
        assert_eq!(
            decoded.input_domain,
            Some(super::super::cube::InputDomain::SrgbCompressed)
        );
        assert!(
            decoded
                .provenance
                .warnings
                .iter()
                .any(|w| w.contains("compressed into sRGB"))
        );
    }

    #[test]
    fn reports_adopt_current_policy_without_changing_current_rendering() {
        let dir = tempfile::tempdir().unwrap();
        let photo = p3_red(dir.path());
        let lut = dir.path().join("input.cube");
        std::fs::write(&lut, cube_text(0.2)).unwrap();
        let state = AppState::default();
        *state.input_transform.lock().unwrap() = Some(lut.clone());
        *state.output_transform.lock().unwrap() = Some(lut);
        *state.v3_asset_dir.lock().unwrap() = Some(dir.path().join("assets"));
        let mut identity = super::super::identity::pin(&state, &serde_json::json!({})).unwrap();
        let current = serde_json::json!({"processVersion":3,"v3Pipeline":identity});
        let before = source_with_edits(&state, photo.to_str().unwrap(), &current).unwrap();
        assert_eq!(
            input_snapshot(&state, photo.to_str().unwrap(), &current).unwrap()["input_policy_mismatch"],
            false
        );
        identity.input_policy = "profiled-display-cube-or-wide-gamut-bypass-1".into();
        let legacy = serde_json::json!({"processVersion":3,"v3Pipeline":identity});
        let snapshot = input_snapshot(&state, photo.to_str().unwrap(), &legacy).unwrap();
        let audit = input_report(&state, photo.to_str().unwrap(), &legacy).unwrap();
        assert_eq!(snapshot, audit);
        assert_eq!(audit["input_policy_mismatch"], false);
        assert_eq!(
            audit["pipeline"]["input_policy"],
            super::super::identity::INPUT_POLICY
        );
        let after = source_with_edits(&state, photo.to_str().unwrap(), &legacy).unwrap();
        assert_eq!(before.pixels, after.pixels);
    }

    #[test]
    fn wide_gamut_source_takes_the_p3_capture_when_installed() {
        let dir = tempfile::tempdir().unwrap();
        let photo = p3_red(dir.path());
        let srgb = dir.path().join("input.cube");
        let p3 = dir.path().join("input-p3.cube");
        std::fs::write(&srgb, cube_text(0.2)).unwrap();
        std::fs::write(&p3, cube_text(0.2)).unwrap();
        let state = AppState::default();
        *state.input_transform.lock().unwrap() = Some(srgb.clone());
        *state.input_transform_p3.lock().unwrap() = Some(p3);
        *state.output_transform.lock().unwrap() = Some(srgb);
        let decoded = source(&state, photo.to_str().unwrap()).unwrap();
        assert_eq!(
            decoded.input_domain,
            Some(super::super::cube::InputDomain::DisplayP3)
        );
        assert!(
            !decoded
                .provenance
                .warnings
                .iter()
                .any(|w| w.contains("compressed"))
        );
        // And pinning records the P3 capture too.
        *state.v3_asset_dir.lock().unwrap() = Some(dir.path().join("assets"));
        let identity = super::super::identity::pin(&state, &serde_json::json!({})).unwrap();
        assert!(identity.input_transform_p3.is_some());
    }

    #[test]
    fn creative_cubes_do_not_inherit_legacy_domain_or_stale_cache_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("look.cube");
        std::fs::write(&path, cube_text(0.2)).unwrap();
        let state = AppState::default();
        let edits = serde_json::json!({"lutPath":path});
        assert_eq!(look(&state, &edits).unwrap().unwrap().lut.data[0], 0.2);
        std::fs::write(&path, cube_text(0.35)).unwrap();
        assert_eq!(look(&state, &edits).unwrap().unwrap().lut.data[0], 0.35);
        std::fs::write(&path, format!("DOMAIN_MAX 2 2 2\n{}", cube_text(0.35))).unwrap();
        assert!(look(&state, &edits).is_err());
    }

    #[test]
    fn centre_cache_matches_a_fresh_render_after_every_change() {
        let pixels = image::ImageBuffer::from_fn(48, 32, |x, y| {
            let v = if (x / 5 + y / 5) % 2 == 0 { 0.08 } else { 0.3 };
            image::Rgba([v, v * 0.8, v * 0.6, 1.])
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba32F(pixels.clone())
            .to_rgba8()
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let source = Arc::new(super::super::input::decode_profiled_photo(bytes.get_ref()).unwrap());
        let image = Arc::new(DynamicImage::ImageRgba32F(pixels));
        let warm = AppState::default();
        for amount in [20., 80., -40., 0.] {
            let mut controls = Controls::default();
            controls.detail.texture = 10.; // spatial stage stays active at Centre=0
            controls.effects.centre = amount;
            let cached =
                spatial(&warm.v3, &source, image.clone(), &controls, 0, 0, None, 1.).unwrap();
            let fresh = spatial(
                &V3Caches::default(),
                &source,
                image.clone(),
                &controls,
                0,
                0,
                None,
                1.,
            )
            .unwrap();
            assert!(
                cached.as_rgba32f() == fresh.as_rgba32f(),
                "stale Centre at {amount}"
            );
        }
    }
}

#[cfg(test)]
mod adobe_import_tests {
    /// An edit from the Lightroom importer (src/utils/adobeImport.ts) goes
    /// through the same conversion every render uses. Run with
    /// ADOBE_CONVERTED=path/to/converted.json cargo test -- --ignored.
    #[test]
    #[ignore]
    fn converted_lightroom_edit_converts_for_rendering() {
        let Ok(path) = std::env::var("ADOBE_CONVERTED") else {
            return;
        };
        let edits: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let controls = super::controls_for(&edits, false).expect("renders");
        println!("tone {:?}", controls.tone);
        assert!(
            controls.tone.exposure > 0.0,
            "Lightroom's +2.5 EV arrives as brighter"
        );
        assert!(controls.tone.highlights < 0.0 && controls.tone.shadows > 0.0);
    }
}
