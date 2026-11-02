//! What Cull shows: quick to make, since a cull goes through hundreds of
//! photos a second or two each.
//!
//! - **A RAW:** the JPEG the camera embedded in it (1616 × 1080 on a Sony
//!   A7C II), as Lightroom and Photo Mechanic cull: 26 ms, against 0.8 s to
//!   develop and render it.
//! - **A JPEG, HEIC, PNG…:** the file itself, at screen size.
//!
//!   Both keep their colour space: the JPEG sent carries the source's ICC
//!   profile (or Adobe RGB's, when EXIF marks the photo Adobe RGB without
//!   one), so the window shows an Adobe RGB or Display P3 photo as it is,
//!   not washed out, at no extra cost.
//! - **A photo only in a cloud service, or one with no embedded JPEG, or
//!   "Show edits" on:** developed through the colour engine (a cloud photo
//!   from its smart preview, so nothing downloads), as a JPEG. A cloud
//!   photo without a preview shows its grid thumbnail if it has one; failing
//!   both, it is downloaded and given back to the cloud once developed.
//!
//! Zooming in develops the photo at full size, for focus
//! (`generate_cull_full`): a JPEG off the main thread, which Cull asks for
//! as soon as a photo has been looked at for a moment, so it is usually
//! ready by the time you zoom.

use std::io::Cursor;
use std::path::Path;

use image::DynamicImage;
use rawler::decoders::RawDecodeParams;
use tauri::Manager;
use tauri::ipc::Response;

use crate::AppState;
use crate::color_engine::input::{ExifColorSpace, exif_color_space};

/// Longest side of a cull preview.
const SIZE: u32 = 1920;
const QUALITY: u8 = 88;

/// A picture's colour space, as the ICC profile its JPEG is to carry; None
/// for sRGB, which a JPEG without one is taken to be.
fn display_profile(icc: Option<Vec<u8>>, bytes: &[u8]) -> Option<Vec<u8>> {
    // Only an RGB profile describes the RGB pixels sent (a grey photo's
    // pixels are sent as RGB).
    if let Some(icc) = icc {
        return moxcms::ColorProfile::new_from_slice(&icc)
            .is_ok_and(|p| p.color_space == moxcms::DataColorSpace::Rgb)
            .then_some(icc);
    }
    match exif_color_space(bytes) {
        ExifColorSpace::AdobeRgb => moxcms::ColorProfile::new_adobe_rgb().encode().ok(),
        _ => None,
    }
}

fn jpeg(image: &DynamicImage, profile: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, QUALITY);
    if let Some(profile) = profile {
        encoder
            .set_icc_profile(profile)
            .map_err(|e| e.to_string())?;
    }
    encoder.encode_image(&rgb).map_err(|e| e.to_string())?;
    Ok(out)
}

/// The grid's thumbnail of a photo (with its edits, as the grid shows it),
/// when one was made: read from this Mac's cache, never from the photo.
fn cached_thumbnail(path: &str, app_handle: &tauri::AppHandle) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let dir = crate::file_management::get_thumb_cache_dir(app_handle).ok()?;
    let keys = crate::file_management::ThumbnailKeys::current(app_handle);
    if !crate::file_management::has_cached_thumbnail(path, &dir, &keys) {
        return None;
    }
    let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    let (data, _, _) = crate::file_management::generate_single_thumbnail_and_cache(
        path, &dir, None, false, app_handle, &settings,
    )?;
    let encoded = data.strip_prefix("data:image/jpeg;base64,")?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

/// The camera's own JPEG inside a RAW, upright, or None.
fn embedded(bytes: &[u8]) -> Option<DynamicImage> {
    let source = rawler::rawsource::RawSource::new_from_slice(bytes);
    let decoder = rawler::get_decoder(&source).ok()?;
    let params = RawDecodeParams::default();
    let image = decoder
        .full_image(&source, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.preview_image(&source, &params).ok().flatten())?;
    // Small thumbnails are no use on a screen.
    if image.width().max(image.height()) < 1000 {
        return None;
    }
    let orientation = decoder
        .raw_metadata(&source, &params)
        .ok()
        .and_then(|m| m.exif.orientation)
        .and_then(|o| image::metadata::Orientation::from_exif(o as u8));
    let mut image = image;
    if let Some(o) = orientation {
        image.apply_orientation(o);
    }
    Some(image)
}

/// A finished picture (JPEG, HEIC, PNG…) at screen size, upright, with the
/// ICC profile it carries.
fn finished(bytes: &[u8]) -> Option<(DynamicImage, Option<Vec<u8>>)> {
    let mut decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    use image::ImageDecoder;
    let orientation = decoder.orientation().ok();
    let icc = decoder.icc_profile().ok().flatten();
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        image.apply_orientation(o);
    }
    let image = if image.width().max(image.height()) > SIZE {
        image.thumbnail(SIZE, SIZE)
    } else {
        image
    };
    Some((image, icc))
}

/// Where a photo kept by a cloud service can be shown from.
struct CloudPhoto<'a> {
    photo: &'a Path,
    /// Only in the cloud: reading it would download it.
    only: bool,
    /// Developed from its smart preview (current, and allowed now).
    from_preview: bool,
}

impl<'a> CloudPhoto<'a> {
    fn of(real: &'a str) -> Self {
        let photo = Path::new(real);
        let only = crate::cloud_files::is_cloud_only(photo);
        let from_preview = only
            && crate::smart_preview::may_stand_in(photo)
            && crate::smart_preview::is_current(photo);
        CloudPhoto {
            photo,
            only,
            from_preview,
        }
    }

    /// Held while a cloud photo with no preview is read (so downloaded) to
    /// develop it: it is given back to the cloud afterwards, as the editor
    /// gives back what it opened. Cull develops a few photos ahead, which
    /// would otherwise leave every one it passed on this Mac.
    fn downloading(&self) -> Option<crate::smart_preview::OriginalNeeded> {
        (self.only && !self.from_preview).then(|| crate::smart_preview::need_original(self.photo))
    }
}

#[tauri::command(async)]
pub fn generate_cull_preview(
    path: String,
    show_edits: bool,
    js_adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    let real = path.split("?vc=").next().unwrap_or(&path).to_string();
    let cloud = CloudPhoto::of(&real);
    if !show_edits && !cloud.only {
        let bytes = std::fs::read(&real).map_err(|e| e.to_string())?;
        let quick = if crate::formats::is_raw_file(&real) {
            // The camera embeds its JPEG in the colour space it was set to,
            // which only the RAW's EXIF says.
            embedded(&bytes).map(|image| (image, None))
        } else if crate::formats::is_video_file(&real) {
            None
        } else {
            finished(&bytes)
        };
        if let Some((image, icc)) = quick {
            return jpeg(&image, display_profile(icc, &bytes)).map(Response::new);
        }
    }
    // A cloud photo with no preview yet: its grid thumbnail, rather than
    // downloading it.
    if cloud.only
        && !cloud.from_preview
        && let Some(thumbnail) = cached_thumbnail(&path, &app_handle)
    {
        return Ok(Response::new(thumbnail));
    }
    let _original = cloud.downloading();
    // Developed: with the photo's edits when asked, from its smart preview
    // when it is only in the cloud.
    let state = app_handle.state::<AppState>();
    let context = crate::image_processing::get_or_init_gpu_context(&state, &app_handle)?;
    let edits = crate::image_processing::render_adjustments_for_empty(&js_adjustments);
    let frame = crate::color_engine::application::render_for_output(
        &context,
        &state,
        &real,
        edits.as_ref(),
        Some(SIZE),
    )
    .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    frame
        .write_display_jpeg(&mut out, QUALITY)
        .map_err(|e| e.to_string())?;
    Ok(Response::new(out))
}

/// Quality of a full-size picture: focus is judged on it, so higher than
/// the screen preview's. 6 MB for 33 megapixels, where the PNG it replaced
/// was 55 MB to send and decode.
const FULL_QUALITY: u8 = 92;

/// The photo developed at full size, for checking focus at 100%: with its
/// edits when Show edits is on, as it opens unedited otherwise.
#[tauri::command(async)]
pub fn generate_cull_full(
    path: String,
    show_edits: bool,
    js_adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    let real = path.split("?vc=").next().unwrap_or(&path).to_string();
    // A cloud photo with a preview is shown from it (softer close up); one
    // without is downloaded for this, and given back after.
    let _original = CloudPhoto::of(&real).downloading();
    let state = app_handle.state::<AppState>();
    let context = crate::image_processing::get_or_init_gpu_context(&state, &app_handle)?;
    let unedited = serde_json::json!({});
    let edits = crate::image_processing::render_adjustments_for_empty(if show_edits {
        &js_adjustments
    } else {
        &unedited
    });
    let frame = crate::color_engine::application::render_for_output(
        &context,
        &state,
        &real,
        edits.as_ref(),
        None,
    )
    .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    frame
        .write_display_jpeg(&mut out, FULL_QUALITY)
        .map_err(|e| e.to_string())?;
    Ok(Response::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A portrait RAW's embedded JPEG comes out upright, quickly.
    /// (CULL_TEST_RAW=<a vertical RAW>; skipped without one.)
    #[test]
    #[ignore]
    fn embedded_previews_are_upright_and_quick() {
        let Ok(path) = std::env::var("CULL_TEST_RAW") else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let t = std::time::Instant::now();
        let image = embedded(&bytes).expect("an embedded JPEG");
        let data = jpeg(&image, display_profile(None, &bytes)).unwrap();
        println!(
            "{}x{} JPEG {:.0} KB in {:.0} ms",
            image.width(),
            image.height(),
            data.len() as f64 / 1e3,
            t.elapsed().as_secs_f64() * 1e3
        );
        assert!(
            image.height() > image.width(),
            "a portrait photo should come out upright"
        );
    }

    fn png(profile: Option<Vec<u8>>, exif: Option<Vec<u8>>) -> Vec<u8> {
        use image::ImageEncoder;
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        if let Some(profile) = profile {
            encoder.set_icc_profile(profile).unwrap();
        }
        if let Some(exif) = exif {
            encoder.set_exif_metadata(exif).unwrap();
        }
        encoder
            .write_image(
                &[255, 0, 0, 0, 255, 0],
                2,
                1,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        bytes
    }

    /// The ICC profile a cull JPEG carries.
    fn profile_of(jpeg: &[u8]) -> Option<Vec<u8>> {
        use image::ImageDecoder;
        image::codecs::jpeg::JpegDecoder::new(Cursor::new(jpeg))
            .unwrap()
            .icc_profile()
            .unwrap()
    }

    fn quick(bytes: &[u8]) -> Vec<u8> {
        let (image, icc) = finished(bytes).unwrap();
        jpeg(&image, display_profile(icc, bytes)).unwrap()
    }

    #[test]
    fn a_wide_gamut_photo_keeps_its_profile() {
        let p3 = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        assert_eq!(profile_of(&quick(&png(Some(p3.clone()), None))), Some(p3));
        // An sRGB photo is sent as plain sRGB.
        assert_eq!(profile_of(&quick(&png(None, None))), None);
    }

    #[test]
    fn an_adobe_rgb_photo_without_a_profile_is_sent_as_adobe_rgb() {
        // EXIF with ColorSpace 2 (Adobe RGB) in its Exif IFD.
        let le16 = |v: u16| v.to_le_bytes();
        let le32 = |v: u32| v.to_le_bytes();
        let mut exif = b"II\x2a\0".to_vec();
        exif.extend(le32(8));
        exif.extend(le16(1));
        exif.extend([le16(0x8769), le16(4)].concat());
        exif.extend([le32(1), le32(26), le32(0)].concat());
        exif.extend(le16(1));
        exif.extend([le16(0xA001), le16(3)].concat());
        exif.extend([le32(1), le32(2), le32(0)].concat());
        let adobe = moxcms::ColorProfile::new_adobe_rgb().encode().unwrap();
        assert_eq!(profile_of(&quick(&png(None, Some(exif)))), Some(adobe));
    }
}
