use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageEncoder, imageops};
use jxl_encoder::{LosslessConfig, LossyConfig, PixelLayout};
use serde::{Deserialize, Serialize};

use crate::color_engine::config::OutputSpace;
use serde_json::Value;
use tauri::Emitter;
use tauri::Manager;

use crate::AppState;
use crate::exif_processing;
use crate::file_management::{generate_filename_from_template, parse_virtual_path};
use crate::image_processing::{GpuContext, get_or_init_gpu_context, render_adjustments_for_empty};

use crate::hydrate_adjustments;

#[cfg(test)]
mod format_tests {
    use crate::color_engine::config::OutputSpace;
    use image::{DynamicImage, ImageBuffer};

    /// Every format the Export panel offers encodes.
    #[test]
    fn every_offered_format_encodes() {
        let img = DynamicImage::ImageRgba16(ImageBuffer::from_fn(24, 16, |x, y| {
            image::Rgba([(x * 2000) as u16, (y * 3000) as u16, 30000, 65535])
        }));
        for format in ["jpg", "png", "tiff", "webp", "jxl", "avif"] {
            let bytes = super::encode_image_to_bytes(&img, format, 90, OutputSpace::Srgb)
                .unwrap_or_else(|e| panic!("{format}: {e}"));
            assert!(bytes.len() > 16, "{format} produced {} bytes", bytes.len());
        }
    }
}

#[cfg(test)]
mod precision_tests {
    /// Eight-bit exports of a 16-bit render: dither must break the plateaus
    /// rounding leaves in a slow ramp, keep the average, stay within a level
    /// of plain rounding, repeat exactly, and leave 8-bit images untouched.
    #[test]
    fn eight_bit_exports_are_dithered_from_deeper_renders() {
        let ramp = DynamicImage::ImageRgba16(ImageBuffer::from_fn(512, 4, |x, _| {
            let v = (20000 + x * 3) as u16;
            image::Rgba([v, v, v, 65535])
        }));
        let plain = ramp.to_rgb8();
        let dithered = super::to_rgb8_dithered(&ramp);
        let longest = |img: &image::RgbImage| {
            let (mut best, mut run) = (1, 1);
            for x in 1..img.width() {
                run = if img.get_pixel(x, 0)[0] == img.get_pixel(x - 1, 0)[0] {
                    run + 1
                } else {
                    1
                };
                best = best.max(run);
            }
            best
        };
        assert!(
            longest(&dithered) * 4 < longest(&plain),
            "banding survived: {} vs {}",
            longest(&dithered),
            longest(&plain)
        );
        let mean = |img: &image::RgbImage| {
            img.pixels().map(|p| p[0] as f64).sum::<f64>() / img.pixels().len() as f64
        };
        assert!((mean(&dithered) - mean(&plain)).abs() < 0.25);
        for (a, b) in plain.pixels().zip(dithered.pixels()) {
            assert!(a[0].abs_diff(b[0]) <= 1);
        }
        assert_eq!(
            dithered,
            super::to_rgb8_dithered(&ramp),
            "must be deterministic"
        );
        let eight = DynamicImage::ImageRgb8(plain.clone());
        assert_eq!(
            super::to_rgb8_dithered(&eight),
            plain,
            "an 8-bit image must come back unchanged"
        );
    }

    use super::*;
    use image::Rgba;

    #[test]
    fn supported_color_exports_embed_the_output_profile() {
        use image::ImageDecoder;
        let image = DynamicImage::ImageRgba16(ImageBuffer::from_pixel(
            2,
            2,
            Rgba([30001, 31002, 32003, 65535]),
        ));
        for (format, space) in ["png", "jpg", "tiff"]
            .into_iter()
            .flat_map(|f| [(f, OutputSpace::Srgb), (f, OutputSpace::DisplayP3)])
        {
            let bytes = encode_image_to_bytes(&image, format, 95, space).unwrap();
            // The generic reader derives TIFF allocation limits from pixel
            // size; on tiny fixtures this can be smaller than an ICC profile.
            // Read metadata directly with the TIFF decoder's normal limits.
            let profile = if format == "tiff" {
                image::codecs::tiff::TiffDecoder::new(Cursor::new(bytes))
                    .unwrap()
                    .icc_profile()
                    .unwrap()
            } else {
                image::ImageReader::new(Cursor::new(bytes))
                    .with_guessed_format()
                    .unwrap()
                    .into_decoder()
                    .unwrap()
                    .icc_profile()
                    .unwrap()
            }
            .unwrap_or_else(|| panic!("missing {format} export profile"));
            let mut profile = profile;
            let mut expected = space.icc_profile().unwrap();
            // ICC creation timestamps may differ across a second boundary.
            profile[24..36].fill(0);
            expected[24..36].fill(0);
            assert_eq!(profile, expected, "{format} {space:?} profile changed");
        }
    }

    #[test]
    fn png_and_tiff_keep_real_16_bit_values() {
        let image = DynamicImage::ImageRgba16(ImageBuffer::from_fn(1025, 2, |x, _| {
            Rgba([30000 + x as u16, 31001, 32002, 65535])
        }));
        for format in ["png", "tiff", "tif"] {
            let bytes = encode_image_to_bytes(&image, format, 95, OutputSpace::Srgb).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert!(matches!(
                decoded.color(),
                image::ColorType::Rgb16 | image::ColorType::Rgba16
            ));
            assert_eq!(
                decoded.to_rgb16(),
                image.to_rgb16(),
                "{format} lost precision"
            );
        }
        for format in ["jpg", "webp", "avif", "jxl"] {
            assert!(
                !encode_image_to_bytes(&image, format, 95, OutputSpace::Srgb)
                    .unwrap()
                    .is_empty(),
                "{format}"
            );
        }
    }

    #[test]
    fn resize_and_transparent_watermark_do_not_reduce_base_to_eight_bits() {
        let mut image = DynamicImage::ImageRgba16(ImageBuffer::from_pixel(
            33,
            17,
            Rgba([30001, 31002, 32003, 65535]),
        ));
        image = image.resize(31, 15, imageops::FilterType::Lanczos3);
        assert!(image.as_rgba16().is_some());
        let before = image.clone();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transparent.png");
        image::RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 0]))
            .save(&path)
            .unwrap();
        apply_watermark(
            &mut image,
            &WatermarkSettings {
                path: path.to_string_lossy().into_owned(),
                anchor: WatermarkAnchor::Center,
                scale: 100.0,
                spacing: 0.0,
                opacity: 100.0,
            },
        )
        .unwrap();
        assert_eq!(image, before);
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum ResizeMode {
    LongEdge,
    ShortEdge,
    Width,
    Height,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ResizeOptions {
    pub mode: ResizeMode,
    pub value: u32,
    pub dont_enlarge: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExportSettings {
    pub jpeg_quality: u8,
    pub resize: Option<ResizeOptions>,
    pub keep_metadata: bool,
    #[serde(default)]
    pub preserve_timestamps: bool,
    pub strip_gps: bool,
    pub filename_template: Option<String>,
    pub watermark: Option<WatermarkSettings>,
    #[serde(default)]
    pub export_masks: bool,
    #[serde(default)]
    pub preserve_folders: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum WatermarkAnchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WatermarkSettings {
    pub path: String,
    pub anchor: WatermarkAnchor,
    pub scale: f32,
    pub spacing: f32,
    pub opacity: f32,
}

fn apply_watermark(
    base_image: &mut DynamicImage,
    watermark_settings: &WatermarkSettings,
) -> Result<(), String> {
    let watermark_img = image::open(&watermark_settings.path)
        .map_err(|e| format!("Failed to open watermark image: {}", e))?;

    let (base_w, base_h) = base_image.dimensions();
    let base_min_dim = base_w.min(base_h) as f32;

    let watermark_scale_factor =
        (base_min_dim * (watermark_settings.scale / 100.0)) / watermark_img.width().max(1) as f32;
    let new_wm_w = (watermark_img.width() as f32 * watermark_scale_factor).round() as u32;
    let new_wm_h = (watermark_img.height() as f32 * watermark_scale_factor).round() as u32;

    if new_wm_w == 0 || new_wm_h == 0 {
        return Ok(());
    }

    let scaled_watermark =
        watermark_img.resize_exact(new_wm_w, new_wm_h, image::imageops::FilterType::Lanczos3);
    let mut scaled_watermark_rgba = scaled_watermark.to_rgba8();

    let opacity_factor = (watermark_settings.opacity / 100.0).clamp(0.0, 1.0);
    for pixel in scaled_watermark_rgba.pixels_mut() {
        pixel[3] = (pixel[3] as f32 * opacity_factor) as u8;
    }
    let final_watermark = DynamicImage::ImageRgba8(scaled_watermark_rgba);

    let spacing_pixels = (base_min_dim * (watermark_settings.spacing / 100.0)) as i64;
    let (wm_w, wm_h) = final_watermark.dimensions();

    let x = match watermark_settings.anchor {
        WatermarkAnchor::TopLeft | WatermarkAnchor::CenterLeft | WatermarkAnchor::BottomLeft => {
            spacing_pixels
        }
        WatermarkAnchor::TopCenter | WatermarkAnchor::Center | WatermarkAnchor::BottomCenter => {
            (base_w as i64 - wm_w as i64) / 2
        }
        WatermarkAnchor::TopRight | WatermarkAnchor::CenterRight | WatermarkAnchor::BottomRight => {
            base_w as i64 - wm_w as i64 - spacing_pixels
        }
    };

    let y = match watermark_settings.anchor {
        WatermarkAnchor::TopLeft | WatermarkAnchor::TopCenter | WatermarkAnchor::TopRight => {
            spacing_pixels
        }
        WatermarkAnchor::CenterLeft | WatermarkAnchor::Center | WatermarkAnchor::CenterRight => {
            (base_h as i64 - wm_h as i64) / 2
        }
        WatermarkAnchor::BottomLeft
        | WatermarkAnchor::BottomCenter
        | WatermarkAnchor::BottomRight => base_h as i64 - wm_h as i64 - spacing_pixels,
    };

    if let Some(base) = base_image.as_mut_rgba16() {
        // DynamicImage's generic pixel interface is RGBA8. Blend through the
        // typed image so even pixels under a transparent watermark keep detail.
        image::imageops::overlay(base, &final_watermark.to_rgba16(), x, y);
    } else {
        image::imageops::overlay(base_image, &final_watermark, x, y);
    }

    Ok(())
}

fn calculate_resize_target(
    current_w: u32,
    current_h: u32,
    resize_opts: &ResizeOptions,
) -> (u32, u32) {
    if resize_opts.dont_enlarge {
        let exceeds = match resize_opts.mode {
            ResizeMode::LongEdge => current_w.max(current_h) > resize_opts.value,
            ResizeMode::ShortEdge => current_w.min(current_h) > resize_opts.value,
            ResizeMode::Width => current_w > resize_opts.value,
            ResizeMode::Height => current_h > resize_opts.value,
        };
        if !exceeds {
            return (current_w, current_h);
        }
    }

    let fix_width = match resize_opts.mode {
        ResizeMode::LongEdge => current_w >= current_h,
        ResizeMode::ShortEdge => current_w <= current_h,
        ResizeMode::Width => true,
        ResizeMode::Height => false,
    };

    let value = resize_opts.value;
    if fix_width {
        let h = (value as f32 * (current_h as f32 / current_w as f32)).round() as u32;
        (value, h)
    } else {
        let w = (value as f32 * (current_w as f32 / current_h as f32)).round() as u32;
        (w, value)
    }
}

fn apply_export_resize_and_watermark(
    mut image: DynamicImage,
    export_settings: &ExportSettings,
) -> Result<DynamicImage, String> {
    if let Some(resize_opts) = &export_settings.resize {
        let (current_w, current_h) = image.dimensions();
        let (target_w, target_h) = calculate_resize_target(current_w, current_h, resize_opts);

        if target_w != current_w || target_h != current_h {
            image = image.resize(target_w, target_h, imageops::FilterType::Lanczos3);
        }
    }

    if let Some(watermark_settings) = &export_settings.watermark {
        apply_watermark(&mut image, watermark_settings)?;
    }
    Ok(image)
}

pub(crate) fn process_image_for_export_pipeline(
    path: &str,
    js_adjustments: &Value,
    context: &GpuContext,
    state: &tauri::State<AppState>,
) -> Result<DynamicImage, String> {
    let render_adjustments_cow = render_adjustments_for_empty(js_adjustments);
    let render_adjustments = render_adjustments_cow.as_ref();

    crate::color_engine::application::render_file(context, state, path, render_adjustments, None)
        .map(|f| f.export_rgba16())
        .map_err(|e| e.to_string())
}

fn set_timestamps_from_exif(src: &Path, dst: &Path) {
    let capture_dt = exif_processing::get_creation_date_from_path(src);
    let ft = filetime::FileTime::from_unix_time(
        capture_dt.timestamp(),
        capture_dt.timestamp_subsec_nanos(),
    );
    if let Err(e) = filetime::set_file_times(dst, ft, ft) {
        log::warn!("Could not set timestamps on '{}': {}", dst.display(), e);
    }
}

fn save_image_with_metadata(
    image: &DynamicImage,
    output_path: &std::path::Path,
    source_path_str: &str,
    export_settings: &ExportSettings,
    space: OutputSpace,
) -> Result<(), String> {
    let extension = output_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let mut image_bytes =
        encode_image_to_bytes(image, &extension, export_settings.jpeg_quality, space)?;

    exif_processing::write_image_with_metadata(
        &mut image_bytes,
        source_path_str,
        &extension,
        export_settings.keep_metadata,
        export_settings.strip_gps,
        space,
    )?;

    #[cfg(target_os = "android")]
    {
        let file_name = output_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "Missing Android export file name".to_string())?;
        crate::android_integration::save_image_bytes_to_android_gallery(
            file_name,
            mime_type_for_extension(&extension),
            &image_bytes,
        )?;
    }

    #[cfg(not(target_os = "android"))]
    fs::write(output_path, image_bytes).map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(target_os = "android")]
pub fn mime_type_for_extension(extension: &str) -> &'static str {
    match extension {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        "tif" | "tiff" => "image/tiff",
        "jxl" => "image/jxl",
        _ => "application/octet-stream",
    }
}

/// Eight bits from a deeper image, dithered. A 16-bit render carries smooth
/// gradients that plain rounding turns into steps — visible in a JPEG sky.
/// One LSB of triangular noise, fixed per pixel, trades them for noise too
/// fine to see. An image already in eight bits comes back unchanged.
fn to_rgba8_dithered(image: &DynamicImage) -> image::RgbaImage {
    if matches!(
        image,
        DynamicImage::ImageRgba8(_) | DynamicImage::ImageRgb8(_)
    ) {
        return image.to_rgba8();
    }
    let deep = image.to_rgba32f();
    ImageBuffer::from_fn(deep.width(), deep.height(), |x, y| {
        let p = deep.get_pixel(x, y).0;
        image::Rgba(std::array::from_fn(|c| {
            let noise = if c == 3 {
                0.0
            } else {
                crate::color_engine::renderer_tpdf(x, y, c as u32)
            };
            (p[c].clamp(0.0, 1.0) * 255.0 + noise)
                .round()
                .clamp(0.0, 255.0) as u8
        }))
    })
}

fn to_rgb8_dithered(image: &DynamicImage) -> image::RgbImage {
    DynamicImage::ImageRgba8(to_rgba8_dithered(image)).to_rgb8()
}

/// `space` is the colour space the pixels are in; the formats that carry a
/// profile embed the matching one.
pub(crate) fn encode_image_to_bytes(
    image: &DynamicImage,
    output_format: &str,
    jpeg_quality: u8,
    space: OutputSpace,
) -> Result<Vec<u8>, String> {
    let profile = || space.icc_profile().map_err(|e| e.to_string());
    let mut image_bytes = Vec::new();
    let mut cursor = Cursor::new(&mut image_bytes);

    match output_format.to_lowercase().as_str() {
        "jxl" => {
            let (width, height) = image.dimensions();
            let has_alpha = image.color().has_alpha();

            let jxl_data = if jpeg_quality == 100 {
                if has_alpha {
                    let rgba = to_rgba8_dithered(image);
                    LosslessConfig::new()
                        .encode(rgba.as_raw(), width, height, PixelLayout::Rgba8)
                        .map_err(|e| format!("Failed to encode lossless JXL: {}", e))?
                } else {
                    let rgb = to_rgb8_dithered(image);
                    LosslessConfig::new()
                        .encode(rgb.as_raw(), width, height, PixelLayout::Rgb8)
                        .map_err(|e| format!("Failed to encode lossless JXL: {}", e))?
                }
            } else {
                let distance = (100.0 - jpeg_quality as f32) / 10.0;
                let distance = distance.max(0.01);

                if has_alpha {
                    let rgba = to_rgba8_dithered(image);
                    LossyConfig::new(distance)
                        .encode(rgba.as_raw(), width, height, PixelLayout::Rgba8)
                        .map_err(|e| format!("Failed to encode lossy JXL: {}", e))?
                } else {
                    let rgb = to_rgb8_dithered(image);
                    LossyConfig::new(distance)
                        .encode(rgb.as_raw(), width, height, PixelLayout::Rgb8)
                        .map_err(|e| format!("Failed to encode lossy JXL: {}", e))?
                }
            };

            return Ok(jxl_data);
        }
        "webp" => {
            let webp_image = DynamicImage::ImageRgba8(to_rgba8_dithered(image));
            let encoder = webp::Encoder::from_image(&webp_image)
                .map_err(|_| "Failed to create WebP encoder".to_string())?;
            let webp_mem = encoder.encode(jpeg_quality as f32);
            return Ok(webp_mem.to_vec());
        }
        "jpg" | "jpeg" => {
            let rgb_image = to_rgb8_dithered(image);
            let mut encoder = JpegEncoder::new_with_quality(&mut cursor, jpeg_quality);
            encoder
                .set_icc_profile(profile()?)
                .map_err(|e| e.to_string())?;
            rgb_image
                .write_with_encoder(encoder)
                .map_err(|e| e.to_string())?;
        }
        "png" => {
            let image_to_encode = if image.as_rgb32f().is_some() {
                DynamicImage::ImageRgb16(image.to_rgb16())
            } else {
                image.clone()
            };

            let mut encoder = image::codecs::png::PngEncoder::new(&mut cursor);
            encoder
                .set_icc_profile(profile()?)
                .map_err(|e| e.to_string())?;
            image_to_encode
                .write_with_encoder(encoder)
                .map_err(|e| e.to_string())?;
        }
        "tif" | "tiff" => {
            let mut encoder = image::codecs::tiff::TiffEncoder::new(&mut cursor);
            encoder
                .set_icc_profile(profile()?)
                .map_err(|e| e.to_string())?;
            DynamicImage::ImageRgb16(image.to_rgb16())
                .write_with_encoder(encoder)
                .map_err(|e| e.to_string())?;
        }
        "avif" => {
            DynamicImage::ImageRgba8(to_rgba8_dithered(image))
                .write_to(&mut cursor, image::ImageFormat::Avif)
                .map_err(|e| e.to_string())?;
        }
        _ => return Err(format!("Unsupported file format: {}", output_format)),
    };
    Ok(image_bytes)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn export_images(
    paths: Vec<String>,
    output_folder_or_file: String,
    is_explicit_file_path: bool,
    base_origin_folders: Vec<String>,
    export_settings: ExportSettings,
    output_format: String,
    current_edit_path: Option<String>,
    current_edit_adjustments: Option<Value>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    if state.export_task_handle.lock().unwrap().is_some() {
        return Err("An export is already in progress.".to_string());
    }

    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let context = Arc::new(context);
    let progress_counter = Arc::new(AtomicUsize::new(0));

    let available_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();

    let available_ram_gb = sys.available_memory() as f64 / 1024.0 / 1024.0 / 1024.0;

    let ram_based_limit = (available_ram_gb / 2.5).floor() as usize;

    let num_threads = if paths.len() == 1 {
        1
    } else {
        available_cores.min(ram_based_limit).clamp(1, 16)
    };

    log::info!(
        "Batch Export: {} cores, {:.1} GB free RAM -> {} threads",
        available_cores,
        available_ram_gb,
        num_threads
    );

    let task = tokio::spawn(async move {
        let output_folder_path = std::path::Path::new(&output_folder_or_file);
        let total_paths = paths.len();
        let mut base_path_counts: HashMap<String, usize> = HashMap::new();
        let mut export_items = Vec::with_capacity(total_paths);

        for (i, path_str) in paths.into_iter().enumerate() {
            let (source_path, _) = parse_virtual_path(&path_str);
            let source_str = source_path.to_string_lossy().to_string();
            let count = base_path_counts.entry(source_str.clone()).or_insert(0);
            *count += 1;

            let mut explicit_vc = None;
            if let Some(idx) = path_str.rfind("vc=") {
                let id_str = path_str[idx + 3..].split('&').next().unwrap_or("");
                if let Ok(id) = id_str.parse::<u32>() {
                    explicit_vc = Some(id);
                }
            }
            if explicit_vc.is_none() {
                let lower = path_str.to_lowercase();
                if let Some(idx) = lower.rfind("_vc") {
                    let id_str: String = lower[idx + 3..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(id) = id_str.parse::<u32>() {
                        explicit_vc = Some(id);
                    }
                }
            }
            export_items.push((i, path_str, *count, explicit_vc));
        }

        let semaphore = Arc::new(tokio::sync::Semaphore::new(num_threads));
        let mut join_handles = Vec::new();

        for (global_index, image_path_str, appearance_count, explicit_vc) in export_items {
            let permit = semaphore.clone().acquire_owned().await.unwrap();

            let app_handle_clone = app_handle.clone();
            let context_clone = Arc::clone(&context);
            let progress_counter_clone = Arc::clone(&progress_counter);
            let output_folder_path = output_folder_path.to_path_buf();
            let base_origin_folders = base_origin_folders.clone();
            let export_settings = export_settings.clone();
            let output_format = output_format.clone();
            let current_edit_path = current_edit_path.clone();
            let current_edit_adjustments = current_edit_adjustments.clone();

            let handle = tokio::task::spawn_blocking(move || {
                if app_handle_clone
                    .state::<AppState>()
                    .export_task_handle
                    .lock()
                    .unwrap()
                    .is_none()
                {
                    return Err("Export cancelled".to_string());
                }

                let state = app_handle_clone.state::<AppState>();
                let (source_path, sidecar_path) = parse_virtual_path(&image_path_str);
                let source_path_str = source_path.to_string_lossy().to_string();
                let is_current_edit = Some(&source_path_str) == current_edit_path.as_ref();

                let mut js_adjustments = match (is_current_edit, current_edit_adjustments) {
                    (true, Some(adjustments)) => adjustments,
                    _ => {
                        let metadata = crate::exif_processing::load_sidecar(&sidecar_path);
                        metadata.adjustments
                    }
                };

                hydrate_adjustments(&state, &mut js_adjustments);
                let original_path = std::path::Path::new(&source_path_str);
                let file_date = exif_processing::get_creation_date_from_path(original_path);

                let filename_template = export_settings
                    .filename_template
                    .as_deref()
                    .unwrap_or("{original_filename}_edited");

                let mut new_stem = generate_filename_from_template(
                    filename_template,
                    original_path,
                    global_index + 1,
                    total_paths,
                    &file_date,
                );

                if let Some(vc_id) = explicit_vc {
                    new_stem = format!("{}_VC{:02}", new_stem, vc_id);
                } else if appearance_count > 1 {
                    new_stem = format!("{}_VC{:02}", new_stem, appearance_count - 1);
                }

                let new_filename = format!("{}.{}", new_stem, output_format);
                let output_path = if is_explicit_file_path && total_paths == 1 {
                    output_folder_path
                } else if export_settings.preserve_folders {
                    let matched_base = base_origin_folders
                        .iter()
                        .map(std::path::Path::new)
                        .find(|b| source_path.starts_with(b));
                    if let Some(base_origin) = matched_base {
                        if let Ok(rel_path) = source_path.strip_prefix(base_origin) {
                            let rel_dir = rel_path
                                .parent()
                                .unwrap_or_else(|| std::path::Path::new(""));
                            let rel_dir_is_safe = rel_dir.components().all(|component| {
                                matches!(
                                    component,
                                    std::path::Component::Normal(_) | std::path::Component::CurDir
                                )
                            });
                            if rel_dir_is_safe {
                                let full_dir = output_folder_path.join(rel_dir);
                                if let Err(e) = std::fs::create_dir_all(&full_dir) {
                                    log::warn!("Failed to create export subdirectory: {}", e);
                                }
                                full_dir.join(&new_filename)
                            } else {
                                output_folder_path.join(&new_filename)
                            }
                        } else {
                            output_folder_path.join(&new_filename)
                        }
                    } else {
                        output_folder_path.join(&new_filename)
                    }
                } else {
                    output_folder_path.join(&new_filename)
                };

                let extension = output_format.to_lowercase();

                let result: Result<(), String> = (|| {
                    {
                        if extension == "cube" || export_settings.export_masks {
                            return Err("V3 currently exports the composited photo; separate mask-image and LUT exports are not supported.".into());
                        }
                        if !matches!(
                            extension.as_str(),
                            "png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" | "jxl" | "avif"
                        ) {
                            return Err(format!("{extension} export is not supported."));
                        }
                    }

                    // PNG, JPEG and TIFF carry a colour profile, so they are
                    // rendered in the space the editor previews in and hold
                    // exactly what was on screen. WebP, JPEG XL and AVIF are
                    // written without one, so they are rendered in sRGB,
                    // which every viewer assumes for an untagged image.
                    let carries_profile =
                        matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "tif" | "tiff");
                    let frame = if carries_profile {
                        crate::color_engine::application::render_for_output(
                            &context_clone,
                            &state,
                            &image_path_str,
                            &js_adjustments,
                            None,
                        )
                    } else {
                        crate::color_engine::application::render_file(
                            &context_clone,
                            &state,
                            &image_path_str,
                            &js_adjustments,
                            None,
                        )
                    }
                    .map_err(|e| e.to_string())?;
                    let space = frame.space;
                    let rendered = frame.export_rgba16();
                    let final_image =
                        apply_export_resize_and_watermark(rendered, &export_settings)?;
                    save_image_with_metadata(
                        &final_image,
                        &output_path,
                        &source_path_str,
                        &export_settings,
                        space,
                    )?;

                    if export_settings.preserve_timestamps {
                        set_timestamps_from_exif(Path::new(&source_path_str), &output_path);
                    }

                    Ok(())
                })();

                let current_progress = progress_counter_clone.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = app_handle_clone.emit(
                    "batch-export-progress",
                    serde_json::json!({
                        "current": current_progress,
                        "total": total_paths,
                        "path": &image_path_str
                    }),
                );

                drop(permit);
                result
            });

            join_handles.push(handle);
        }

        let mut results = Vec::new();
        for handle in join_handles {
            match handle.await {
                Ok(res) => results.push(res),
                Err(e) => results.push(Err(format!("Thread crashed: {}", e))),
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let mut error_count = 0;
        for result in results {
            if let Err(e) = result {
                error_count += 1;
                log::error!("Export error: {}", e);
                if total_paths == 1 {
                    let _ = app_handle.emit("export-error", e);
                }
            }
        }

        if error_count > 0 && total_paths > 1 {
            let _ = app_handle.emit(
                "export-complete-with-errors",
                serde_json::json!({ "errors": error_count, "total": total_paths }),
            );
        } else if error_count == 0 {
            let _ = app_handle.emit(
                "batch-export-progress",
                serde_json::json!({ "current": total_paths, "total": total_paths, "path": "" }),
            );
            let _ = app_handle.emit("export-complete", ());
        }

        *app_handle
            .state::<AppState>()
            .export_task_handle
            .lock()
            .unwrap() = None;
    });

    *state.export_task_handle.lock().unwrap() = Some(task);
    Ok(())
}

#[tauri::command]
pub fn cancel_export(state: tauri::State<AppState>) -> Result<(), String> {
    match state.export_task_handle.lock().unwrap().take() {
        Some(handle) => {
            handle.abort();
            println!("Export task cancellation requested.");
        }
        _ => {
            return Err("No export task is currently running.".to_string());
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn estimate_export_sizes(
    paths: Vec<String>,
    export_settings: ExportSettings,
    output_format: String,
    current_edit_path: Option<String>,
    current_edit_adjustments: Option<Value>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    if output_format.to_lowercase() == "cube" {
        return Ok(1_050_000 * paths.len());
    }

    if paths.is_empty() {
        return Ok(0);
    }

    let first_path = &paths[0];
    let (source_path, sidecar_path) = parse_virtual_path(first_path);
    let source_path_str = source_path.to_string_lossy().to_string();

    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let is_current_edit = Some(&source_path_str) == current_edit_path.as_ref();

    let mut candidate = if is_current_edit {
        current_edit_adjustments
            .unwrap_or_else(|| crate::exif_processing::load_sidecar(&sidecar_path).adjustments)
    } else {
        crate::exif_processing::load_sidecar(&sidecar_path).adjustments
    };
    hydrate_adjustments(&state, &mut candidate);
    // An estimate, so a preview-sized render scaled to the export's pixel
    // count: a full-resolution render here took a second per change and
    // evicted the editor's prepared picture. Rendered aside, so it touches
    // no editor cache either.
    const ESTIMATE_EDGE: u32 = 1024;
    let frame = crate::color_engine::application::render_aside(
        &context,
        &state,
        &source_path_str,
        &candidate,
        Some(ESTIMATE_EDGE),
    )
    .map_err(|e| e.to_string())?;
    let (full_w, full_h) = frame.full_size;
    let (target_w, target_h) = match &export_settings.resize {
        Some(resize) => calculate_resize_target(full_w, full_h, resize),
        None => (full_w, full_h),
    };
    let reduced = frame.export_rgba16();
    let (small_w, small_h) = reduced.dimensions();
    let bytes = encode_image_to_bytes(
        &reduced,
        &output_format,
        export_settings.jpeg_quality,
        frame.space,
    )?
    .len();
    let scale = (target_w as f64 * target_h as f64) / (small_w as f64 * small_h as f64).max(1.);
    Ok((bytes as f64 * scale) as usize * paths.len())
}
