//! Smart previews: a compact stand-in for a photo kept in a cloud service,
//! so the library and editor can show it, with its edits, while the photo
//! itself stays in the cloud (as Lightroom's Smart Previews do).
//!
//! A preview is the photo's *decoded* picture (what the colour engine
//! develops, before any edit), at most `LONG_EDGE` pixels on its long side,
//! stored beside the photo as `<photo>.dripreview` so the cloud service
//! syncs it to every Mac. Its pixels are kept in DaVinci Wide Gamut through
//! the DaVinci Intermediate curve, as 16-bit lossy JPEG XL: about a megabyte,
//! with the range and precision edits need.
//!
//! When the photo is only in the cloud, the engine reads the preview in its
//! place, scaled back up to the photo's own size, so crops, masks and
//! patches (all in the photo's pixels) land where they belong; it is only
//! softer close up. A preview records the photo's size and modification
//! time, and is not used for a different version of the photo.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose};
use image::Rgba32FImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::color_engine::config::{Primaries, SourceColor};
use crate::color_engine::input::{DecodedFrame, InputProvenance};
use crate::color_engine::spaces;

/// The longest side a preview keeps, as Lightroom's.
pub const LONG_EDGE: u32 = 2560;
/// JPEG XL distance (1 is visually lossless): 0.3, measured on RAWs and
/// JPEGs, keeps 95% of pixels within ~0.1 stop at 0.6-1 MB.
const DISTANCE: f32 = 0.3;
const MAGIC: &[u8; 8] = b"DRIPREV1";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    version: u32,
    /// The photo it stands in for: its size in bytes and modification time.
    source_size: u64,
    source_modified: u64,
    /// The photo's decoded size, which the preview is scaled back up to.
    width: u32,
    height: u32,
    color: SourceColor,
    source_hash: String,
    transform_decision: String,
    interpretation: String,
    profile_hash: Option<String>,
    /// The photo's ICC profile, when it carried one (base64).
    source_profile: Option<String>,
    exif: Option<HashMap<String, String>>,
}

/// Linear light (0 up, highlights well above 1) to the 0..1 code value
/// stored: highlights rolled off (`x / (x + K)`, so nothing clips), then the
/// sRGB curve, so the encoder's own model of what the eye sees spends its
/// bits where edits will look. `from_code` is its exact inverse.
const K: f64 = 0.75;
fn to_code(x: f64) -> f64 {
    let x = x.max(0.0);
    let t = x / (x + K);
    if t <= 0.003_130_8 {
        t * 12.92
    } else {
        1.055 * t.powf(1.0 / 2.4) - 0.055
    }
}
fn from_code(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    let t = if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    };
    let t = t.min(1.0 - 1e-9);
    t * K / (1.0 - t)
}

/// What is kept on this Mac for a photo that is only in the cloud.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A small preview of each, synced beside it: browse and edit offline.
    Previews,
    /// Only a thumbnail (in this Mac's cache); opening downloads the photo.
    Thumbnails,
    /// Nothing; opening downloads the photo.
    Nothing,
}

impl Mode {
    fn parse(s: &str) -> Option<Mode> {
        match s {
            "previews" => Some(Mode::Previews),
            "thumbnails" => Some(Mode::Thumbnails),
            "none" => Some(Mode::Nothing),
            _ => None,
        }
    }
}

/// The modes as last loaded or saved in Settings: the default, and the
/// folders that differ from it.
type Modes = (Option<Mode>, Vec<(PathBuf, Mode)>);
static MODES: std::sync::RwLock<Modes> = std::sync::RwLock::new((None, Vec::new()));

/// Whether the first-time card about cloud photos has been answered: until
/// it has, nothing is downloaded to build previews.
pub static CARD_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_modes(settings: &crate::app_settings::AppSettings) {
    CARD_SEEN.store(
        settings.cloud_photos_card_seen.unwrap_or(false),
        std::sync::atomic::Ordering::Relaxed,
    );
    let default = settings.cloud_photos_mode.as_deref().and_then(Mode::parse);
    let mut folders: Vec<(PathBuf, Mode)> = settings
        .cloud_folder_modes
        .iter()
        .flatten()
        .filter_map(|(p, m)| Some((PathBuf::from(p), Mode::parse(m)?)))
        .collect();
    // Deepest first, so a folder's own choice wins over its parent's.
    folders.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
    if let Ok(mut modes) = MODES.write() {
        *modes = (default, folders);
    }
}

/// The mode for `photo`: its nearest folder's own choice, else the default.
pub fn mode_for(photo: &Path) -> Mode {
    let Ok(modes) = MODES.read() else {
        return Mode::Previews;
    };
    modes
        .1
        .iter()
        .find(|(folder, _)| photo.starts_with(folder))
        .map(|(_, m)| *m)
        .or(modes.0)
        .unwrap_or(Mode::Previews)
}

/// Resize without clamping: the image crate's resize clamps float pixels to
/// 0..1, which would flatten a RAW's highlights. Separable tent filter, as
/// wide as the scale when reducing (an area average), bilinear when
/// enlarging.
fn resize(img: &Rgba32FImage, w: u32, h: u32) -> Rgba32FImage {
    fn taps(from: u32, to: u32) -> Vec<Vec<(usize, f32)>> {
        let scale = from as f32 / to as f32;
        let support = scale.max(1.0);
        (0..to)
            .map(|i| {
                let centre = (i as f32 + 0.5) * scale - 0.5;
                let lo = (centre - support).floor().max(0.0) as usize;
                let hi = ((centre + support).ceil() as usize).min(from as usize - 1);
                let mut t: Vec<(usize, f32)> = (lo..=hi)
                    .map(|j| (j, (1.0 - (j as f32 - centre).abs() / support).max(0.0)))
                    .filter(|&(_, wt)| wt > 0.0)
                    .collect();
                if t.is_empty() {
                    t.push((centre.round().clamp(0.0, from as f32 - 1.0) as usize, 1.0));
                }
                let sum: f32 = t.iter().map(|&(_, wt)| wt).sum();
                t.iter_mut().for_each(|e| e.1 /= sum);
                t
            })
            .collect()
    }
    let (sw, sh) = img.dimensions();
    let (tx, ty) = (taps(sw, w), taps(sh, h));
    let src = img.as_raw();
    // Rows first, to (w x sh).
    let mut mid = vec![0f32; w as usize * sh as usize * 4];
    mid.par_chunks_mut(w as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let line = &src[y * sw as usize * 4..(y + 1) * sw as usize * 4];
            for (x, t) in tx.iter().enumerate() {
                let mut acc = [0f32; 4];
                for &(j, wt) in t {
                    for c in 0..4 {
                        acc[c] += line[j * 4 + c] * wt;
                    }
                }
                row[x * 4..x * 4 + 4].copy_from_slice(&acc);
            }
        });
    let mut out = vec![0f32; w as usize * h as usize * 4];
    out.par_chunks_mut(w as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for &(j, wt) in &ty[y] {
                let line = &mid[j * w as usize * 4..(j + 1) * w as usize * 4];
                for (o, v) in row.iter_mut().zip(line) {
                    *o += v * wt;
                }
            }
        });
    Rgba32FImage::from_raw(w, h, out).expect("sized to fit")
}

/// Photos that must be read from their original right now (exporting, or a
/// tool that makes patches at full size), with how many uses want it: the
/// engine does not stand a preview in for these, downloading the photo if
/// it must. Everything else rendering at the time still uses previews.
static NEED_ORIGINAL: std::sync::Mutex<Vec<(PathBuf, usize)>> = std::sync::Mutex::new(Vec::new());

/// Held while `photo` must be read from its original. If the photo was only
/// in the cloud, it is given back once nothing needs it (unless it is the
/// photo open in the editor, which is given back when another is opened).
pub struct OriginalNeeded {
    photo: PathBuf,
    was_cloud_only: bool,
}

pub fn need_original(photo: &Path) -> OriginalNeeded {
    let mut list = NEED_ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
    match list.iter_mut().find(|(p, _)| p == photo) {
        Some(entry) => entry.1 += 1,
        None => list.push((photo.to_path_buf(), 1)),
    }
    OriginalNeeded {
        photo: photo.to_path_buf(),
        was_cloud_only: crate::cloud_files::is_cloud_only(photo),
    }
}

impl Drop for OriginalNeeded {
    fn drop(&mut self) {
        let last = {
            let mut list = NEED_ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
            match list.iter().position(|(p, _)| *p == self.photo) {
                Some(i) => {
                    list[i].1 -= 1;
                    let last = list[i].1 == 0;
                    if last {
                        list.remove(i);
                    }
                    last
                }
                None => false,
            }
        };
        if last && self.was_cloud_only && !crate::cloud_files::is_open(&self.photo) {
            crate::cloud_files::give_back_later(self.photo.clone());
        }
    }
}

/// Whether `photo` may be shown from its preview now.
pub fn may_stand_in(photo: &Path) -> bool {
    mode_for(photo) == Mode::Previews
        && !NEED_ORIGINAL
            .lock()
            .map(|l| l.iter().any(|(p, _)| p == photo))
            .unwrap_or(false)
}

/// Where `photo`'s preview lives.
pub fn path_for(photo: &Path) -> PathBuf {
    let mut name = photo.file_name().unwrap_or_default().to_os_string();
    name.push(".dripreview");
    photo.with_file_name(name)
}

/// Whether `photo` lives in a cloud service's folder (where previews are
/// kept: anywhere else the photo is always on this Mac).
pub fn in_cloud_folder(photo: &Path) -> bool {
    let p = photo.to_string_lossy();
    p.contains("/Library/CloudStorage/") || p.contains("/Library/Mobile Documents/")
}

fn source_version(photo: &Path) -> Option<(u64, u64)> {
    let m = fs::metadata(photo).ok()?;
    let modified = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((m.len(), modified))
}

/// Whether `photo` has a preview made from its current version. Reads only
/// the preview's header.
pub fn is_current(photo: &Path) -> bool {
    read_header(&path_for(photo)).is_some_and(|(h, _)| {
        h.version == VERSION && source_version(photo) == Some((h.source_size, h.source_modified))
    })
}

fn read_header(path: &Path) -> Option<(Header, u64)> {
    let mut f = fs::File::open(path).ok()?;
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic).ok()?;
    if &magic != MAGIC {
        return None;
    }
    let mut len = [0u8; 4];
    f.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 4 << 20 {
        return None;
    }
    let mut json = vec![0u8; len];
    f.read_exact(&mut json).ok()?;
    Some((serde_json::from_slice(&json).ok()?, 12 + len as u64))
}

/// A preview ready to be written: the picture already reduced (quick, done
/// where the photo was decoded), the slow encode left for the background.
pub struct Pending {
    photo: PathBuf,
    small: Rgba32FImage,
    width: u32,
    height: u32,
    color: SourceColor,
    source_hash: String,
    transform_decision: String,
    interpretation: String,
    profile_hash: Option<String>,
    source_profile: Option<Vec<u8>>,
}

/// Reduce `photo`'s decoded picture (before any highlight recovery or look:
/// what the engine develops) for its preview.
pub fn prepare(photo: &Path, frame: &DecodedFrame) -> Pending {
    let (width, height) = frame.pixels.dimensions();
    let scale = (LONG_EDGE as f32 / width.max(height) as f32).min(1.0);
    let (pw, ph) = (
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    );
    let small = if scale < 1.0 {
        resize(&frame.pixels, pw, ph)
    } else {
        frame.pixels.clone()
    };
    Pending {
        photo: photo.to_path_buf(),
        small,
        width,
        height,
        color: frame.color.clone(),
        source_hash: frame.provenance.source_hash.clone(),
        transform_decision: frame.provenance.transform_decision.clone(),
        interpretation: frame.provenance.interpretation.clone(),
        profile_hash: frame.provenance.profile_hash.clone(),
        source_profile: frame.source_profile.clone(),
    }
}

/// Make `photo`'s preview and write it beside it.
pub fn write(
    photo: &Path,
    frame: &DecodedFrame,
    exif: Option<HashMap<String, String>>,
) -> Result<()> {
    write_pending(prepare(photo, frame), exif)
}

/// Encode and write a prepared preview (slow: run it off the render path).
pub fn write_pending(p: Pending, exif: Option<HashMap<String, String>>) -> Result<()> {
    write_pending_with(p, exif, DISTANCE)
}

#[cfg(test)]
fn write_with(
    photo: &Path,
    frame: &DecodedFrame,
    exif: Option<HashMap<String, String>>,
    distance: f32,
) -> Result<()> {
    write_pending_with(prepare(photo, frame), exif, distance)
}

fn write_pending_with(
    p: Pending,
    exif: Option<HashMap<String, String>>,
    distance: f32,
) -> Result<()> {
    let photo = p.photo.as_path();
    let (source_size, source_modified) =
        source_version(photo).context("the photo's own size and date")?;
    let (width, height) = (p.width, p.height);
    let small = p.small;
    let (pw, ph) = small.dimensions();
    let to_dwg = spaces::conversion(p.color.primaries, Primaries::DavinciWideGamut);
    let encoded: Vec<u16> = small
        .as_raw()
        .par_chunks(4)
        .flat_map_iter(|p| {
            let rgb = to_dwg * glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
            [rgb.x, rgb.y, rgb.z].map(|v| (to_code(v) * 65535.0).round() as u16)
        })
        .collect();
    let bytes: Vec<u8> = encoded.iter().flat_map(|v| v.to_ne_bytes()).collect();
    let jxl = jxl_encoder::LossyConfig::new(distance)
        .encode(&bytes, pw, ph, jxl_encoder::PixelLayout::Rgb16)
        .map_err(|e| anyhow::anyhow!("JPEG XL: {e}"))?;
    let header = Header {
        version: VERSION,
        source_size,
        source_modified,
        width,
        height,
        color: p.color,
        source_hash: p.source_hash,
        transform_decision: p.transform_decision,
        interpretation: p.interpretation,
        profile_hash: p.profile_hash,
        source_profile: p
            .source_profile
            .as_ref()
            .map(|b| general_purpose::STANDARD.encode(b)),
        exif,
    };
    let json = serde_json::to_vec(&header)?;
    let mut out = Vec::with_capacity(12 + json.len() + jxl.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&jxl);
    // Written whole, then moved into place: a half-written preview must
    // never be synced or read.
    let target = path_for(photo);
    let mut part = target.clone().into_os_string();
    part.push(".part");
    fs::write(&part, &out)?;
    fs::rename(&part, &target)?;
    log::info!(
        "[preview] wrote {} ({pw}x{ph}, {} KB)",
        target.display(),
        out.len() / 1024
    );
    Ok(())
}

/// The camera data a preview carries.
pub fn exif(photo: &Path) -> Option<HashMap<String, String>> {
    read_header(&path_for(photo)).and_then(|(h, _)| h.exif)
}

/// `photo`'s decoded picture, from its preview, at the photo's own size.
/// None when it has no current preview.
pub fn read_frame(photo: &Path) -> Result<Option<DecodedFrame>> {
    if !is_current(photo) {
        return Ok(None);
    }
    let path = path_for(photo);
    let (header, offset) = read_header(&path).context("preview header")?;
    let bytes = fs::read(&path)?;
    ensure!(bytes.len() as u64 > offset, "empty preview");
    let image = jxl_oxide::JxlImage::read_with_defaults(&bytes[offset as usize..])
        .map_err(|e| anyhow::anyhow!("the preview's picture: {e}"))?;
    let render = image
        .render_frame(0)
        .map_err(|e| anyhow::anyhow!("the preview's picture: {e}"))?;
    let buffer = render.image_all_channels();
    let (pw, ph, channels) = (
        buffer.width() as u32,
        buffer.height() as u32,
        buffer.channels(),
    );
    if pw == 0 || ph == 0 || channels < 3 {
        bail!("empty preview picture");
    }
    let from_dwg = spaces::conversion(Primaries::DavinciWideGamut, header.color.primaries);
    let linear: Vec<f32> = buffer
        .buf()
        .par_chunks(channels)
        .flat_map_iter(|p| {
            let dwg = glam::DVec3::new(
                from_code(p[0] as f64),
                from_code(p[1] as f64),
                from_code(p[2] as f64),
            );
            let rgb = from_dwg * dwg;
            [rgb.x as f32, rgb.y as f32, rgb.z as f32, 1.0]
        })
        .collect();
    let small = Rgba32FImage::from_raw(pw, ph, linear).context("preview pixels")?;
    #[cfg(test)]
    if std::env::var_os("SMART_PREVIEW_NO_UPSCALE").is_some() {
        return Ok(Some(test_frame(small)));
    }
    let pixels = if (pw, ph) == (header.width, header.height) {
        small
    } else {
        resize(&small, header.width, header.height)
    };
    Ok(Some(DecodedFrame {
        pixels,
        unrecovered: None,
        color: header.color,
        provenance: InputProvenance {
            source_hash: header.source_hash,
            transform_decision: header.transform_decision,
            decoder_revision: "smart_preview_v1",
            interpretation: header.interpretation + " (from its smart preview)",
            profile_hash: header.profile_hash,
            warnings: vec!["Shown from its smart preview: the photo is in the cloud.".into()],
            calibration: None,
        },
        source_profile: header
            .source_profile
            .and_then(|p| general_purpose::STANDARD.decode(p).ok()),
        rendered_origin: false,
        input_domain: None,
    }))
}

/// The editor's working copy of a photo, from its preview's decoded
/// picture: as the photo's own decode would give it (a RAW linear in sRGB
/// primaries, as floats; any other photo display-encoded, 8-bit).
pub fn pristine(frame: &DecodedFrame, is_raw: bool) -> image::DynamicImage {
    let to_srgb = spaces::conversion(frame.color.primaries, Primaries::Srgb);
    let (w, h) = frame.pixels.dimensions();
    let linear: Vec<[f32; 3]> = frame
        .pixels
        .as_raw()
        .par_chunks(4)
        .map(|p| {
            let v = to_srgb * glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
            [v.x as f32, v.y as f32, v.z as f32]
        })
        .collect();
    if is_raw {
        let data = linear
            .iter()
            .flat_map(|v| [v[0].max(0.0), v[1].max(0.0), v[2].max(0.0), 1.0])
            .collect();
        image::DynamicImage::ImageRgba32F(Rgba32FImage::from_raw(w, h, data).expect("sized to fit"))
    } else {
        let encode = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            let e = if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (e * 255.0).round() as u8
        };
        let data = linear.iter().flat_map(|v| v.map(encode)).collect();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, data).expect("sized to fit"))
    }
}

#[cfg(test)]
fn test_frame(pixels: Rgba32FImage) -> DecodedFrame {
    tests::frame_with(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Error and size by distance, on a real photo's decoded picture
    /// (SMART_PREVIEW_PHOTO=path): `cargo test --release smart_preview_sizes -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn smart_preview_sizes() {
        let src = std::env::var("SMART_PREVIEW_PHOTO").unwrap();
        let bytes = fs::read(&src).unwrap();
        let original = if crate::formats::is_raw_file(&src) {
            crate::color_engine::raw::decode_raw(&bytes, false, || Ok(())).unwrap()
        } else {
            crate::color_engine::input::decode_profiled_photo(&bytes).unwrap()
        };
        let dir = std::env::temp_dir().join("darkroom-smart-preview-sizes");
        let _ = fs::create_dir_all(&dir);
        let photo = dir.join(Path::new(&src).file_name().unwrap());
        fs::write(&photo, b"stand-in").unwrap();
        // Compared at the preview's own size: what it can hold.
        let (w, h) = original.pixels.dimensions();
        let s = LONG_EDGE as f32 / w.max(h) as f32;
        let (pw, ph) = ((w as f32 * s).round() as u32, (h as f32 * s).round() as u32);
        let reference = resize(&original.pixels, pw, ph);
        for distance in [2.0f32, 1.0, 0.5, 0.3] {
            write_with(&photo, &original, None, distance).unwrap();
            let size = fs::metadata(path_for(&photo)).unwrap().len();
            // The codec alone, at the preview's own size.
            unsafe { std::env::set_var("SMART_PREVIEW_NO_UPSCALE", "1") };
            let back = read_frame(&photo).unwrap().unwrap().pixels;
            unsafe { std::env::remove_var("SMART_PREVIEW_NO_UPSCALE") };
            // Error in stops on pixels with some light.
            let mut errs: Vec<f32> = reference
                .pixels()
                .zip(back.pixels())
                .flat_map(|(a, b)| {
                    (0..3)
                        .filter(|&c| a[c] > 0.01)
                        .map(move |c| (b[c].max(1e-6) / a[c]).log2().abs())
                })
                .collect();
            errs.sort_by(f32::total_cmp);
            let q = |f: f32| errs[((errs.len() - 1) as f32 * f) as usize];
            println!(
                "distance {distance}: {:.0} KB  error stops median {:.3} p95 {:.3} p99 {:.3}",
                size as f32 / 1024.0,
                q(0.5),
                q(0.95),
                q(0.99)
            );
        }
    }
    use crate::color_engine::config::{ReferenceDomain, Transfer};

    fn frame(w: u32, h: u32) -> DecodedFrame {
        frame_with(Rgba32FImage::from_fn(w, h, |x, y| {
            let t = x as f32 / w as f32;
            image::Rgba([
                0.02 + 3.0 * t,
                0.18 * (1.0 + (y as f32 / h as f32)),
                0.6 - 0.5 * t,
                1.0,
            ])
        }))
    }

    pub(super) fn frame_with(pixels: Rgba32FImage) -> DecodedFrame {
        DecodedFrame {
            pixels,
            unrecovered: None,
            color: SourceColor {
                primaries: Primaries::Srgb,
                transfer: Transfer::Linear,
                reference: ReferenceDomain::Scene,
            },
            provenance: InputProvenance {
                source_hash: "abc".into(),
                transform_decision: "test".into(),
                decoder_revision: "test",
                interpretation: "test".into(),
                profile_hash: None,
                warnings: vec![],
                calibration: None,
            },
            source_profile: None,
            rendered_origin: false,
            input_domain: None,
        }
    }

    #[test]
    fn a_preview_comes_back_at_the_photos_size_and_values() {
        let dir = std::env::temp_dir().join("darkroom-smart-preview-test");
        let _ = fs::create_dir_all(&dir);
        let photo = dir.join("photo.ARW");
        fs::write(&photo, b"stand-in").unwrap();
        let original = frame(3000, 2000);
        write(&photo, &original, None).unwrap();
        assert!(is_current(&photo));
        let size = fs::metadata(path_for(&photo)).unwrap().len();
        assert!(size < 3_000_000, "{size} bytes");
        let back = read_frame(&photo).unwrap().unwrap();
        assert_eq!(back.pixels.dimensions(), (3000, 2000));
        // Smooth gradients survive within a few percent, highlights above 1 too.
        for (x, y) in [(100, 100), (1500, 1000), (2900, 1900)] {
            let a = original.pixels.get_pixel(x, y);
            let b = back.pixels.get_pixel(x, y);
            for c in 0..3 {
                assert!(
                    (a[c] - b[c]).abs() <= 0.03 * a[c].abs().max(0.05),
                    "{a:?} vs {b:?}"
                );
            }
        }
        // A changed photo is not stood in for by its old preview.
        fs::write(&photo, b"a different photo").unwrap();
        assert!(!is_current(&photo));
        assert!(read_frame(&photo).unwrap().is_none());
    }
}
