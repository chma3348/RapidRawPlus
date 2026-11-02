//! Compositing heal, clone and generative patches into the working space.
//!
//! A patch is pixels somebody else produced — a heal, a clone, a generated
//! fill — stored beside the edit as an 8-bit image and a mask. The previous
//! engine composites them onto whatever the decoded base happened to be,
//! which works there because nothing declares a colour space, so nothing can
//! disagree. This engine does declare one, which is why v3 refused patches
//! until now: nothing said what the stored pixels *were*.
//!
//! What they are depends on where they came from, and the patch records it:
//!
//! - `encoding: "gamma"` marks a patch lifted from float or RAW data, stored
//!   through a 1/2.4 curve so deep shadows survive eight bits. Undo the curve
//!   and the values are linear in the source's own primaries. A RAW's
//!   scene-linear values run well above one, so its patches are made from
//!   the picture divided by `raw_patch_scale` (a whole number of stops, from
//!   the picture itself) and multiplied back here.
//! - Anything else came from rendered, display-referred pixels, and is sRGB.
//!
//! Patches join the picture at the decoded-source stage, the same place the
//! previous engine puts them and before any geometry, so their coordinates
//! line up with the mask stored with them. When an input transform is
//! installed the source has already become scene data, so a display-referred
//! patch goes through that same transform — otherwise it would be the one
//! part of the frame that never had its rendering undone.

use crate::color_engine::config::{Primaries, ReferenceDomain, SourceColor, Transfer};
use crate::color_engine::cube::{CapturedInput, InputDomain};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, Rgba32FImage, imageops};
use rayon::prelude::*;
use serde_json::Value;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

/// The curve generative fills are stored through. Mirrors `LAMA_GAMMA`.
const STORED_GAMMA: f32 = 2.4;

pub fn visible(edits: &Value) -> Vec<&Value> {
    edits["aiPatches"]
        .as_array()
        .map(|patches| {
            patches
                .iter()
                .filter(|p| {
                    p["visible"].as_bool() != Some(false)
                        && p["patchData"]["color"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The scale a RAW's patches are made at: the picture divided by this fits
/// eight bits but for its brightest 0.1% (specular highlights). A power of
/// two, so the tool that makes a patch and the render that places it, both
/// measuring the same decoded picture, can never disagree.
pub fn raw_patch_scale(pixels: &Rgba32FImage) -> f32 {
    let raw = pixels.as_raw();
    let n = raw.len() / 4;
    let stride = (n / 400_000).max(1);
    let mut peaks: Vec<f32> = (0..n)
        .step_by(stride)
        .map(|i| raw[i * 4].max(raw[i * 4 + 1]).max(raw[i * 4 + 2]))
        .filter(|v| v.is_finite())
        .collect();
    if peaks.is_empty() {
        return 1.0;
    }
    let at = (peaks.len() - 1) * 999 / 1000;
    let high = *peaks.select_nth_unstable_by(at, f32::total_cmp).1;
    high.max(1.0).log2().ceil().exp2()
}

/// Composite every visible patch onto `base`, which is in `color`.
/// `raw_scale` is `raw_patch_scale` of a RAW's decoded picture (1 for
/// anything else): what its gamma-stored patches are multiplied back by.
pub fn composite(
    base: &mut Rgba32FImage,
    edits: &Value,
    color: &SourceColor,
    source_profile: Option<&[u8]>,
    input_transform: Option<(&CapturedInput, InputDomain)>,
    raw_scale: f32,
) -> Result<()> {
    let patches = visible(edits);
    if patches.is_empty() {
        return Ok(());
    }
    let (width, height) = base.dimensions();
    let conversion = conversion_key(color, source_profile, input_transform, raw_scale);
    for patch in patches {
        let opacity = (patch["opacity"].as_f64().unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0);
        let key = (layer_identity(patch), conversion, width, height);
        let cached = layers().lock().ok().and_then(|mut c| c.find(&key));
        let layer = match cached {
            Some(layer) => layer,
            None => {
                let layer = Arc::new(build_layer(
                    patch,
                    (width, height),
                    color,
                    source_profile,
                    input_transform,
                    raw_scale,
                )?);
                if let Ok(mut c) = layers().lock() {
                    c.put(key, layer.clone());
                }
                layer
            }
        };
        layer.blend(base, opacity);
    }
    Ok(())
}

/// What a patch contributes to the picture, for cache keys: every field but
/// the ones the composite never reads (the other attempts a fill keeps, and
/// the original a re-blend can go back to). Its pictures are hashed whole —
/// in parallel, and without serialising the patch, which with a few fills
/// cost tens of milliseconds on every render — so any change to them
/// changes this.
pub fn content_identity(patch: &Value) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hash_value(
        &mut hasher,
        patch,
        &["reconstructVariants", "replacementOriginal"],
    );
    digest64(&hasher)
}

/// What a patch's converted layer depends on: its pictures, its feather and
/// (for a patch drawn as shapes) its shapes — not its opacity or whether it
/// is shown, which are applied as it is blended.
fn layer_identity(patch: &Value) -> u64 {
    let data = &patch["patchData"];
    let mut hasher = blake3::Hasher::new();
    for part in [
        &patch["id"],
        &data["color"],
        &data["mask"],
        &data["encoding"],
        &patch["feather"],
    ] {
        hash_value(&mut hasher, part, &[]);
    }
    if data["mask"].as_str().is_none_or(str::is_empty) {
        for part in [&patch["subMasks"], &patch["invert"], &patch["name"]] {
            hash_value(&mut hasher, part, &[]);
        }
    }
    digest64(&hasher)
}

/// What converting a patch into the working space depends on.
fn conversion_key(
    color: &SourceColor,
    source_profile: Option<&[u8]>,
    input_transform: Option<(&CapturedInput, InputDomain)>,
    raw_scale: f32,
) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(format!("{color:?}").as_bytes());
    match source_profile {
        Some(icc) => hasher.update(&[1]).update(icc),
        None => hasher.update(&[0]),
    };
    if let Some((captured, domain)) = input_transform {
        hasher.update(format!("{}|{domain:?}", captured.digest()).as_bytes());
    }
    hasher.update(&raw_scale.to_bits().to_le_bytes());
    digest64(&hasher)
}

fn digest64(hasher: &blake3::Hasher) -> u64 {
    let digest = hasher.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(first)
}

/// Feed a JSON value to `hasher` unambiguously (each value tagged with its
/// kind and length), leaving out object keys in `skip`. Long strings — the
/// pictures — are hashed in parallel pieces.
fn hash_value(hasher: &mut blake3::Hasher, value: &Value, skip: &[&str]) {
    match value {
        Value::Null => {
            hasher.update(b"n");
        }
        Value::Bool(b) => {
            hasher.update(if *b { b"t" } else { b"f" });
        }
        Value::Number(n) => {
            let text = n.to_string();
            hasher.update(b"#");
            hasher.update(&(text.len() as u64).to_le_bytes());
            hasher.update(text.as_bytes());
        }
        Value::String(s) => {
            hasher.update(b"s");
            hasher.update(&(s.len() as u64).to_le_bytes());
            hash_bytes(hasher, s.as_bytes());
        }
        Value::Array(items) => {
            hasher.update(b"[");
            hasher.update(&(items.len() as u64).to_le_bytes());
            for item in items {
                hash_value(hasher, item, skip);
            }
        }
        Value::Object(map) => {
            hasher.update(b"{");
            for (key, item) in map {
                if skip.contains(&key.as_str()) {
                    continue;
                }
                hasher.update(&(key.len() as u64).to_le_bytes());
                hasher.update(key.as_bytes());
                hash_value(hasher, item, skip);
            }
            hasher.update(b"}");
        }
    }
}

fn hash_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    const PIECE: usize = 1 << 20;
    if bytes.len() <= PIECE {
        hasher.update(bytes);
        return;
    }
    let pieces: Vec<[u8; 32]> = bytes
        .par_chunks(PIECE)
        .map(|piece| *blake3::hash(piece).as_bytes())
        .collect();
    for piece in pieces {
        hasher.update(&piece);
    }
}

/// A patch made ready to blend: converted into the working space once, and
/// kept only where it can land — the rows and columns of the photograph
/// whose mask samples touch anything above zero. Everywhere else its
/// coverage is exactly zero, so leaving those pixels alone is exactly what
/// blending them would have done.
struct Layer {
    /// Rows and columns of the photograph that may be covered.
    rows: Range<usize>,
    columns: Range<usize>,
    colour: Plane<f32, 3>,
    mask: Plane<u8, 1>,
}

/// The part of a layer the blend reads, with each of those photograph rows'
/// and columns' sample positions moved into it.
struct Plane<T, const N: usize> {
    pixels: Vec<T>,
    width: usize,
    exact: bool,
    rows: Vec<(usize, usize, f32)>,
    columns: Vec<(usize, usize, f32)>,
}

impl<T: Copy, const N: usize> Plane<T, N> {
    /// Cut `pixels` (`width` wide, `N` values a pixel) down to what `at`
    /// reads for the photograph's `rows` and `columns`.
    fn cut(
        pixels: &[T],
        width: usize,
        at: &Sampler,
        rows: &Range<usize>,
        columns: &Range<usize>,
    ) -> Self {
        let rows_at = &at.rows[rows.clone()];
        let columns_at = &at.columns[columns.clone()];
        if rows_at.is_empty() || columns_at.is_empty() {
            return Self {
                pixels: Vec::new(),
                width: 0,
                exact: at.exact,
                rows: Vec::new(),
                columns: Vec::new(),
            };
        }
        let span = |taps: &[(usize, usize, f32)]| {
            let low = taps.iter().map(|t| t.0).min().unwrap_or(0);
            let high = taps.iter().map(|t| t.1).max().unwrap_or(0);
            (low, high)
        };
        let (y0, y1) = span(rows_at);
        let (x0, x1) = span(columns_at);
        let cut_width = x1 + 1 - x0;
        let mut cut = Vec::with_capacity(cut_width * (y1 + 1 - y0) * N);
        for y in y0..=y1 {
            let start = (y * width + x0) * N;
            cut.extend_from_slice(&pixels[start..start + cut_width * N]);
        }
        let shift = |taps: &[(usize, usize, f32)], by: usize| {
            taps.iter()
                .map(|&(low, high, f)| (low - by, high - by, f))
                .collect()
        };
        Self {
            pixels: cut,
            width: cut_width,
            exact: at.exact,
            rows: shift(rows_at, y0),
            columns: shift(columns_at, x0),
        }
    }

    fn bytes(&self) -> usize {
        self.pixels.len() * std::mem::size_of::<T>()
    }
}

impl Layer {
    fn bytes(&self) -> usize {
        self.colour.bytes() + self.mask.bytes()
    }

    fn blend(&self, base: &mut Rgba32FImage, opacity: f32) {
        if self.rows.is_empty() || self.columns.is_empty() {
            return;
        }
        let width = base.width() as usize;
        let (colour, cw) = (&self.colour.pixels, self.colour.width);
        let (mask, mw) = (&self.mask.pixels, self.mask.width);
        let first = self.rows.start;
        base.par_chunks_mut(width * 4)
            .skip(first)
            .take(self.rows.len())
            .enumerate()
            .for_each(|(i, row)| {
                let (cy0, cy1, cfy) = self.colour.rows[i];
                let (my0, my1, mfy) = self.mask.rows[i];
                for (j, x) in self.columns.clone().enumerate() {
                    let (mx0, mx1, mfx) = self.mask.columns[j];
                    let coverage = if self.mask.exact {
                        mask[my0 * mw + mx0] as f32
                    } else {
                        let m = |yy: usize, xx: usize| mask[yy * mw + xx] as f32;
                        let top = m(my0, mx0) + (m(my0, mx1) - m(my0, mx0)) * mfx;
                        let bottom = m(my1, mx0) + (m(my1, mx1) - m(my1, mx0)) * mfx;
                        top + (bottom - top) * mfy
                    };
                    if coverage < 0.5 {
                        continue;
                    }
                    let alpha = coverage / 255.0 * opacity;
                    let (cx0, cx1, cfx) = self.colour.columns[j];
                    for c in 0..3 {
                        let patched = if self.colour.exact {
                            colour[(cy0 * cw + cx0) * 3 + c]
                        } else {
                            let v = |yy: usize, xx: usize| colour[(yy * cw + xx) * 3 + c];
                            let top = v(cy0, cx0) + (v(cy0, cx1) - v(cy0, cx0)) * cfx;
                            let bottom = v(cy1, cx0) + (v(cy1, cx1) - v(cy1, cx0)) * cfx;
                            top + (bottom - top) * cfy
                        };
                        row[x * 4 + c] = patched * alpha + row[x * 4 + c] * (1.0 - alpha);
                    }
                }
            });
    }
}

/// Decode and convert one patch for a photograph of `photo` size.
fn build_layer(
    patch: &Value,
    photo: (u32, u32),
    color: &SourceColor,
    source_profile: Option<&[u8]>,
    input_transform: Option<(&CapturedInput, InputDomain)>,
    raw_scale: f32,
) -> Result<Layer> {
    let (width, height) = photo;
    let data = &patch["patchData"];
    let feather = patch["feather"].as_f64().unwrap_or(0.0) as f32;

    // A patch smaller than the photograph (a sky shown while it is being
    // chosen is about screen size) is converted at its own size and
    // sampled into the photograph as it is blended, not enlarged first:
    // enlarging and converting a whole 24-megapixel layer took over a
    // second on every change.
    let mut colour = decode_layer(data["color"].as_str().context("Patch has no colour data")?)?;
    let stored_gamma = data["encoding"].as_str() == Some("gamma");
    to_source_space(
        &mut colour,
        color,
        source_profile,
        stored_gamma,
        input_transform,
    )?;
    if stored_gamma && raw_scale != 1.0 {
        colour.par_iter_mut().for_each(|v| *v *= raw_scale);
    }

    let mut mask = mask_for(patch, data, width, height)?;
    if feather > 0.0 {
        // Feathering is measured in the photograph's pixels.
        if mask.dimensions() != (width, height) {
            mask = imageops::resize(&mask, width, height, imageops::FilterType::Lanczos3);
        }
        mask = crate::ai_processing::feather_mask_inward(&mask, feather);
    }

    let colour_at = Sampler::new(colour.dimensions(), photo);
    let mask_at = Sampler::new(mask.dimensions(), photo);
    // The photograph's rows (columns) with a mask sample on a mask row
    // (column) holding anything above zero. Outside them every sample is
    // zero, and so is the coverage.
    let mw = mask.width() as usize;
    let raw = mask.as_raw();
    let lit_rows: Vec<bool> = if mw == 0 {
        Vec::new()
    } else {
        raw.par_chunks(mw)
            .map(|r| r.iter().any(|&v| v > 0))
            .collect()
    };
    let mut lit_columns = vec![false; mw];
    if mw > 0 {
        for (r, _) in raw.chunks(mw).zip(&lit_rows).filter(|(_, lit)| **lit) {
            for (lit, &v) in lit_columns.iter_mut().zip(r) {
                *lit |= v > 0;
            }
        }
    }
    let reach = |taps: &[(usize, usize, f32)], lit: &[bool]| {
        let touches =
            |t: &(usize, usize, f32)| lit.get(t.0) == Some(&true) || lit.get(t.1) == Some(&true);
        match (
            taps.iter().position(touches),
            taps.iter().rposition(touches),
        ) {
            (Some(a), Some(b)) => a..b + 1,
            _ => 0..0,
        }
    };
    let (mut rows, mut columns) = (
        reach(&mask_at.rows, &lit_rows),
        reach(&mask_at.columns, &lit_columns),
    );
    if rows.is_empty() || columns.is_empty() {
        (rows, columns) = (0..0, 0..0);
    }
    Ok(Layer {
        colour: Plane::cut(
            colour.as_raw(),
            colour.width() as usize,
            &colour_at,
            &rows,
            &columns,
        ),
        mask: Plane::cut(raw, mw, &mask_at, &rows, &columns),
        rows,
        columns,
    })
}

/// Converted layers, most recently used last, within a memory budget.
/// Without them a preview-cache miss (each preview size, each geometry
/// change, each memory trim) decoded and converted every patch's full-frame
/// picture again: a few hundred milliseconds a patch.
struct Layers {
    items: Vec<(LayerKey, Arc<Layer>)>,
}

/// The patch, how it is converted, and the photograph's size.
type LayerKey = (u64, u64, u32, u32);

const LAYER_BUDGET: usize = 512 << 20;

impl Layers {
    fn find(&mut self, key: &LayerKey) -> Option<Arc<Layer>> {
        let at = self.items.iter().position(|(k, _)| k == key)?;
        let item = self.items.remove(at);
        let layer = item.1.clone();
        self.items.push(item);
        Some(layer)
    }

    fn put(&mut self, key: LayerKey, layer: Arc<Layer>) {
        self.items.retain(|(k, _)| *k != key);
        if layer.bytes() > LAYER_BUDGET {
            return;
        }
        self.items.push((key, layer));
        while self.bytes() > LAYER_BUDGET {
            self.items.remove(0);
        }
    }

    fn bytes(&self) -> usize {
        self.items.iter().map(|(_, l)| l.bytes()).sum()
    }
}

fn layers() -> &'static Mutex<Layers> {
    static LAYERS: OnceLock<Mutex<Layers>> = OnceLock::new();
    LAYERS.get_or_init(|| Mutex::new(Layers { items: Vec::new() }))
}

/// Let go of every converted layer (memory pressure, or another photo), and
/// say about how many bytes that was.
pub fn clear_layers() -> usize {
    layers().lock().map_or(0, |mut c| {
        let freed = c.bytes();
        c.items.clear();
        freed
    })
}

/// Where each row and column of the photograph falls in a layer of another
/// size, for bilinear sampling (pixel centres aligned): the two neighbours
/// and the weight of the second. A layer of the photograph's size is read
/// exactly.
struct Sampler {
    exact: bool,
    rows: Vec<(usize, usize, f32)>,
    columns: Vec<(usize, usize, f32)>,
}

impl Sampler {
    fn new(layer: (u32, u32), photo: (u32, u32)) -> Self {
        let axis = |from: u32, to: u32| -> Vec<(usize, usize, f32)> {
            let last = from.max(1) as usize - 1;
            let step = from as f32 / to.max(1) as f32;
            (0..to as usize)
                .map(|i| {
                    let at = ((i as f32 + 0.5) * step - 0.5).clamp(0.0, last as f32);
                    let low = at.floor() as usize;
                    (low, (low + 1).min(last), at - low as f32)
                })
                .collect()
        };
        Self {
            exact: layer == photo,
            rows: axis(layer.1, photo.1),
            columns: axis(layer.0, photo.0),
        }
    }
}

/// A patch's colour, at its own size.
fn decode_layer(encoded: &str) -> Result<image::Rgb32FImage> {
    let bytes = general_purpose::STANDARD.decode(encoded)?;
    let image = image::load_from_memory(&bytes)?.to_rgb8();
    Ok(DynamicImage::ImageRgb8(image).to_rgb32f())
}

fn mask_for(patch: &Value, data: &Value, width: u32, height: u32) -> Result<image::GrayImage> {
    if let Some(encoded) = data["mask"].as_str().filter(|s| !s.is_empty()) {
        // At its own size: `composite` samples it into the photograph.
        let bytes = general_purpose::STANDARD.decode(encoded)?;
        return Ok(image::load_from_memory(&bytes)?.to_luma8());
    }
    // Older patches carry their shapes instead of a rendered mask.
    let info: crate::image_loader::PatchMaskInfo =
        serde_json::from_value(patch.clone()).context("Patch has neither a mask nor shapes")?;
    let definition = crate::mask_generation::MaskDefinition {
        id: info.id,
        name: info.name,
        visible: true,
        invert: info.invert,
        opacity: 100.0,
        grow: 0.0,
        feather: 0.0,
        adjustments: Value::Null,
        sub_masks: info.sub_masks,
    };
    crate::mask_generation::generate_mask_bitmap(&definition, width, height, 1.0, (0.0, 0.0), None)
        .context("Could not build a patch mask from its shapes")
}

/// Bring stored patch pixels into the space the base image is in.
fn to_source_space(
    colour: &mut image::Rgb32FImage,
    color: &SourceColor,
    source_profile: Option<&[u8]>,
    stored_gamma: bool,
    input_transform: Option<(&CapturedInput, InputDomain)>,
) -> Result<()> {
    if stored_gamma {
        // Linear already, once the storage curve is undone, in the primaries
        // the source was decoded to.
        colour
            .par_iter_mut()
            .for_each(|v| *v = v.clamp(0.0, 1.0).powf(STORED_GAMMA));
        return Ok(());
    }
    // Display pixels, in the file's own code values: the previous engine's
    // tools never colour-manage a photograph, so a heal or a fill is made in
    // whatever space the file is in. Decode them exactly as the photograph
    // was decoded — through its profile when it has one — or a patch on a
    // Display P3 file lands duller than the pixels around it.
    let mut linear = image::ImageBuffer::from_fn(colour.width(), colour.height(), |x, y| {
        let p = colour.get_pixel(x, y);
        image::Rgba([p[0], p[1], p[2], 1.0])
    });
    match source_profile {
        Some(icc) => {
            let profile = moxcms::ColorProfile::new_from_slice(icc).map_err(|e| {
                anyhow::anyhow!("The photograph's colour profile is unreadable: {e}")
            })?;
            linear = crate::color_engine::input::convert_rgb_profile(&linear, &profile)?;
        }
        None => {
            for p in linear.pixels_mut() {
                for c in 0..3 {
                    p[c] = srgb_decode(p[c]);
                }
            }
        }
    }
    match (color.reference, input_transform) {
        // The source has been through the input transform, so the patch must
        // be too, or it keeps a rendering the rest of the frame has had
        // removed. `apply_input_transform` takes linear values, as here.
        (ReferenceDomain::Scene, Some((captured, domain))) => {
            captured.apply_as(domain, &mut linear)
        }
        _ => {
            debug_assert_eq!(
                color.transfer,
                Transfer::Linear,
                "the input adapter is expected to hand back linear pixels"
            );
            if color.primaries != Primaries::Srgb {
                let matrix =
                    crate::color_engine::spaces::conversion(Primaries::Srgb, color.primaries);
                for p in linear.pixels_mut() {
                    let v = matrix * glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
                    p[0] = v.x as f32;
                    p[1] = v.y as f32;
                    p[2] = v.z as f32;
                }
            }
        }
    }
    for (out, src) in colour.pixels_mut().zip(linear.pixels()) {
        out.0 = [src[0], src[1], src[2]];
    }
    Ok(())
}

fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch_json(colour: image::RgbImage, mask: image::GrayImage, encoding: &str) -> Value {
        let encode = |image: DynamicImage| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            general_purpose::STANDARD.encode(bytes.into_inner())
        };
        serde_json::json!({"aiPatches": [{
            "visible": true,
            "opacity": 100.0,
            "feather": 0.0,
            "patchData": {
                "color": encode(DynamicImage::ImageRgb8(colour)),
                "mask": encode(DynamicImage::ImageLuma8(mask)),
                "encoding": encoding,
            }
        }]})
    }

    fn display_source() -> SourceColor {
        SourceColor {
            primaries: Primaries::Srgb,
            transfer: Transfer::Linear,
            reference: ReferenceDomain::Display,
        }
    }

    /// A patch stored as display pixels must arrive as the same colour the
    /// source would have been decoded to — not as its raw stored numbers.
    #[test]
    fn a_display_patch_is_decoded_into_the_working_space() {
        let colour = image::ImageBuffer::from_pixel(4, 4, image::Rgb([128u8, 64, 200]));
        let mask = image::ImageBuffer::from_pixel(4, 4, image::Luma([255u8]));
        let mut base = image::ImageBuffer::from_pixel(4, 4, image::Rgba([0.0f32, 0.0, 0.0, 1.0]));
        composite(
            &mut base,
            &patch_json(colour, mask, "srgb"),
            &display_source(),
            None,
            None,
            1.0,
        )
        .unwrap();
        let got = base.get_pixel(0, 0);
        for (c, stored) in [128.0f32, 64.0, 200.0].iter().enumerate() {
            let want = srgb_decode(stored / 255.0);
            assert!(
                (got[c] - want).abs() < 1e-4,
                "channel {c}: {got:?} wanted {want}"
            );
        }
    }

    #[test]
    fn a_gamma_patch_undoes_its_storage_curve() {
        let colour = image::ImageBuffer::from_pixel(2, 2, image::Rgb([128u8, 128, 128]));
        let mask = image::ImageBuffer::from_pixel(2, 2, image::Luma([255u8]));
        let mut base = image::ImageBuffer::from_pixel(2, 2, image::Rgba([0.0f32, 0.0, 0.0, 1.0]));
        composite(
            &mut base,
            &patch_json(colour, mask, "gamma"),
            &display_source(),
            None,
            None,
            1.0,
        )
        .unwrap();
        let want = (128.0f32 / 255.0).powf(STORED_GAMMA);
        assert!((base.get_pixel(0, 0)[0] - want).abs() < 1e-4);
    }

    #[test]
    fn a_raw_patch_is_multiplied_back_by_its_scale() {
        // Made from the picture divided by 4, stored through the curve: the
        // render multiplies it back.
        let colour = image::ImageBuffer::from_pixel(2, 2, image::Rgb([128u8, 128, 128]));
        let mask = image::ImageBuffer::from_pixel(2, 2, image::Luma([255u8]));
        let mut base = image::ImageBuffer::from_pixel(2, 2, image::Rgba([0.0f32, 0.0, 0.0, 1.0]));
        composite(
            &mut base,
            &patch_json(colour, mask, "gamma"),
            &display_source(),
            None,
            None,
            4.0,
        )
        .unwrap();
        let want = 4.0 * (128.0f32 / 255.0).powf(STORED_GAMMA);
        assert!((base.get_pixel(0, 0)[0] - want).abs() < 1e-4);
    }

    #[test]
    fn the_raw_patch_scale_is_whole_stops_and_fits_the_highlights() {
        let mut pixels =
            image::ImageBuffer::from_pixel(100, 100, image::Rgba([0.5f32, 2.5, 0.2, 1.0]));
        assert_eq!(raw_patch_scale(&pixels), 4.0);
        // A few specular pixels do not raise it.
        for x in 0..5 {
            pixels.put_pixel(x, 0, image::Rgba([40.0, 40.0, 40.0, 1.0]));
        }
        assert_eq!(raw_patch_scale(&pixels), 4.0);
        // Never below one.
        let dim = image::ImageBuffer::from_pixel(10, 10, image::Rgba([0.1f32, 0.1, 0.1, 1.0]));
        assert_eq!(raw_patch_scale(&dim), 1.0);
    }

    /// The mask is coverage, and zero coverage must leave the base exactly.
    #[test]
    fn the_mask_decides_where_a_patch_lands() {
        let colour = image::ImageBuffer::from_pixel(4, 1, image::Rgb([255u8, 255, 255]));
        let mask =
            image::ImageBuffer::from_fn(4, 1, |x, _| image::Luma([if x < 2 { 255u8 } else { 0 }]));
        let mut base =
            image::ImageBuffer::from_pixel(4, 1, image::Rgba([0.25f32, 0.25, 0.25, 1.0]));
        composite(
            &mut base,
            &patch_json(colour, mask, "srgb"),
            &display_source(),
            None,
            None,
            1.0,
        )
        .unwrap();
        assert!(
            base.get_pixel(0, 0)[0] > 0.9,
            "covered pixel was not patched"
        );
        assert_eq!(
            base.get_pixel(3, 0)[0],
            0.25,
            "uncovered pixel must be untouched"
        );
    }

    #[test]
    fn a_hidden_patch_does_nothing() {
        let mut edits = patch_json(
            image::ImageBuffer::from_pixel(2, 2, image::Rgb([255u8, 0, 0])),
            image::ImageBuffer::from_pixel(2, 2, image::Luma([255u8])),
            "srgb",
        );
        edits["aiPatches"][0]["visible"] = Value::Bool(false);
        let mut base = image::ImageBuffer::from_pixel(2, 2, image::Rgba([0.1f32, 0.1, 0.1, 1.0]));
        let before = base.clone();
        composite(&mut base, &edits, &display_source(), None, None, 1.0).unwrap();
        assert_eq!(base, before);
    }

    /// The composite as it was before layers were cut down and cached: every
    /// patch decoded, converted and blended over the whole photograph.
    fn reference(
        base: &mut Rgba32FImage,
        edits: &Value,
        color: &SourceColor,
        raw_scale: f32,
    ) -> Result<()> {
        let (width, height) = base.dimensions();
        for patch in visible(edits) {
            let data = &patch["patchData"];
            let feather = patch["feather"].as_f64().unwrap_or(0.0) as f32;
            let opacity =
                (patch["opacity"].as_f64().unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0);
            let mut colour = decode_layer(data["color"].as_str().unwrap())?;
            let stored_gamma = data["encoding"].as_str() == Some("gamma");
            to_source_space(&mut colour, color, None, stored_gamma, None)?;
            if stored_gamma && raw_scale != 1.0 {
                colour.par_iter_mut().for_each(|v| *v *= raw_scale);
            }
            let mut mask = mask_for(patch, data, width, height)?;
            if feather > 0.0 {
                if mask.dimensions() != (width, height) {
                    mask = imageops::resize(&mask, width, height, imageops::FilterType::Lanczos3);
                }
                mask = crate::ai_processing::feather_mask_inward(&mask, feather);
            }
            let colour_at = Sampler::new(colour.dimensions(), (width, height));
            let mask_at = Sampler::new(mask.dimensions(), (width, height));
            let (cw, mw) = (colour.width() as usize, mask.width() as usize);
            let (colour, mask) = (colour.as_raw(), mask.as_raw());
            for (y, row) in base.chunks_mut((width * 4) as usize).enumerate() {
                let (cy0, cy1, cfy) = colour_at.rows[y];
                let (my0, my1, mfy) = mask_at.rows[y];
                for x in 0..width as usize {
                    let (mx0, mx1, mfx) = mask_at.columns[x];
                    let coverage = if mask_at.exact {
                        mask[y * mw + x] as f32
                    } else {
                        let m = |yy: usize, xx: usize| mask[yy * mw + xx] as f32;
                        let top = m(my0, mx0) + (m(my0, mx1) - m(my0, mx0)) * mfx;
                        let bottom = m(my1, mx0) + (m(my1, mx1) - m(my1, mx0)) * mfx;
                        top + (bottom - top) * mfy
                    };
                    if coverage < 0.5 {
                        continue;
                    }
                    let alpha = coverage / 255.0 * opacity;
                    let (cx0, cx1, cfx) = colour_at.columns[x];
                    for c in 0..3 {
                        let patched = if colour_at.exact {
                            colour[(y * cw + x) * 3 + c]
                        } else {
                            let v = |yy: usize, xx: usize| colour[(yy * cw + xx) * 3 + c];
                            let top = v(cy0, cx0) + (v(cy0, cx1) - v(cy0, cx0)) * cfx;
                            let bottom = v(cy1, cx0) + (v(cy1, cx1) - v(cy1, cx0)) * cfx;
                            top + (bottom - top) * cfy
                        };
                        row[x * 4 + c] = patched * alpha + row[x * 4 + c] * (1.0 - alpha);
                    }
                }
            }
        }
        Ok(())
    }

    /// A fill-like patch: textured colour inside a soft-edged blob, zero
    /// outside it (as stored), at `size`, its blob centred on `centre`.
    fn blob_patch(size: (u32, u32), centre: (f32, f32), radius: f32, seed: u32) -> Value {
        let colour = image::ImageBuffer::from_fn(size.0, size.1, |x, y| {
            let d = ((x as f32 - centre.0).powi(2) + (y as f32 - centre.1).powi(2)).sqrt();
            if d > radius + 2.0 {
                return image::Rgb([0u8, 0, 0]);
            }
            let v = |k: u32| ((x * 31 + y * 17 + seed * 7 + k * 53) % 251) as u8;
            image::Rgb([v(0), v(1), v(2)])
        });
        let mask = image::ImageBuffer::from_fn(size.0, size.1, |x, y| {
            let d = ((x as f32 - centre.0).powi(2) + (y as f32 - centre.1).powi(2)).sqrt();
            image::Luma([((radius + 2.0 - d) * 64.0).clamp(0.0, 255.0) as u8])
        });
        let mut edits = patch_json(
            colour,
            mask,
            if seed.is_multiple_of(2) {
                "gamma"
            } else {
                "srgb"
            },
        );
        edits["aiPatches"][0]["id"] = json!(format!("blob-{seed}"));
        edits
    }

    fn textured_base(width: u32, height: u32) -> Rgba32FImage {
        image::ImageBuffer::from_fn(width, height, |x, y| {
            let v = ((x * 13 + y * 7) % 97) as f32 / 97.0;
            image::Rgba([v, 1.0 - v, v * 0.5, 1.0])
        })
    }

    /// Cutting a layer down to where it can land, and caching it, must give
    /// exactly the picture blending the whole layer gave — at the
    /// photograph's size and not, feathered or not, near the edges too.
    #[test]
    fn a_cut_down_layer_blends_exactly_as_the_whole_one() {
        let photo = (97u32, 61u32);
        let cases = [
            // (stored size, blob centre, radius, feather, opacity)
            ((97u32, 61u32), (40.0f32, 30.0f32), 9.0f32, 0.0f64, 100.0f64),
            ((97, 61), (3.0, 2.0), 6.0, 40.0, 80.0),
            ((97, 61), (94.0, 59.0), 7.0, 0.0, 100.0),
            // Stored smaller and larger than the photograph: sampled.
            ((40, 25), (20.0, 12.0), 5.0, 0.0, 100.0),
            ((40, 25), (38.0, 1.0), 4.0, 70.0, 100.0),
            ((300, 190), (150.0, 90.0), 30.0, 0.0, 55.0),
            ((300, 190), (10.0, 180.0), 25.0, 25.0, 100.0),
        ];
        for (seed, (size, centre, radius, feather, opacity)) in cases.into_iter().enumerate() {
            let mut edits = blob_patch(size, centre, radius, seed as u32);
            edits["aiPatches"][0]["feather"] = json!(feather);
            edits["aiPatches"][0]["opacity"] = json!(opacity);
            for raw_scale in [1.0f32, 4.0] {
                let mut want = textured_base(photo.0, photo.1);
                reference(&mut want, &edits, &display_source(), raw_scale).unwrap();
                // Twice: built, then from the cache.
                for _ in 0..2 {
                    let mut got = textured_base(photo.0, photo.1);
                    composite(&mut got, &edits, &display_source(), None, None, raw_scale).unwrap();
                    assert!(
                        got.as_raw()
                            .iter()
                            .zip(want.as_raw())
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                        "case {seed} (raw scale {raw_scale}) differs from the whole-layer blend"
                    );
                }
            }
        }
    }

    #[test]
    fn an_empty_mask_leaves_the_photograph_alone() {
        let colour = image::ImageBuffer::from_pixel(8, 8, image::Rgb([200u8, 10, 10]));
        let mask = image::ImageBuffer::from_pixel(8, 8, image::Luma([0u8]));
        let mut edits = patch_json(colour, mask, "srgb");
        edits["aiPatches"][0]["id"] = json!("empty-mask");
        let mut base = textured_base(8, 8);
        let before = base.clone();
        composite(&mut base, &edits, &display_source(), None, None, 1.0).unwrap();
        assert_eq!(base, before);
    }

    /// The key a prepared picture is cached under changes with any change
    /// to a patch's pictures, however small, or its blend; a layer's key
    /// ignores what is applied as it blends; neither looks at the other
    /// attempts a fill keeps.
    #[test]
    fn patch_keys_follow_what_the_render_reads() {
        let base = blob_patch((64, 48), (30.0, 20.0), 8.0, 1)["aiPatches"][0].clone();
        let colour = base["patchData"]["color"].as_str().unwrap().to_string();
        let changed = |f: &dyn Fn(&mut Value)| {
            let mut p = base.clone();
            f(&mut p);
            p
        };
        // One character of a picture, near its middle: a long string is
        // hashed in pieces, and every piece counts.
        let long = "A".repeat(3 << 20);
        let with_long = changed(&|p| p["patchData"]["color"] = json!(long));
        let mut edited = long.clone().into_bytes();
        edited[(3 << 20) / 2] = b'B';
        let with_edited = changed(&|p| {
            p["patchData"]["color"] = json!(String::from_utf8(edited.clone()).unwrap())
        });
        assert_ne!(content_identity(&with_long), content_identity(&with_edited));
        assert_ne!(layer_identity(&with_long), layer_identity(&with_edited));

        let mut flipped = colour.clone().into_bytes();
        let at = flipped.len() / 2;
        flipped[at] = if flipped[at] == b'A' { b'B' } else { b'A' };
        let recoloured = changed(&|p| {
            p["patchData"]["color"] = json!(String::from_utf8(flipped.clone()).unwrap())
        });
        let remasked = changed(&|p| p["patchData"]["mask"] = json!("other"));
        let faded = changed(&|p| p["opacity"] = json!(50.0));
        let feathered = changed(&|p| p["feather"] = json!(10.0));
        let regamma = changed(&|p| p["patchData"]["encoding"] = json!("gamma"));
        let other_attempts = changed(&|p| {
            p["patchData"]["reconstructVariants"] = json!([{"id": "attempt-1", "color": "x"}]);
            p["patchData"]["replacementOriginal"] = json!({"color": "y"});
        });

        let key = content_identity(&base);
        for other in [&recoloured, &remasked, &faded, &feathered, &regamma] {
            assert_ne!(key, content_identity(other));
        }
        assert_eq!(key, content_identity(&other_attempts));
        assert_eq!(key, content_identity(&base.clone()));

        let layer = layer_identity(&base);
        for other in [&recoloured, &remasked, &feathered, &regamma] {
            assert_ne!(layer, layer_identity(other));
        }
        assert_eq!(layer, layer_identity(&faded));
        assert_eq!(layer, layer_identity(&other_attempts));
    }

    /// A patch whose pictures change must not be served its old layer.
    #[test]
    fn a_changed_patch_is_not_served_its_old_layer() {
        let make = |value: u8| {
            let colour = image::ImageBuffer::from_pixel(6, 6, image::Rgb([value, value, value]));
            let mask = image::ImageBuffer::from_pixel(6, 6, image::Luma([255u8]));
            let mut edits = patch_json(colour, mask, "gamma");
            edits["aiPatches"][0]["id"] = json!("same-id");
            edits
        };
        for value in [40u8, 200] {
            let mut base =
                image::ImageBuffer::from_pixel(6, 6, image::Rgba([0.0f32, 0.0, 0.0, 1.0]));
            composite(&mut base, &make(value), &display_source(), None, None, 1.0).unwrap();
            let want = (value as f32 / 255.0).powf(STORED_GAMMA);
            assert!((base.get_pixel(3, 3)[0] - want).abs() < 1e-5);
        }
    }

    /// Every stored fill in a real sidecar composites exactly as before.
    /// Needs the test photographs: `cargo test -- --ignored real_fills`.
    #[test]
    #[ignore]
    fn real_fills_composite_exactly_as_before() {
        for name in [
            "DSC08212.JPG",
            "DSC08265.JPG",
            "DSC08226.JPG",
            "DSC08203.JPG",
        ] {
            let path = format!("/Users/chrismanning/Desktop/Test Photos/{name}.dri");
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let side: Value = serde_json::from_str(&text).unwrap();
            let edits = &side["adjustments"];
            let Some(first) = visible(edits).first().map(|p| (*p).clone()) else {
                continue;
            };
            let size = decode_layer(first["patchData"]["color"].as_str().unwrap())
                .unwrap()
                .dimensions();
            for feather in [None, Some(30.0)] {
                let mut edits = edits.clone();
                if let Some(f) = feather {
                    for p in edits["aiPatches"].as_array_mut().unwrap() {
                        p["feather"] = json!(f);
                    }
                }
                let mut want = textured_base(size.0, size.1);
                reference(&mut want, &edits, &display_source(), 1.0).unwrap();
                let mut got = textured_base(size.0, size.1);
                composite(&mut got, &edits, &display_source(), None, None, 1.0).unwrap();
                let same = got
                    .as_raw()
                    .iter()
                    .zip(want.as_raw())
                    .all(|(a, b)| a.to_bits() == b.to_bits());
                assert!(same, "{name} (feather {feather:?}) differs");
                eprintln!(
                    "{name} feather {feather:?}: identical, {} patches",
                    visible(&edits).len()
                );
            }
        }
    }
}
