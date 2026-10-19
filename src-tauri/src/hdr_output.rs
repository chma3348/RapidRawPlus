//! What an HDR merge becomes on disk.
//!
//! - **A RAW bracket** becomes a floating-point DNG (`hdr_dng`): the merged
//!   light, unclipped, opened like any RAW — with the RAW look, and every
//!   control free to reach into the extra range.
//! - **A bracket of camera JPEGs (or other finished pictures)** becomes a
//!   16-bit TIFF in the same rendering as the frames: their look is already
//!   baked in, so a DNG would have the RAW look laid over it twice. The
//!   range the darker frames add above white is folded under it with a soft
//!   shoulder instead of being clipped, so the Highlights slider can bring
//!   it back.
//!
//! Either way the merge is placed at the middle frame's exposure: its
//! midtones open as that frame's did, not scaled down to fit the brightest
//! light (which made merges dark and flat).
//!
//! The merged file is written to a temporary place as soon as it is made:
//! the merge window's preview is a render of that very file, through the
//! editor's own pipeline, and saving moves it beside the photos.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use image::Rgb32FImage;
use rayon::prelude::*;

/// A merge (HDR or panorama) waiting to be saved.
pub struct HdrResult {
    /// The merged file, in a temporary folder.
    pub file: PathBuf,
    /// "dng", "jxl" or "tif".
    pub extension: &'static str,
    /// The frames it was made from, real paths.
    pub sources: Vec<PathBuf>,
    /// "HDR" or "Pano": the saved file's suffix and its stack's label.
    pub kind: &'static str,
}

/// What a merge's pixels are.
pub enum Merged {
    /// Linear light from RAW frames, in the reference frame's units: a DNG.
    Raw(crate::hdr_dng::DngCamera),
    /// A finished picture, sRGB-encoded 0..1: a JPEG XL or a TIFF.
    Rendered,
}

/// Merges are kept compact (lossy JPEG XL, as Lightroom 13 keeps its
/// merges) unless the setting asks for lossless.
pub fn compact_from_settings(settings: &crate::app_settings::AppSettings) -> bool {
    settings.merge_compression.as_deref() != Some("lossless")
}

/// The JPEG XL distance for compact finished-picture merges: the same as
/// the DNG's, chosen on real photos pushed three stops and zoomed in.
const RENDERED_DISTANCE: f32 = 0.3;
/// Its effort: 3 against the default 7 measured within 0.04 levels on
/// real photos, at half the memory (a whole picture is encoded at once:
/// 7 needed 19 GB for a 131-megapixel panorama, 3 needs 8).
const RENDERED_EFFORT: u8 = 3;
/// Beyond this a finished-picture merge is saved lossless (a compressed
/// TIFF) even when compact is chosen: one JPEG XL of it would need more
/// memory than a Mac can be expected to spare. (A DNG's JPEG XL is tiled,
/// so RAW merges have no such limit.)
const COMPACT_MAX_PIXELS: u64 = 150_000_000;

/// Write a merge to a temporary file, render its preview through the
/// editor's pipeline (at 1600 px, for the merge window), and for a DNG
/// embed that preview in the file (Finder, Quick Look and Nextcloud's web
/// view show it). Returns the merge and the preview as a JPEG data URL.
pub fn finish(
    context: &crate::image_processing::GpuContext,
    state: &crate::AppState,
    pixels: &Rgb32FImage,
    merged: &Merged,
    compact: bool,
    sources: Vec<PathBuf>,
    kind: &'static str,
) -> Result<(HdrResult, String)> {
    let dng_compression = if compact {
        crate::hdr_dng::COMPACT
    } else {
        crate::hdr_dng::Compression::Lossless
    };
    let pixel_count = pixels.width() as u64 * pixels.height() as u64;
    let compact_rendered = compact && pixel_count <= COMPACT_MAX_PIXELS;
    let extension = match merged {
        Merged::Raw(_) => "dng",
        Merged::Rendered if compact_rendered => "jxl",
        Merged::Rendered => "tif",
    };
    let file = temporary_path(extension)?;
    let result = HdrResult {
        file: file.clone(),
        extension,
        sources,
        kind,
    };
    match merged {
        Merged::Raw(camera) => {
            crate::hdr_dng::write_float_dng(&file, pixels, camera, dng_compression, None)?
        }
        Merged::Rendered => write_rendered(&file, pixels, compact_rendered)?,
    }
    let preview = crate::color_engine::application::render_aside(
        context,
        state,
        &file.to_string_lossy(),
        &serde_json::Value::Null,
        Some(1600),
    )
    .context("Previewing the merge")?
    .preview_rgba8();
    let preview = image::DynamicImage::ImageRgba8(preview).to_rgb8();
    if let Merged::Raw(camera) = merged {
        let embedded = image::imageops::thumbnail(
            &preview,
            preview.width().min(1024),
            ((preview.height() as u64 * preview.width().min(1024) as u64)
                / preview.width().max(1) as u64) as u32,
        );
        crate::hdr_dng::write_float_dng(&file, pixels, camera, dng_compression, Some(&embedded))?;
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90).encode_image(&preview)?;
    use base64::Engine as _;
    let url = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    );
    Ok((result, url))
}

/// A finished-picture merge (sRGB-encoded 0..1): JPEG XL at 16 bits when
/// compact, otherwise a 16-bit TIFF, deflate-compressed with the
/// horizontal predictor (an uncompressed float TIFF made a 144-megapixel
/// panorama 1.65 GB).
pub fn write_rendered(path: &Path, encoded: &Rgb32FImage, compact: bool) -> Result<()> {
    let (w, h) = encoded.dimensions();
    let data: Vec<u16> = encoded
        .as_raw()
        .par_iter()
        .map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
        .collect();
    if compact {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let jxl = jxl_encoder::LossyConfig::new(RENDERED_DISTANCE)
            .with_effort(RENDERED_EFFORT)
            .encode(&bytes, w, h, jxl_encoder::PixelLayout::Rgb16)
            .map_err(|e| anyhow::anyhow!("JPEG XL: {e}"))?;
        std::fs::write(path, jxl)?;
        return Ok(());
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    tiff::encoder::TiffEncoder::new(file)?
        .with_compression(tiff::encoder::Compression::Deflate(
            tiff::encoder::DeflateLevel::Balanced,
        ))
        .with_predictor(tiff::tags::Predictor::Horizontal)
        .write_image::<tiff::encoder::colortype::RGB16>(w, h, &data)
        .context("Writing the merged TIFF")?;
    Ok(())
}

/// Linear display light to sRGB-encoded values, for a finished-picture
/// merge.
pub fn encode_srgb(img: &mut Rgb32FImage) {
    img.par_iter_mut().for_each(|v| {
        let x = v.clamp(0.0, 1.0);
        *v = if x <= 0.0031308 {
            x * 12.92
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        };
    });
}

impl Drop for HdrResult {
    /// A merge closed without saving leaves nothing behind.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

/// A frame for merging, as the colour engine decodes it: linear light,
/// sRGB primaries. RAWs are scene-referred (1.0 where the sensor clipped);
/// finished pictures are display-referred (their white at 1.0).
pub fn load_frame(path: &str, bytes: &[u8]) -> Result<Rgb32FImage> {
    let frame = if crate::formats::is_raw_file(path) {
        crate::color_engine::raw::decode_raw(bytes, false, || Ok(()))?
    } else {
        crate::color_engine::input::decode_profiled_photo(bytes)?
    };
    let (w, h) = frame.pixels.dimensions();
    let rgb: Vec<f32> = frame
        .pixels
        .as_raw()
        .par_chunks(4)
        .flat_map_iter(|p| [p[0], p[1], p[2]])
        .collect();
    Rgb32FImage::from_raw(w, h, rgb).context("Decoded frame has the wrong size")
}

/// The exposure the merge is shown at: the middle of the bracket's (the
/// reference the frames were aligned to).
pub fn reference_exposure(exposures: &[f32]) -> f32 {
    let mut sorted = exposures.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted[sorted.len() / 2]
}

/// Radiance (value / exposure) back into the reference frame's units.
pub fn place_at(merged: &mut Rgb32FImage, exposure: f32) {
    merged.par_iter_mut().for_each(|v| *v *= exposure);
}

/// Fold a finished-picture merge's light above white under it: unchanged
/// up to 0.8 (on the brightest channel, so colours keep their hue), then a
/// smooth shoulder that reaches white at the merge's brightest light,
/// easing toward neutral at the very top as a camera's rendering does.
pub fn fold_highlights(merged: &mut Rgb32FImage) {
    const KNEE: f32 = 0.8;
    let mut maxes: Vec<f32> = merged.pixels().map(|p| p[0].max(p[1]).max(p[2])).collect();
    let at = ((maxes.len() as f64 * 0.9999) as usize).min(maxes.len().saturating_sub(1));
    let (_, &mut peak, _) = maxes.select_nth_unstable_by(at, f32::total_cmp);
    if peak > 1.0 {
        // Rational shoulder: slope 1 at the knee, 1.0 at the peak.
        let a = (peak - KNEE) / (1.0 - KNEE);
        merged.par_chunks_mut(3).for_each(|p| {
            let m = p[0].max(p[1]).max(p[2]);
            if m <= KNEE {
                return;
            }
            let x = (m - KNEE) / (peak - KNEE);
            let f = (x * a / (1.0 + (a - 1.0) * x)).min(1.0);
            let folded = KNEE + (1.0 - KNEE) * f;
            let scale = folded / m;
            let t = ((folded - 0.9) / 0.1).clamp(0.0, 1.0);
            let toward_white = t * t * (3.0 - 2.0 * t) * 0.5;
            for v in p.iter_mut() {
                *v = *v * scale * (1.0 - toward_white) + folded * toward_white;
            }
        });
    }
    merged.par_iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
}

/// The camera named in a RAW's metadata, for the merged DNG.
pub fn camera_of(bytes: &[u8]) -> crate::hdr_dng::DngCamera {
    let source = rawler::rawsource::RawSource::new_from_slice(bytes);
    let named = rawler::get_decoder(&source)
        .ok()
        .and_then(|d| d.raw_metadata(&source, &Default::default()).ok())
        .map(|m| (m.make, m.model));
    let (make, model) = named.unwrap_or_default();
    crate::hdr_dng::DngCamera { make, model }
}

/// A new temporary path for a merge, unique so the render caches never
/// mistake one merge for the last.
pub fn temporary_path(extension: &str) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join("darkroom-index-hdr");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!(
        "merge-{}-{}.{extension}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )))
}

/// Move the merge beside its first frame (copying when the photos are on
/// another volume, as a cloud folder is), and return where it went.
pub fn save(result: &HdrResult) -> Result<PathBuf> {
    let first = result.sources.first().context("A merge without frames")?;
    let target = crate::versions::free_version_path(first, result.kind, result.extension);
    if std::fs::rename(&result.file, &target).is_err() {
        std::fs::copy(&result.file, &target).context("Saving the merge")?;
        let _ = std::fs::remove_file(&result.file);
    }
    ensure!(target.exists(), "The merge was not saved");
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    /// Folding keeps everything up to the knee, puts the brightest light at
    /// white, and never reverses an order of brightness.
    #[test]
    fn folding_is_gentle_below_the_knee_and_monotone_above() {
        let values: Vec<f32> = (0..400).map(|i| i as f32 / 100.0).collect();
        let mut img = Rgb32FImage::from_fn(400, 1, |x, _| {
            let v = values[x as usize];
            Rgb([v, v * 0.6, v * 0.3])
        });
        fold_highlights(&mut img);
        for x in 0..80 {
            assert_eq!(
                img.get_pixel(x, 0)[0],
                values[x as usize],
                "changed below the knee"
            );
        }
        let mut last = 0.0;
        for x in 0..400 {
            let r = img.get_pixel(x, 0)[0];
            assert!(r >= last - 1e-6, "brightness reversed at {x}");
            assert!(r <= 1.0);
            last = r;
        }
        assert!(
            img.get_pixel(399, 0)[0] > 0.999,
            "the brightest light should reach white"
        );
    }
}
