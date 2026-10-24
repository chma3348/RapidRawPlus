//! What Cull shows: quick to make, since a cull goes through hundreds of
//! photos a second or two each.
//!
//! - **A RAW:** the JPEG the camera embedded in it (1616 × 1080 on a Sony
//!   A7C II), as Lightroom and Photo Mechanic cull: 26 ms, against 0.8 s to
//!   develop and render it.
//! - **A JPEG, HEIC, PNG…:** the file itself, at screen size.
//! - **A photo only in a cloud service, or one with no embedded JPEG, or
//!   "Show edits" on:** developed through the colour engine (a cloud photo
//!   from its smart preview, so nothing downloads), as a JPEG.
//!
//! Zooming in still develops the photo at full size, for focus.

use std::io::Cursor;

use image::DynamicImage;
use rawler::decoders::RawDecodeParams;
use tauri::Manager;
use tauri::ipc::Response;

use crate::AppState;

/// Longest side of a cull preview.
const SIZE: u32 = 1920;
const QUALITY: u8 = 88;

fn jpeg(image: &DynamicImage) -> Result<Vec<u8>, String> {
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, QUALITY)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok(out)
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

/// A finished picture (JPEG, HEIC, PNG…) at screen size, upright.
fn finished(bytes: &[u8]) -> Option<DynamicImage> {
    let mut decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    use image::ImageDecoder;
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        image.apply_orientation(o);
    }
    Some(if image.width().max(image.height()) > SIZE {
        image.thumbnail(SIZE, SIZE)
    } else {
        image
    })
}

#[tauri::command(async)]
pub fn generate_cull_preview(
    path: String,
    show_edits: bool,
    js_adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    let real = path.split("?vc=").next().unwrap_or(&path).to_string();
    let cloud_only = crate::cloud_files::is_cloud_only_str(&real);
    if !show_edits && !cloud_only {
        let bytes = std::fs::read(&real).map_err(|e| e.to_string())?;
        let quick = if crate::formats::is_raw_file(&real) {
            embedded(&bytes)
        } else if crate::formats::is_video_file(&real) {
            None
        } else {
            finished(&bytes)
        };
        if let Some(image) = quick {
            return jpeg(&image).map(Response::new);
        }
    }
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
        let data = jpeg(&image).unwrap();
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
}
