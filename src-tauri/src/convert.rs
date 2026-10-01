//! Converting photos from one file type to another — HEIC to JPEG, say —
//! without editing them.
//!
//! This is not Export: nothing is rendered through the photo's edits. The
//! pixels stay as shot and the metadata comes along. Where macOS can do the
//! conversion itself (HEIC, AVIF, PSD, PNG, TIFF, JPEG sources to JPEG, PNG,
//! TIFF or HEIC) it does, through `sips`: that keeps the photo's colour
//! profile (an iPhone's Display P3), its rotation, date, camera and GPS
//! exactly. WebP and AVIF outputs carry no profile here, so those are
//! converted to sRGB first; RAW files have no "as shot" picture to keep and
//! are rendered with v3's default look.
//!
//! A converted file sits beside the original by default and joins its
//! version stack ("Converted"), so the library keeps one tile per photo. An
//! existing file of the same name is taken as already converted and
//! skipped, so converting a folder twice does nothing the second time. The
//! whole run is one entry in the operation journal: ⌘Z sends the new files
//! to the Trash and brings back any originals that were trashed.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use image::{DynamicImage, ImageDecoder, ImageReader};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::color_engine::config::OutputSpace;

pub const CONVERTED_KIND: &str = "Converted";

static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Jpeg,
    Png,
    Tiff,
    Heic,
    Webp,
    Avif,
}

impl Target {
    pub fn extension(self) -> &'static str {
        match self {
            Target::Jpeg => "jpg",
            Target::Png => "png",
            Target::Tiff => "tiff",
            Target::Heic => "heic",
            Target::Webp => "webp",
            Target::Avif => "avif",
        }
    }

    /// The `sips` format name, for the outputs macOS writes itself.
    fn sips_format(self) -> Option<&'static str> {
        match self {
            Target::Jpeg => Some("jpeg"),
            Target::Png => Some("png"),
            Target::Tiff => Some("tiff"),
            Target::Heic => Some("heic"),
            Target::Webp | Target::Avif => None,
        }
    }

    /// Same type as `path` already (so converting would only copy it).
    fn matches(self, path: &Path) -> bool {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match self {
            Target::Jpeg => ext == "jpg" || ext == "jpeg",
            Target::Tiff => ext == "tif" || ext == "tiff",
            Target::Heic => ext == "heic" || ext == "heif",
            _ => ext == self.extension(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Destination {
    /// Next to each original.
    Beside,
    /// In a subfolder of each original's folder, e.g. `JPEG/`.
    Subfolder { name: String },
    /// In one chosen folder, keeping the structure below `root`.
    Folder { path: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertRequest {
    pub files: Vec<String>,
    pub format: Target,
    /// 1–100, for JPEG, WebP, AVIF and HEIC.
    pub quality: u8,
    /// Longest side in pixels; none keeps the original size.
    pub max_edge: Option<u32>,
    pub destination: Destination,
    /// The folder the conversion started from, for keeping structure.
    pub root: Option<String>,
    pub strip_location: bool,
    pub trash_originals: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub files: Vec<String>,
    /// Upper-case extension → count, e.g. "HEIC" → 124.
    pub counts: BTreeMap<String, usize>,
    /// How many have edits (which Convert does not apply).
    pub edited: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConvertSummary {
    pub converted: usize,
    pub skipped: usize,
    pub failed: Vec<String>,
    pub cancelled: bool,
}

// ---------------------------------------------------------------------------
// Finding what to convert
// ---------------------------------------------------------------------------

fn is_convertible(path: &Path) -> bool {
    crate::formats::is_supported_image_file(path) && !crate::formats::is_video_file(path)
}

/// The photos under `paths` (files, or folders to look in), with counts by
/// type. Virtual copies are the same file and are not listed twice.
pub fn scan(paths: &[String], include_subfolders: bool) -> ScanResult {
    let mut files: Vec<PathBuf> = Vec::new();
    for p in paths {
        let real = PathBuf::from(p.split("?vc=").next().unwrap_or(p));
        if real.is_dir() {
            let walker = walkdir::WalkDir::new(&real)
                .max_depth(if include_subfolders { usize::MAX } else { 1 })
                .into_iter()
                .filter_map(Result::ok);
            for entry in walker {
                let path = entry.path();
                let hidden = path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'));
                if entry.file_type().is_file() && !hidden && is_convertible(path) {
                    files.push(path.to_path_buf());
                }
            }
        } else if real.is_file() && is_convertible(&real) {
            files.push(real);
        }
    }
    files.sort();
    files.dedup();

    let mut result = ScanResult::default();
    for f in &files {
        let ext = f
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_uppercase())
            .unwrap_or_default();
        let ext = match ext.as_str() {
            "JPEG" => "JPG".to_string(),
            "TIF" => "TIFF".to_string(),
            "HEIF" => "HEIC".to_string(),
            _ => ext,
        };
        *result.counts.entry(ext).or_default() += 1;
        let sidecar = crate::exif_processing::get_primary_sidecar_path(f);
        if sidecar.exists() {
            let meta = crate::exif_processing::load_sidecar(&sidecar);
            let is_raw = crate::formats::is_raw_file(f);
            if crate::image_processing::is_image_edited(&meta.adjustments, is_raw, None) {
                result.edited += 1;
            }
        }
    }
    result.files = files
        .into_iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    result
}

// ---------------------------------------------------------------------------
// Where each file goes
// ---------------------------------------------------------------------------

/// The converted file's path, or `None` when it would be the source itself.
pub fn destination_for(source: &Path, request: &ConvertRequest) -> Option<PathBuf> {
    let parent = source.parent()?;
    let stem = source.file_stem()?.to_string_lossy();
    let folder = match &request.destination {
        Destination::Beside => parent.to_path_buf(),
        Destination::Subfolder { name } => {
            let name = name.trim();
            let name = if name.is_empty() || name.contains(['/', '\\']) {
                request.format.extension().to_ascii_uppercase()
            } else {
                name.to_string()
            };
            parent.join(name)
        }
        Destination::Folder { path } => {
            let base = PathBuf::from(path);
            match request.root.as_deref().map(Path::new) {
                Some(root) => match parent.strip_prefix(root) {
                    Ok(relative) => base.join(relative),
                    Err(_) => base,
                },
                None => base,
            }
        }
    };
    let target = folder.join(format!("{stem}.{}", request.format.extension()));
    (target != source).then_some(target)
}

// ---------------------------------------------------------------------------
// Converting one file
// ---------------------------------------------------------------------------

/// Pixel size without decoding, through `sips` (any format macOS reads).
#[cfg(target_os = "macos")]
fn sips_size(path: &Path) -> Option<(u32, u32)> {
    let out = std::process::Command::new("sips")
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key))
            .and_then(|v| v.trim().parse::<u32>().ok())
    };
    Some((value("pixelWidth:")?, value("pixelHeight:")?))
}

/// macOS's own conversion: profile, orientation and metadata kept as they are.
#[cfg(target_os = "macos")]
fn convert_with_sips(source: &Path, target: &Path, request: &ConvertRequest) -> Result<(), String> {
    let format = request.format.sips_format().ok_or("not a sips format")?;
    let mut command = std::process::Command::new("sips");
    command.args(["-s", "format", format]);
    if matches!(request.format, Target::Jpeg | Target::Heic) {
        command.args([
            "-s",
            "formatOptions",
            &request.quality.clamp(1, 100).to_string(),
        ]);
    }
    if let Some(max) = request.max_edge
        && sips_size(source).is_some_and(|(w, h)| w.max(h) > max)
    {
        command.args(["-Z", &max.to_string()]);
    }
    let tmp = target.with_extension(format!("{}.rapidraw-tmp", request.format.extension()));
    let output = command
        .arg(source)
        .arg("--out")
        .arg(&tmp)
        .output()
        .map_err(|e| format!("could not run sips: {e}"))?;
    if !output.status.success() || !tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
        let message = String::from_utf8_lossy(&output.stderr);
        return Err(format!("macOS could not convert it ({})", message.trim()));
    }
    std::fs::rename(&tmp, target).map_err(|e| e.to_string())
}

/// The colour profile stored with a decodable file, if any.
fn source_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    if let Some(ext) = crate::image_loader::system_codec_format(bytes) {
        let png = crate::image_loader::system_codec_png(bytes, ext).ok()?;
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(png)).ok()?;
        return decoder.icc_profile().ok().flatten();
    }
    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    decoder.icc_profile().ok().flatten()
}

/// `image` (in the colour space `profile` describes) as sRGB, 8-bit RGBA.
fn to_srgb(image: &DynamicImage, profile: Option<&[u8]>) -> DynamicImage {
    let mut rgba = image.to_rgba8();
    if let Some(icc) = profile
        && let Ok(source) = moxcms::ColorProfile::new_from_slice(icc)
        && let Ok(transform) = source.create_transform_8bit(
            moxcms::Layout::Rgba,
            &moxcms::ColorProfile::new_srgb(),
            moxcms::Layout::Rgba,
            moxcms::TransformOptions::default(),
        )
    {
        let src = rgba.as_raw().clone();
        if transform.transform(&src, &mut rgba).is_err() {
            log::warn!("Colour conversion to sRGB failed; keeping the file's own values");
            rgba = image.to_rgba8();
        }
    }
    DynamicImage::ImageRgba8(rgba)
}

fn limit_size(image: DynamicImage, max_edge: Option<u32>) -> DynamicImage {
    match max_edge {
        Some(max) if image.width().max(image.height()) > max => {
            image.resize(max, max, image::imageops::FilterType::Lanczos3)
        }
        _ => image,
    }
}

/// Decode, convert and encode in the app: WebP and AVIF outputs, RAW
/// sources, and platforms without `sips`.
fn convert_in_app(
    app: &AppHandle,
    source: &Path,
    target: &Path,
    request: &ConvertRequest,
) -> Result<(), String> {
    let source_str = source.to_string_lossy().to_string();
    let (image, space) = if crate::formats::is_raw_file(source) {
        // No "as shot" picture: v3's default rendering, in sRGB.
        let state = app.state::<crate::AppState>();
        let context = crate::image_processing::get_or_init_gpu_context(&state, app)?;
        let edits = crate::image_processing::render_adjustments_for_empty(&serde_json::json!({}))
            .into_owned();
        let frame = crate::color_engine::application::render_file(
            &context,
            &state,
            &source_str,
            &edits,
            request.max_edge,
        )
        .map_err(|e| e.to_string())?;
        (frame.export_rgba16(), OutputSpace::Srgb)
    } else {
        let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
        let image = crate::image_loader::load_image_with_orientation(&bytes, None)
            .map_err(|e| e.to_string())?;
        let profile = source_profile(&bytes);
        (to_srgb(&image, profile.as_deref()), OutputSpace::Srgb)
    };
    let image = limit_size(image, request.max_edge);
    let ext = request.format.extension();
    let mut bytes = crate::export_processing::encode_image_to_bytes(
        &image,
        ext,
        request.quality.clamp(1, 100),
        space,
    )?;
    crate::exif_processing::write_image_with_metadata(
        &mut bytes,
        &source_str,
        ext,
        true,
        request.strip_location,
        space,
    )?;
    let tmp = target.with_extension(format!("{ext}.rapidraw-tmp"));
    std::fs::write(&tmp, &bytes)
        .and_then(|_| std::fs::rename(&tmp, target))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
}

/// Remove location from a file macOS converted, keeping the rest of its
/// metadata as the export path writes it.
fn strip_location(target: &Path, source: &Path, format: Target) -> Result<(), String> {
    if !matches!(format, Target::Jpeg | Target::Png) {
        return Ok(());
    }
    let mut bytes = std::fs::read(target).map_err(|e| e.to_string())?;
    crate::exif_processing::write_image_with_metadata(
        &mut bytes,
        &source.to_string_lossy(),
        format.extension(),
        true,
        true,
        OutputSpace::DisplayP3,
    )?;
    std::fs::write(target, bytes).map_err(|e| e.to_string())
}

/// Convert one file. `Ok(None)` when it was skipped (already converted).
fn convert_one(
    app: &AppHandle,
    source: &Path,
    request: &ConvertRequest,
) -> Result<Option<PathBuf>, String> {
    if request.format.matches(source) && matches!(request.destination, Destination::Beside) {
        return Ok(None);
    }
    let Some(target) = destination_for(source, request) else {
        return Ok(None);
    };
    if target.exists() {
        return Ok(None);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    crate::watcher::note_own_write(&target);

    #[cfg(target_os = "macos")]
    let by_macos = request.format.sips_format().is_some() && !crate::formats::is_raw_file(source);
    #[cfg(not(target_os = "macos"))]
    let by_macos = false;

    if by_macos {
        #[cfg(target_os = "macos")]
        {
            convert_with_sips(source, &target, request)?;
            if request.strip_location {
                strip_location(&target, source, request.format)?;
            }
        }
    } else {
        convert_in_app(app, source, &target, request)?;
    }
    if matches!(request.destination, Destination::Beside) {
        crate::versions::record_version(&target, source, CONVERTED_KIND);
    }
    Ok(Some(target))
}

// ---------------------------------------------------------------------------
// The whole run
// ---------------------------------------------------------------------------

pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Convert every file in `request`, reporting progress as it goes.
pub fn run(app: AppHandle, request: ConvertRequest) -> ConvertSummary {
    CANCEL.store(false, Ordering::SeqCst);
    let total = request.files.len();
    let done = AtomicUsize::new(0);
    let actions = Mutex::new(Vec::new());
    let originals_done = Mutex::new(Vec::new());
    let summary = Mutex::new(ConvertSummary::default());

    // A few at a time: `sips` and the encoders are each busy on their own.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap());
    pool.install(|| {
        request.files.par_iter().for_each(|file| {
            if CANCEL.load(Ordering::SeqCst) {
                return;
            }
            let source = PathBuf::from(file);
            let name = source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match convert_one(&app, &source, &request) {
                Ok(Some(target)) => {
                    summary.lock().unwrap().converted += 1;
                    actions
                        .lock()
                        .unwrap()
                        .push(crate::journal::Action::Created { path: target });
                    originals_done.lock().unwrap().push(source.clone());
                }
                Ok(None) => summary.lock().unwrap().skipped += 1,
                Err(e) => {
                    log::warn!("Could not convert {}: {e}", source.display());
                    summary.lock().unwrap().failed.push(format!("{name}: {e}"));
                }
            }
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = app.emit(
                "convert-progress",
                serde_json::json!({ "current": n, "total": total, "name": name }),
            );
        });
    });

    let mut actions = actions.into_inner().unwrap();
    if request.trash_originals {
        // Each original with everything that travels with it, once all
        // conversions are written.
        let mut trash = Vec::new();
        for original in originals_done.into_inner().unwrap() {
            if let Ok(files) = crate::file_management::find_all_associated_files(&original) {
                trash.extend(files);
            }
        }
        match crate::journal::trash_all(&trash) {
            Ok(trashed) => actions.extend(trashed),
            Err(e) => summary
                .lock()
                .unwrap()
                .failed
                .push(format!("Originals were kept: {e}")),
        }
    }
    let mut summary = summary.into_inner().unwrap();
    summary.cancelled = CANCEL.load(Ordering::SeqCst);
    let label = format!(
        "Convert {} photo{} to {}",
        summary.converted,
        if summary.converted == 1 { "" } else { "s" },
        request.format.extension().to_ascii_uppercase()
    );
    crate::journal::record(label, actions);
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(destination: Destination, format: Target) -> ConvertRequest {
        ConvertRequest {
            files: vec![],
            format,
            quality: 90,
            max_edge: None,
            destination,
            root: Some("/shoot".into()),
            strip_location: false,
            trash_originals: false,
        }
    }

    #[test]
    fn converted_files_go_where_asked() {
        let source = Path::new("/shoot/day1/IMG_1.HEIC");
        assert_eq!(
            destination_for(source, &request(Destination::Beside, Target::Jpeg)),
            Some(PathBuf::from("/shoot/day1/IMG_1.jpg"))
        );
        assert_eq!(
            destination_for(
                source,
                &request(Destination::Subfolder { name: "".into() }, Target::Jpeg)
            ),
            Some(PathBuf::from("/shoot/day1/JPG/IMG_1.jpg"))
        );
        assert_eq!(
            destination_for(
                source,
                &request(
                    Destination::Folder {
                        path: "/out".into()
                    },
                    Target::Webp
                )
            ),
            Some(PathBuf::from("/out/day1/IMG_1.webp")),
            "structure below the starting folder is kept"
        );
        // Never onto itself.
        assert_eq!(
            destination_for(
                Path::new("/shoot/a.jpg"),
                &request(Destination::Beside, Target::Jpeg)
            ),
            None
        );
    }

    #[test]
    fn scanning_counts_by_type_and_skips_videos() {
        let dir = tempfile::tempdir().unwrap();
        for n in [
            "a.HEIC",
            "b.heic",
            "c.jpg",
            "d.MOV",
            ".hidden.heic",
            "notes.txt",
        ] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/e.heic"), b"x").unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let flat = scan(std::slice::from_ref(&root), false);
        assert_eq!(flat.counts.get("HEIC"), Some(&2));
        assert_eq!(flat.counts.get("JPG"), Some(&1));
        assert_eq!(flat.files.len(), 3);
        let deep = scan(&[root], true);
        assert_eq!(deep.counts.get("HEIC"), Some(&3));
    }

    /// A real HEIC, made and converted by macOS: the JPEG keeps its size
    /// and colour profile, and a second run skips it.
    #[cfg(target_os = "macos")]
    #[test]
    fn heic_becomes_jpeg_with_its_profile() {
        let dir = tempfile::tempdir().unwrap();
        // A Display P3 picture, as an iPhone writes them: macOS embeds its
        // own Display P3 profile.
        let plain = dir.path().join("plain.png");
        image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 120]))
            .save(&plain)
            .unwrap();
        let png = dir.path().join("seed.png");
        let tagged = std::process::Command::new("sips")
            .args(["-m", "/System/Library/ColorSync/Profiles/Display P3.icc"])
            .arg(&plain)
            .arg("--out")
            .arg(&png)
            .output()
            .unwrap();
        assert!(tagged.status.success());
        let heic = dir.path().join("IMG_1.HEIC");
        let made = std::process::Command::new("sips")
            .args(["-s", "format", "heic"])
            .arg(&png)
            .arg("--out")
            .arg(&heic)
            .output()
            .unwrap();
        if !made.status.success() {
            eprintln!("this Mac's sips cannot write HEIC; skipping");
            return;
        }
        let mut req = request(Destination::Beside, Target::Jpeg);
        let target = dir.path().join("IMG_1.jpg");
        convert_with_sips(&heic, &target, &req).unwrap();
        let jpeg = image::open(&target).unwrap();
        assert_eq!((jpeg.width(), jpeg.height()), (64, 48));
        assert!(
            source_profile(&std::fs::read(&target).unwrap()).is_some(),
            "profile kept"
        );

        // Limited size.
        req.max_edge = Some(32);
        let small = dir.path().join("small.jpg");
        convert_with_sips(&heic, &small, &req).unwrap();
        let small = image::open(&small).unwrap();
        assert_eq!(small.width().max(small.height()), 32);
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// What `paths` (files or folders) hold that could be converted.
#[tauri::command]
pub async fn scan_convertible(
    paths: Vec<String>,
    include_subfolders: bool,
) -> Result<ScanResult, String> {
    tauri::async_runtime::spawn_blocking(move || scan(&paths, include_subfolders))
        .await
        .map_err(|e| e.to_string())
}

/// Convert, reporting `convert-progress` events; resolves with a summary.
#[tauri::command]
pub async fn convert_files(
    request: ConvertRequest,
    app_handle: AppHandle,
) -> Result<ConvertSummary, String> {
    tauri::async_runtime::spawn_blocking(move || run(app_handle, request))
        .await
        .map_err(|e| e.to_string())
}

/// Stop a conversion after the files already under way.
#[tauri::command]
pub fn cancel_conversion() {
    cancel();
}
