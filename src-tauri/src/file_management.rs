use memmap2::{Mmap, MmapOptions};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use chrono::{DateTime, Utc};
use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::AppState;
#[cfg(target_os = "android")]
use crate::android_integration::*;
use crate::app_settings::*;
use crate::exif_processing;
use crate::formats::{is_supported_image_file, is_supported_media_file, is_video_file};
use crate::gpu_processing;
use crate::image_loader;
use crate::image_processing::GpuContext;
use crate::image_processing::{ImageMetadata, auto_results_to_json, perform_auto_analysis};
use crate::preset_converter;
use crate::tagging::COLOR_TAG_PREFIX;

fn resolve_thumbnail_cache_dir(app_handle: &AppHandle) -> std::result::Result<PathBuf, String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");
    if !thumb_cache_dir.exists() {
        fs::create_dir_all(&thumb_cache_dir).map_err(|e| e.to_string())?;
    }
    Ok(thumb_cache_dir)
}

fn emit_thumbnail_cache_setup_error(app_handle: &AppHandle, path: &str, reason: &str) {
    let _ = app_handle.emit(
        "thumbnail-generation-error",
        serde_json::json!({ "path": path, "reason": reason }),
    );
}

/// Changed by hand to make every cached thumbnail stale.
const THUMBNAIL_CACHE_SALT: &[u8] = b"thumbnails-2026-11-01-keyed-by-renderer\0";

/// The renderer's version, as far as thumbnails go: the v3 engine's own
/// digest and the other sources that change how a thumbnail looks. A
/// thumbnail made by an older build is not served by a newer one.
fn thumbnail_renderer_digest() -> &'static [u8; 32] {
    static DIGEST: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut hash = blake3::Hasher::new();
        hash.update(crate::color_engine::contract::implementation_digest().as_bytes());
        hash.update(crate::color_engine::contract::REVISION.as_bytes());
        for source in [
            include_str!("color_engine/raw_look.rs"),
            include_str!("color_engine/raw_look_table.rs"),
            include_str!("color_engine/rcd.rs"),
            include_str!("color_engine/config.rs"),
            include_str!("color_engine/selection.rs"),
            include_str!("color_engine/dehaze_table.rs"),
            include_str!("color_engine/detail_table.rs"),
            include_str!("color_engine/sharpen_table.rs"),
            include_str!("color_engine/tone_zones_table.rs"),
            include_str!("adjustment_utils.rs"),
            include_str!("flat_field.rs"),
            include_str!("heal_blend.rs"),
            include_str!("lens_correction.rs"),
            include_str!("mask_generation.rs"),
            include_str!("smart_preview.rs"),
        ] {
            hash.update(&(source.len() as u64).to_le_bytes());
            hash.update(source.as_bytes());
        }
        *hash.finalize().as_bytes()
    })
}

/// What decides how every thumbnail looks besides its photo and its edit:
/// the renderer, the colour transforms installed, the size thumbnails are
/// made at and the tonemapper override.
#[derive(Clone)]
pub(crate) struct ThumbnailKeys {
    base: [u8; 32],
    tonemapper_raw: Option<u32>,
    tonemapper_other: Option<u32>,
}

impl ThumbnailKeys {
    pub(crate) fn new(state: &AppState, settings: &AppSettings) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(THUMBNAIL_CACHE_SALT);
        hash.update(thumbnail_renderer_digest());
        hash.update(&thumbnail_width(settings).to_le_bytes());
        let transforms = [
            &state.input_transform,
            &state.input_transform_p3,
            &state.output_transform,
            &state.output_transform_p3,
        ];
        for (slot, transform) in transforms.into_iter().enumerate() {
            hash.update(&[slot as u8]);
            let path = transform.lock().ok().and_then(|p| p.clone());
            if let Some(path) = path {
                // A replaced cube is a new file, or at least a new size or
                // time: cheaper than hashing it for every thumbnail.
                hash.update(path.to_string_lossy().as_bytes());
                hash.update(&file_identity(&path).unwrap_or_default());
            }
        }
        Self {
            base: *hash.finalize().as_bytes(),
            tonemapper_raw: crate::image_processing::resolve_tonemapper_override(settings, true),
            tonemapper_other: crate::image_processing::resolve_tonemapper_override(settings, false),
        }
    }

    /// For callers without the settings at hand: read again at most every
    /// couple of seconds, as a screenful of cache lookups would otherwise
    /// each read the settings file.
    pub(crate) fn current(app_handle: &AppHandle) -> Self {
        use std::sync::Mutex;
        use std::time::{Duration, Instant};
        static LAST: Mutex<Option<(Instant, ThumbnailKeys)>> = Mutex::new(None);
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, keys)) = last.as_ref()
            && at.elapsed() < Duration::from_secs(2)
        {
            return keys.clone();
        }
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let keys = Self::new(&app_handle.state::<AppState>(), &settings);
        *last = Some((Instant::now(), keys.clone()));
        keys
    }

    /// The cache key of `path_str`'s thumbnail with these edits (the
    /// sidecar's adjustments as bytes). None when the photo is not there.
    fn key(&self, path_str: &str, adjustments_bytes: &[u8]) -> Option<String> {
        let (source_path, _) = parse_virtual_path(path_str);
        let copy_id = path_str.rsplit_once("?vc=").map(|(_, id)| id);
        let tonemapper = if crate::formats::is_raw_file(path_str) {
            self.tonemapper_raw
        } else {
            self.tonemapper_other
        };
        let mut hash = blake3::Hasher::new();
        hash.update(&self.base);
        hash.update(&file_identity(&source_path)?);
        hash.update(copy_id.unwrap_or("").as_bytes());
        hash.update(&[0, tonemapper.map_or(0xff, |t| t as u8)]);
        hash.update(adjustments_bytes);
        Some(hash.finalize().to_hex().to_string())
    }
}

/// A file's identity for the thumbnail cache, without reading it: its
/// size, modification and creation times and (on the Mac) its file number.
/// These survive renaming or moving it (on one disk), so its thumbnail is
/// kept; a file rewritten in any way changes them. Its path and disk are
/// left out on purpose: a renamed folder, or a drive mounted again, is not
/// a different photo.
fn file_identity(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::metadata(path).ok()?;
    let nanos = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    let mut id = Vec::with_capacity(48);
    id.extend_from_slice(&meta.len().to_le_bytes());
    id.extend_from_slice(&nanos(meta.modified()).to_le_bytes());
    id.extend_from_slice(&nanos(meta.created()).to_le_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        id.extend_from_slice(&meta.ino().to_le_bytes());
    }
    #[cfg(not(unix))]
    id.extend_from_slice(path.to_string_lossy().as_bytes());
    Some(id)
}

fn thumbnail_width(settings: &AppSettings) -> u32 {
    settings.thumbnail_resolution.unwrap_or(720)
}

/// The thumbnail cache's limits: least recently shown thumbnails go first
/// once it holds more than this, and any not shown for this long.
const THUMBNAIL_CACHE_BYTES: u64 = 2 << 30;
const THUMBNAIL_CACHE_MAX_AGE: std::time::Duration =
    std::time::Duration::from_secs(90 * 24 * 60 * 60);

/// A cached thumbnail being shown: its time is the cache's "last used",
/// brought forward at most once a day, so showing it writes nothing more.
fn touch_cached_thumbnail(path: &Path) {
    let now = std::time::SystemTime::now();
    let stale = fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| now.duration_since(t).unwrap_or_default().as_secs() > 24 * 60 * 60);
    if stale && let Ok(file) = fs::File::options().write(true).open(path) {
        let _ = file.set_modified(now);
    }
}

/// Remove the least recently used thumbnails until the cache is well under
/// `limit` bytes, and any not used within `max_age`. Returns what it
/// removed (files, bytes).
fn prune_thumbnail_cache(
    dir: &Path,
    limit: u64,
    max_age: std::time::Duration,
    now: std::time::SystemTime,
) -> (usize, u64) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let used = meta.modified().unwrap_or(now);
        let age = now.duration_since(used).unwrap_or_default();
        // A write that never finished (see write_atomically).
        if name.starts_with('.') {
            if name.ends_with(".tmp") && age.as_secs() > 60 * 60 {
                let _ = fs::remove_file(entry.path());
            }
            continue;
        }
        if name.ends_with(".jpg") {
            files.push((used, meta.len(), entry.path()));
        }
    }
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    files.sort_by_key(|(used, _, _)| *used);
    // Once over, well under, so it is not pruned again a few thumbnails
    // later.
    let over = total > limit;
    let target = limit / 10 * 8;
    let (mut removed, mut freed) = (0, 0);
    for (used, len, path) in files {
        let too_old = now.duration_since(used).unwrap_or_default() > max_age;
        if !(too_old || (over && total > target)) {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
            removed += 1;
            freed += len;
        }
    }
    (removed, freed)
}

static THUMBNAIL_CACHE_PRUNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static THUMBNAIL_CACHE_WRITES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Prune the thumbnail cache in the background, unless that is already
/// happening.
fn prune_thumbnail_cache_soon(dir: PathBuf) {
    use std::sync::atomic::Ordering;
    if THUMBNAIL_CACHE_PRUNING.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        let (removed, freed) = prune_thumbnail_cache(
            &dir,
            THUMBNAIL_CACHE_BYTES,
            THUMBNAIL_CACHE_MAX_AGE,
            std::time::SystemTime::now(),
        );
        if removed > 0 {
            log::info!(
                "Thumbnail cache: removed {removed} unused thumbnails ({} MB)",
                freed >> 20
            );
        }
        THUMBNAIL_CACHE_PRUNING.store(false, Ordering::SeqCst);
    });
}

/// Save a thumbnail to the cache: whole or not at all, so a worker reading
/// it meanwhile never gets half of one. Every few hundred, the cache is
/// pruned.
fn write_cached_thumbnail(cache_path: &Path, data: &[u8]) {
    if let Err(e) = crate::exif_processing::write_atomically(cache_path, data, false) {
        log::warn!("Could not cache thumbnail {}: {e}", cache_path.display());
        return;
    }
    let writes = THUMBNAIL_CACHE_WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if writes % 500 == 499
        && let Some(dir) = cache_path.parent()
    {
        prune_thumbnail_cache_soon(dir.to_path_buf());
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub adjustments: Value,
    #[serde(rename = "includeMasks", skip_serializing_if = "Option::is_none")]
    pub include_masks: Option<bool>,
    #[serde(
        rename = "includeCropTransform",
        skip_serializing_if = "Option::is_none"
    )]
    pub include_crop_transform: Option<bool>,
    #[serde(rename = "presetType", skip_serializing_if = "Option::is_none")]
    pub preset_type: Option<String>,
}

#[derive(Serialize)]
struct ExportPresetFile<'a> {
    creator: &'a str,
    presets: &'a [PresetItem],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PresetFolder {
    pub id: String,
    pub name: String,
    pub children: Vec<Preset>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum PresetItem {
    Preset(Preset),
    Folder(PresetFolder),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PresetFile {
    pub presets: Vec<PresetItem>,
}

#[derive(Debug)]
pub enum ReadFileError {
    Io(std::io::Error),
    Locked,
    Empty,
    NotFound,
    Invalid,
}

impl fmt::Display for ReadFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadFileError::Io(err) => write!(f, "IO error: {}", err),
            ReadFileError::Locked => write!(f, "File is locked"),
            ReadFileError::Empty => write!(f, "File is empty"),
            ReadFileError::NotFound => write!(f, "File not found"),
            ReadFileError::Invalid => write!(f, "Invalid file"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageFile {
    path: String,
    modified: u64,
    is_edited: bool,
    rating: u8,
    tags: Option<Vec<String>>,
    exif: Option<HashMap<String, String>>,
    is_virtual_copy: bool,
    /// The photo this file is a version (or saved frame) of, as a full
    /// path, when that photo is known. See `versions.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    derived_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    derived_kind: Option<String>,
    /// Culling flag: "pick" or "reject".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flag: Option<String>,
    /// The photo is only in a cloud service, not on this Mac (see
    /// cloud_files): shown with a cloud badge, and not read until opened.
    #[serde(default)]
    cloud_only: bool,
    /// A cloud-only photo whose smart preview has been made (the file
    /// exists; its contents are not read).
    #[serde(default)]
    has_preview: bool,
}

/// Whether a cloud-only photo's smart preview exists (only the file's
/// presence: reading a cloud file would download it).
fn cloud_preview_exists(path: &str) -> bool {
    let source = path.split("?vc=").next().unwrap_or(path);
    crate::cloud_files::is_cloud_only_str(source)
        && crate::smart_preview::path_for(Path::new(source)).exists()
}

/// What a folder's cloud-only photos come to, for the choice of what to
/// keep: how many, how much they would download, how many have previews
/// already. Reads only file metadata.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudFolderSummary {
    photos: usize,
    bytes: u64,
    with_previews: usize,
}

#[tauri::command(async)]
pub fn cloud_folder_summary(path: String) -> Result<CloudFolderSummary, String> {
    let mut summary = CloudFolderSummary {
        photos: 0,
        bytes: 0,
        with_previews: 0,
    };
    for entry in fs::read_dir(&path).map_err(|e| e.to_string())?.flatten() {
        let file = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Photographs only: videos get no preview or thumbnail built, so
        // they are neither waited for nor downloaded by any choice.
        if !is_supported_image_file(&name) || !crate::cloud_files::is_cloud_only(&file) {
            continue;
        }
        summary.photos += 1;
        summary.bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
        if crate::smart_preview::path_for(&file).exists() {
            summary.with_previews += 1;
        }
    }
    Ok(summary)
}

/// Fill in each listed file's original: from its sidecar where recorded,
/// otherwise by name among the other listed files.
fn link_versions(list: &mut [ImageFile]) {
    let real_paths: Vec<PathBuf> = list
        .iter()
        .filter(|f| !f.is_virtual_copy)
        .map(|f| PathBuf::from(&f.path))
        .collect();
    let by_name = crate::versions::originals_by_name(&real_paths);
    for file in list.iter_mut() {
        let real = PathBuf::from(file.path.split("?vc=").next().unwrap_or(&file.path));
        if let Some(name) = file.derived_from.take() {
            // Recorded as a file name beside the version.
            let original = real.with_file_name(name);
            if original != real {
                file.derived_from = Some(original.to_string_lossy().into_owned());
            }
            continue;
        }
        if let Some((original, kind)) = by_name.get(&real) {
            file.derived_from = Some(original.to_string_lossy().into_owned());
            file.derived_kind = Some(kind.to_string());
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ImportSettings {
    pub filename_template: String,
    pub organize_by_date: bool,
    pub date_folder_format: String,
    pub delete_after_import: bool,
}

pub fn parse_virtual_path(virtual_path: &str) -> (PathBuf, PathBuf) {
    let (source_path_str, copy_id) = if let Some((base, id)) = virtual_path.rsplit_once("?vc=") {
        (base.to_string(), Some(id.to_string()))
    } else {
        (virtual_path.to_string(), None)
    };

    let source_path = PathBuf::from(source_path_str);

    let sidecar_filename = if let Some(id) = copy_id {
        format!(
            "{}.{}.dri",
            source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            &id
        )
    } else {
        format!(
            "{}.dri",
            source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    };

    let sidecar_path = source_path.with_file_name(sidecar_filename);
    (source_path, sidecar_path)
}

#[tauri::command]
pub async fn read_exif_for_paths(
    paths: Vec<String>,
) -> Result<HashMap<String, HashMap<String, String>>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let exif_data: HashMap<String, HashMap<String, String>> = paths
            .par_iter()
            .filter_map(|virtual_path| {
                let (source_path, _) = parse_virtual_path(virtual_path);
                let source_path_str = source_path.to_string_lossy().to_string();

                let map = if let Some(known) = crate::exif_processing::known_exif(&source_path) {
                    known
                } else if crate::cloud_files::is_cloud_only(&source_path) {
                    // Reading it would download the whole photo; its smart
                    // preview, when one is here, carries its EXIF.
                    let preview = crate::smart_preview::path_for(&source_path);
                    if preview.exists() && !crate::cloud_files::is_dataless(&preview) {
                        crate::smart_preview::exif(&source_path).unwrap_or_default()
                    } else {
                        HashMap::new()
                    }
                } else if let Ok(mmap) = read_file_mapped(&source_path) {
                    crate::exif_processing::read_exif_data(&source_path_str, &mmap)
                } else if let Ok(bytes) = fs::read(&source_path) {
                    crate::exif_processing::read_exif_data(&source_path_str, &bytes)
                } else {
                    HashMap::new()
                };

                if map.is_empty() {
                    None
                } else {
                    Some((virtual_path.clone(), map))
                }
            })
            .collect();

        Ok(exif_data)
    })
    .await
    .unwrap_or_else(|e| Err(format!("Task failed: {}", e)))
}

#[tauri::command]
pub async fn update_exif_fields(
    paths: Vec<String>,
    updates: HashMap<String, String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    let xmp = settings
        .enable_xmp_sync
        .unwrap_or(true)
        .then(|| settings.create_xmp_if_missing.unwrap_or(false));
    tauri::async_runtime::spawn_blocking(move || {
        let failures: Vec<String> = paths
            .par_iter()
            .filter_map(|path| {
                let original_path = Path::new(&path);
                let primary_path = crate::exif_processing::get_primary_sidecar_path(original_path);
                crate::exif_processing::modify_sidecar(&primary_path, |metadata| {
                    let mut exif_data = metadata.exif.take().unwrap_or_else(|| {
                        if let Some(existing) =
                            crate::exif_processing::read_exif_sidecar(original_path)
                        {
                            existing
                        } else if let Ok(mmap) = read_file_mapped(original_path) {
                            crate::exif_processing::read_exif_data_from_bytes(path, &mmap)
                        } else if let Ok(bytes) = fs::read(original_path) {
                            crate::exif_processing::read_exif_data_from_bytes(path, &bytes)
                        } else {
                            HashMap::new()
                        }
                    });

                    for (k, v) in &updates {
                        let trimmed = v.trim();
                        if trimmed.is_empty() {
                            exif_data.remove(k);
                        } else {
                            exif_data.insert(k.clone(), trimmed.to_string());
                        }
                    }

                    metadata.exif = Some(exif_data);
                    // Caption, author and copyright are shared through XMP.
                    if let Some(create_if_missing) = xmp {
                        crate::xmp::push(path, metadata, create_if_missing);
                    }
                    true
                })
                .err()
            })
            .collect();
        sidecar_failures(failures)
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

/// A batch's result: fine, or the sidecars that could not be updated
/// (left exactly as they were).
fn sidecar_failures(failures: Vec<String>) -> Result<(), String> {
    match failures.as_slice() {
        [] => Ok(()),
        [only] => Err(only.clone()),
        [first, ..] => Err(format!(
            "{} photos were not changed. {first}",
            failures.len()
        )),
    }
}

/// A photo's sidecar for a listing, with another program's XMP changes
/// taken in (and saved) when `pull_xmp`. One that cannot be read shows as
/// unedited and is left alone on disk.
/// `names`: the photo's folder's file names, when the whole folder is
/// being listed (see `xmp::FolderNames`).
fn listed_sidecar(
    path_str: &str,
    sidecar_path: &Path,
    pull_xmp: bool,
    names: Option<&crate::xmp::FolderNames>,
) -> ImageMetadata {
    if !pull_xmp {
        return crate::exif_processing::load_sidecar(sidecar_path);
    }
    crate::exif_processing::modify_sidecar(sidecar_path, |metadata| {
        crate::xmp::pull_listed(path_str, metadata, names)
    })
    .unwrap_or_else(|e| {
        log::warn!("{e}");
        ImageMetadata::default()
    })
}

/// A sidecar file's name split into the photo it belongs to and, for a
/// virtual copy (`IMG.CR3.a1b2c3.dri`), the copy's id. `None` when the
/// name is not a sidecar's.
fn split_sidecar_name(file_name: &str) -> Option<(&str, Option<&str>)> {
    let base = file_name.strip_suffix(".dri")?;
    if base.is_empty() {
        return None;
    }
    // A copy id is six hex digits after a dot, with a photo name before.
    if let Some((photo, id)) = base.rsplit_once('.')
        && !photo.is_empty()
        && id.len() == 6
        && id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    {
        return Some((photo, Some(id)));
    }
    Some((base, None))
}

/// Names a listing leaves out: dot files and folders (the Mac's own, and
/// the `._IMG.ARW` AppleDouble files a card or network share gets), as the
/// folder watcher does.
fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// The listing's entries for one photo: the photo itself and each of its
/// virtual copies (`sidecars`, as gathered by the listing).
fn list_photo(
    path_buf: &Path,
    file_name: &str,
    sidecars: Vec<Option<String>>,
    settings: &AppSettings,
    names: Option<&crate::xmp::FolderNames>,
) -> Vec<ImageFile> {
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
    let path_str = path_buf.to_string_lossy().into_owned();
    let modified = fs::metadata(path_buf)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let is_raw = crate::formats::is_raw_file(&path_str);
    let tm_override = crate::image_processing::resolve_tonemapper_override(settings, is_raw);

    let mut file_results = Vec::with_capacity(sidecars.len());
    for copy_id_opt in sidecars {
        let (virtual_path, is_virtual_copy, sidecar_filename) = match copy_id_opt {
            Some(id) => (
                format!("{}?vc={}", path_str, id),
                true,
                format!("{}.{}.dri", file_name, id),
            ),
            None => (path_str.clone(), false, format!("{}.dri", file_name)),
        };

        let sidecar_path = path_buf.with_file_name(sidecar_filename);
        let metadata = listed_sidecar(
            &path_str,
            &sidecar_path,
            enable_xmp_sync && !is_virtual_copy,
            names,
        );
        let is_edited =
            crate::image_processing::is_image_edited(&metadata.adjustments, is_raw, tm_override);

        file_results.push(ImageFile {
            cloud_only: crate::cloud_files::is_cloud_only_str(&virtual_path),
            has_preview: cloud_preview_exists(&virtual_path),
            path: virtual_path,
            modified,
            is_edited,
            tags: metadata.tags,
            // The EXIF the sidecar holds, read here anyway: the app then
            // asks only for the photos without (read_exif_for_paths).
            exif: metadata.exif,
            is_virtual_copy,
            rating: metadata.rating,
            derived_from: metadata.derived_from,
            derived_kind: metadata.derived_kind,
            flag: metadata.flag,
        });
    }
    file_results
}

// Off the main thread: reading a folder kept by a cloud service can wait on
// the server, and the window must not freeze meanwhile.
#[tauri::command(async)]
pub fn list_images_in_dir(
    path: String,
    app_handle: AppHandle,
    build_cloud: Option<bool>,
) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let mut result_list = list_folder(Path::new(&path), &settings)?;
    link_versions(&mut result_list);
    // Photos here that are only in the cloud get their previews (or
    // thumbnails) built in the background, this folder first — when the
    // folder is being opened in the library (`build_cloud`). Home and the
    // import dialogs list folders without that: Home's photos are built as
    // they come on screen (cloud_builder::request).
    if !build_cloud.unwrap_or(false) {
        return Ok(result_list);
    }
    let cloud: Vec<PathBuf> = result_list
        .iter()
        .filter(|f| f.cloud_only && !f.is_virtual_copy)
        .map(|f| PathBuf::from(&f.path))
        .collect();
    if !cloud.is_empty() {
        crate::cloud_builder::enqueue(&app_handle, cloud);
    }
    Ok(result_list)
}

/// One folder's photos and their copies, unlinked (see `link_versions`).
fn list_folder(path: &Path, settings: &AppSettings) -> Result<Vec<ImageFile>, String> {
    let entries = fs::read_dir(path).map_err(|e| e.to_string())?;
    let mut images = Vec::new();
    let mut sidecars_by_filename: HashMap<String, Vec<Option<String>>> = HashMap::new();
    let mut names = crate::xmp::FolderNames::default();

    for entry in entries.filter_map(Result::ok) {
        let entry_path = entry.path();
        let file_name = entry
            .file_name()
            .into_string()
            .unwrap_or_else(|os| os.to_string_lossy().into_owned());
        names.insert(&file_name);

        if is_hidden_name(&file_name) {
            continue;
        }
        if file_name.ends_with(".dri") {
            // The photo itself is always listed (`None`), with or without a
            // sidecar of its own; each copy's sidecar adds one entry.
            if let Some((source_filename, Some(copy_id))) = split_sidecar_name(&file_name) {
                sidecars_by_filename
                    .entry(source_filename.to_string())
                    .or_insert_with(|| vec![None])
                    .push(Some(copy_id.to_string()));
            }
        } else if is_supported_media_file(&file_name) {
            images.push((file_name, entry_path));
        }
    }

    let tasks: Vec<_> = images
        .into_iter()
        .map(|(file_name, path_buf)| {
            let sidecars = sidecars_by_filename
                .remove(&file_name)
                .unwrap_or_else(|| vec![None]);
            (file_name, path_buf, sidecars)
        })
        .collect();

    Ok(tasks
        .into_par_iter()
        .flat_map(|(file_name, path_buf, sidecars)| {
            list_photo(&path_buf, &file_name, sidecars, settings, Some(&names))
        })
        .collect())
}

#[tauri::command(async)]
pub fn list_images_recursive(
    path: String,
    app_handle: AppHandle,
) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    Ok(list_tree(Path::new(&path), &settings))
}

/// The photos (and their copies) in a folder and all those below it.
fn list_tree(root_path: &Path, settings: &AppSettings) -> Vec<ImageFile> {
    let mut images = Vec::new();

    let mut sidecars_by_path: HashMap<PathBuf, Vec<Option<String>>> = HashMap::new();
    let mut names_by_folder: HashMap<PathBuf, crate::xmp::FolderNames> = HashMap::new();

    // Hidden folders (a library's own, a sync client's) are not walked.
    let walk = WalkDir::new(root_path)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden_name(&e.file_name().to_string_lossy()));
    for entry in walk.filter_map(Result::ok) {
        let entry_path = entry.path();
        let file_name = entry_path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(parent) = entry_path.parent()
            && entry.depth() > 0
        {
            names_by_folder
                .entry(parent.to_path_buf())
                .or_default()
                .insert(&file_name);
        }
        if !entry_path.is_file() || is_hidden_name(&file_name) {
            continue;
        }

        if file_name.ends_with(".dri") {
            // As in list_images_in_dir: the photo itself, then its copies.
            if let Some((source_filename, Some(copy_id))) = split_sidecar_name(&file_name)
                && let Some(parent) = entry_path.parent()
            {
                sidecars_by_path
                    .entry(parent.join(source_filename))
                    .or_insert_with(|| vec![None])
                    .push(Some(copy_id.to_string()));
            }
        } else if is_supported_media_file(entry_path.to_string_lossy().as_ref()) {
            images.push(entry_path.to_path_buf());
        }
    }

    let tasks: Vec<_> = images
        .into_iter()
        .map(|path_buf| {
            let sidecars = sidecars_by_path
                .remove(&path_buf)
                .unwrap_or_else(|| vec![None]);
            let file_name = path_buf
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (file_name, path_buf, sidecars)
        })
        .collect();

    let mut result_list: Vec<ImageFile> = tasks
        .into_par_iter()
        .flat_map(|(file_name, path_buf, sidecars)| {
            let names = path_buf.parent().and_then(|p| names_by_folder.get(p));
            list_photo(&path_buf, &file_name, sidecars, settings, names)
        })
        .collect();

    link_versions(&mut result_list);
    result_list
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AlbumItem {
    Album {
        id: String,
        name: String,
        icon: Option<String>,
        images: Vec<String>,
    },
    Group {
        id: String,
        name: String,
        icon: Option<String>,
        children: Vec<AlbumItem>,
    },
}

fn get_albums_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let albums_dir = data_dir.join("albums");
    if !albums_dir.exists() {
        fs::create_dir_all(&albums_dir).map_err(|e| e.to_string())?;
    }
    Ok(albums_dir.join("albums.json"))
}

pub fn sort_album_tree(items: &mut [AlbumItem]) {
    items.sort_by(|a, b| {
        let get_sort_key = |item: &AlbumItem| match item {
            AlbumItem::Group { name, .. } => (0, name.to_lowercase()),
            AlbumItem::Album { name, .. } => (1, name.to_lowercase()),
        };

        let key_a = get_sort_key(a);
        let key_b = get_sort_key(b);

        key_a.cmp(&key_b)
    });

    for item in items.iter_mut() {
        if let AlbumItem::Group { children, .. } = item {
            sort_album_tree(children);
        }
    }
}

#[tauri::command]
pub fn get_albums(app_handle: AppHandle) -> Result<Vec<AlbumItem>, String> {
    let path = get_albums_path(&app_handle)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut items: Vec<AlbumItem> = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    sort_album_tree(&mut items);
    Ok(items)
}

#[tauri::command]
pub fn save_albums(mut tree: Vec<AlbumItem>, app_handle: AppHandle) -> Result<(), String> {
    let path = get_albums_path(&app_handle)?;
    sort_album_tree(&mut tree);
    let json_string = serde_json::to_string_pretty(&tree).map_err(|e| e.to_string())?;
    fs::write(path, json_string).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_to_album(
    album_id: String,
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let mut tree = get_albums(app_handle.clone())?;

    fn add_recursive(items: &mut [AlbumItem], target_id: &str, paths_to_add: &Vec<String>) -> bool {
        for item in items.iter_mut() {
            #[allow(clippy::collapsible_match)]
            match item {
                AlbumItem::Album { id, images, .. } if id == target_id => {
                    for p in paths_to_add {
                        if !images.contains(p) {
                            images.push(p.clone());
                        }
                    }
                    return true;
                }
                AlbumItem::Group { children, .. } => {
                    if add_recursive(children, target_id, paths_to_add) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if add_recursive(&mut tree, &album_id, &paths) {
        save_albums(tree, app_handle)?;
    }
    Ok(())
}

fn sync_album_path_changes(
    app_handle: &AppHandle,
    renames: Option<&HashMap<String, String>>,
    deletions: Option<&HashSet<String>>,
    folder_rename: Option<(&str, &str)>,
) {
    if let Ok(mut tree) = get_albums(app_handle.clone()) {
        let mut changed = false;

        fn process_nodes(
            nodes: &mut [AlbumItem],
            renames: Option<&HashMap<String, String>>,
            deletions: Option<&HashSet<String>>,
            folder_rename: Option<(&str, &str)>,
            changed: &mut bool,
        ) {
            for node in nodes.iter_mut() {
                match node {
                    AlbumItem::Album { images, .. } => {
                        let mut new_images = Vec::new();

                        for img in images.drain(..) {
                            let mut current_img = img;

                            if let Some((old_folder, new_folder)) = folder_rename {
                                let img_path = Path::new(&current_img);
                                let old_path = Path::new(old_folder);
                                if let Ok(stripped) = img_path.strip_prefix(old_path) {
                                    let new_img_path = Path::new(new_folder).join(stripped);
                                    current_img = new_img_path.to_string_lossy().into_owned();
                                    *changed = true;
                                }
                            }

                            if let Some(r) = renames {
                                if let Some(new_path) = r.get(&current_img) {
                                    current_img = new_path.clone();
                                    *changed = true;
                                } else if let Some((base_path, vc_id)) =
                                    current_img.rsplit_once("?vc=")
                                    && let Some(new_base) = r.get(base_path)
                                {
                                    current_img = format!("{}?vc={}", new_base, vc_id);
                                    *changed = true;
                                }
                            }

                            let mut is_deleted = false;
                            if let Some(d) = deletions {
                                if d.contains(&current_img) {
                                    is_deleted = true;
                                } else {
                                    let img_path = Path::new(&current_img);
                                    for del_path_str in d {
                                        let del_path = Path::new(del_path_str);
                                        if img_path.starts_with(del_path) {
                                            is_deleted = true;
                                            break;
                                        }

                                        if let Some((base_path, _)) =
                                            current_img.rsplit_once("?vc=")
                                            && base_path == del_path_str
                                        {
                                            is_deleted = true;
                                            break;
                                        }
                                    }
                                }
                            }

                            if !is_deleted {
                                new_images.push(current_img);
                            } else {
                                *changed = true;
                            }
                        }
                        *images = new_images;
                    }
                    AlbumItem::Group { children, .. } => {
                        process_nodes(children, renames, deletions, folder_rename, changed);
                    }
                }
            }
        }

        process_nodes(&mut tree, renames, deletions, folder_rename, &mut changed);

        if changed {
            let _ = save_albums(tree, app_handle.clone());
        }
    }
}

#[tauri::command]
pub fn get_album_images(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);

    let result_list: Vec<ImageFile> = paths
        .into_par_iter()
        .filter_map(|virtual_path| {
            let (source_path, sidecar_path) = parse_virtual_path(&virtual_path);
            if !source_path.exists() {
                return None;
            }

            let modified = fs::metadata(&source_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let is_virtual_copy = virtual_path.contains("?vc=");

            let (is_edited, tags, rating, derived_from, derived_kind, flag) = {
                let metadata = listed_sidecar(&virtual_path, &sidecar_path, enable_xmp_sync, None);

                let is_raw = crate::formats::is_raw_file(&source_path);
                let tm_override =
                    crate::image_processing::resolve_tonemapper_override(&settings, is_raw);
                let edited = crate::image_processing::is_image_edited(
                    &metadata.adjustments,
                    is_raw,
                    tm_override,
                );
                (
                    edited,
                    metadata.tags,
                    metadata.rating,
                    metadata.derived_from,
                    metadata.derived_kind,
                    metadata.flag,
                )
            };

            Some(ImageFile {
                cloud_only: crate::cloud_files::is_cloud_only_str(&virtual_path),
                has_preview: cloud_preview_exists(&virtual_path),
                path: virtual_path,
                modified,
                is_edited,
                tags,
                exif: None,
                is_virtual_copy,
                rating,
                derived_from,
                derived_kind,
                flag,
            })
        })
        .collect();

    let mut result_list = result_list;
    link_versions(&mut result_list);
    Ok(result_list)
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    pub name: String,
    pub path: String,
    pub children: Vec<FolderNode>,
    pub is_dir: bool,
    pub image_count: usize,
    pub has_subdirs: bool,
    pub modified: u64,
    pub created: u64,
}

fn has_subdirs(path: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(Result::ok) {
            if let Ok(file_type) = entry.file_type()
                && file_type.is_dir()
            {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with('.') {
                    return true;
                }
            }
        }
    }
    false
}

/// How long one folder tree may spend counting the photos in folders it
/// does not show open, and how deep it looks. A tree is refreshed often
/// (each change on disk), and its root may be a whole cloud drive.
const FOLDER_COUNT_BUDGET: std::time::Duration = std::time::Duration::from_millis(1500);
const FOLDER_COUNT_DEPTH: usize = 12;

/// What a folder held when last listed for the counts: its own photos and
/// its subfolders, with the folder's modification time then. Adding,
/// removing or renaming anything in a folder changes that time, so a
/// recount lists only folders that changed; the others cost one look at
/// their time. Remembered a few minutes at most, for disks (some cloud
/// services) that do not keep folder times.
struct FolderCount {
    modified: Option<std::time::SystemTime>,
    listed: std::time::Instant,
    own: usize,
    subfolders: Vec<PathBuf>,
}

static FOLDER_COUNTS: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, FolderCount>>> =
    std::sync::LazyLock::new(Default::default);

/// The photos in `dir` and the folders below it (hidden ones left out, as
/// the tree leaves them out), for a folder the tree does not scan. Stops
/// looking deeper at `FOLDER_COUNT_DEPTH`, and listing folders at
/// `until`: what is not known by then is not counted.
fn unscanned_image_count(dir: &Path, depth: usize, until: std::time::Instant) -> usize {
    let modified = fs::metadata(dir).and_then(|m| m.modified()).ok();
    let remembered = FOLDER_COUNTS.lock().ok().and_then(|counts| {
        counts.get(dir).and_then(|c| {
            (c.modified == modified
                && modified.is_some()
                && c.listed.elapsed() < std::time::Duration::from_secs(300))
            .then(|| (c.own, c.subfolders.clone()))
        })
    });
    let (own, subfolders) = match remembered {
        Some(known) => known,
        None => {
            if std::time::Instant::now() >= until {
                return 0;
            }
            let mut own = 0;
            let mut subfolders = Vec::new();
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = entry.file_name();
                if is_hidden_name(&name.to_string_lossy()) {
                    continue;
                }
                match entry.file_type() {
                    Ok(t) if t.is_dir() => subfolders.push(entry.path()),
                    Ok(t) if t.is_file() && crate::formats::is_supported_image_file(&name) => {
                        own += 1
                    }
                    _ => {}
                }
            }
            if let Ok(mut counts) = FOLDER_COUNTS.lock() {
                if counts.len() > 50_000 {
                    counts.clear();
                }
                counts.insert(
                    dir.to_path_buf(),
                    FolderCount {
                        modified,
                        listed: std::time::Instant::now(),
                        own,
                        subfolders: subfolders.clone(),
                    },
                );
            }
            (own, subfolders)
        }
    };
    if depth >= FOLDER_COUNT_DEPTH {
        return own;
    }
    own + subfolders
        .iter()
        .map(|sub| unscanned_image_count(sub, depth + 1, until))
        .sum::<usize>()
}

/// `check_unscanned`: whether folders not scanned are each looked into to
/// say whether they have subfolders. Home leaves it off: it does not show
/// that, and in a cloud service each look makes it list the folder from
/// the server. `counts_until`: when counting the photos in folders not
/// scanned stops for this tree (see `unscanned_image_count`).
fn scan_dir_lazy(
    path: &Path,
    expanded_folders: &HashSet<&str>,
    show_image_counts: bool,
    prefetch_one_level: bool,
    check_unscanned: bool,
    counts_until: std::time::Instant,
) -> Result<(Vec<FolderNode>, usize), std::io::Error> {
    let mut children_folders = Vec::new();
    let mut current_dir_image_count = 0;

    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Could not scan directory '{}': {}", path.display(), e);
            return Ok((Vec::new(), 0));
        }
    };

    for entry in entries.filter_map(Result::ok) {
        let current_path = entry.path();
        let (file_type, modified, created) = match entry.metadata() {
            Ok(meta) => {
                let ft = meta.file_type();
                let mod_time = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let cre_time = meta.created().unwrap_or(mod_time);

                (
                    ft,
                    mod_time
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    cre_time
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                )
            }
            Err(_) => continue,
        };

        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if name_str.starts_with('.') {
            continue;
        }

        if file_type.is_dir() {
            let path_str = current_path.to_string_lossy().into_owned();
            let is_expanded = expanded_folders.contains(path_str.as_str());

            let should_scan = is_expanded || prefetch_one_level;
            let next_prefetch = is_expanded;

            let (grand_children, sub_dir_own_images) = if should_scan {
                scan_dir_lazy(
                    &current_path,
                    expanded_folders,
                    show_image_counts,
                    next_prefetch,
                    check_unscanned,
                    counts_until,
                )?
            } else {
                let count = if show_image_counts {
                    unscanned_image_count(&current_path, 0, counts_until)
                } else {
                    0
                };
                (Vec::new(), count)
            };

            let has_any_subdirs = if should_scan {
                grand_children.iter().any(|c| c.is_dir)
            } else {
                check_unscanned && has_subdirs(&current_path)
            };

            let grand_children_sum: usize = grand_children.iter().map(|c| c.image_count).sum();
            let total_child_count = sub_dir_own_images + grand_children_sum;

            children_folders.push(FolderNode {
                name: name_str.into_owned(),
                path: path_str,
                children: grand_children,
                is_dir: true,
                image_count: total_child_count,
                has_subdirs: has_any_subdirs,
                modified,
                created,
            });
        } else if show_image_counts
            && file_type.is_file()
            && crate::formats::is_supported_image_file(&current_path)
        {
            current_dir_image_count += 1;
        }
    }

    children_folders.sort_by_key(|a| a.name.to_lowercase());

    Ok((children_folders, current_dir_image_count))
}

fn get_folder_tree_sync(
    path: String,
    expanded_folders: Vec<String>,
    show_image_counts: bool,
    check_unscanned: bool,
) -> Result<FolderNode, String> {
    let root_path = Path::new(&path);
    if !root_path.is_dir() {
        return Err(format!("Directory does not exist: {}", path));
    }

    let (modified, created) = root_path
        .metadata()
        .map(|m| {
            let mod_time = m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let cre_time = m.created().unwrap_or(mod_time);
            (
                mod_time
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                cre_time
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            )
        })
        .unwrap_or((0, 0));

    let expanded_set: HashSet<&str> = expanded_folders.iter().map(|s| s.as_str()).collect();

    let (children, own_count) = scan_dir_lazy(
        root_path,
        &expanded_set,
        show_image_counts,
        true,
        check_unscanned,
        std::time::Instant::now() + FOLDER_COUNT_BUDGET,
    )
    .map_err(|e| e.to_string())?;

    let children_sum: usize = children.iter().map(|c| c.image_count).sum();
    let has_subdirs = children.iter().any(|c| c.is_dir);

    let name = match root_path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => {
            let trimmed = path.trim_end_matches(&['/', '\\'][..]);
            if trimmed.is_empty() {
                path.clone()
            } else {
                trimmed.to_string()
            }
        }
    };

    Ok(FolderNode {
        name,
        path: path.clone(),
        children,
        is_dir: true,
        image_count: own_count + children_sum,
        has_subdirs,
        modified,
        created,
    })
}

#[tauri::command]
pub async fn get_folder_children(
    path: String,
    show_image_counts: bool,
) -> Result<Vec<FolderNode>, String> {
    match tauri::async_runtime::spawn_blocking(move || {
        let root_path = Path::new(&path);
        if !root_path.is_dir() {
            return Err(format!("Directory does not exist: {}", path));
        }
        let empty_set = HashSet::new();
        let until = std::time::Instant::now() + FOLDER_COUNT_BUDGET;
        let (children, _) =
            scan_dir_lazy(root_path, &empty_set, show_image_counts, false, true, until)
                .map_err(|e| e.to_string())?;

        Ok(children)
    })
    .await
    {
        Ok(Ok(children)) => Ok(children),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("Task failed: {}", e)),
    }
}

#[tauri::command]
pub async fn get_folder_tree(
    path: String,
    expanded_folders: Vec<String>,
    show_image_counts: bool,
) -> Result<FolderNode, String> {
    match tauri::async_runtime::spawn_blocking(move || {
        get_folder_tree_sync(path, expanded_folders, show_image_counts, true)
    })
    .await
    {
        Ok(Ok(folder_node)) => Ok(folder_node),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("Failed to execute folder tree task: {}", e)),
    }
}

#[tauri::command]
pub async fn get_pinned_folder_trees(
    paths: Vec<String>,
    expanded_folders: Vec<String>,
    show_image_counts: bool,
    shallow: Option<bool>,
) -> Result<Vec<FolderNode>, String> {
    let check_unscanned = !shallow.unwrap_or(false);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let results: Vec<Result<FolderNode, String>> = paths
            .par_iter()
            .map(|path| {
                get_folder_tree_sync(
                    path.clone(),
                    expanded_folders.clone(),
                    show_image_counts,
                    check_unscanned,
                )
            })
            .collect();

        let mut folder_nodes = Vec::new();
        for result in results {
            match result {
                Ok(node) => folder_nodes.push(node),
                Err(e) => log::warn!("Failed to get tree for pinned folder: {}", e),
            }
        }
        folder_nodes
    })
    .await;

    match result {
        Ok(nodes) => Ok(nodes),
        Err(e) => Err(format!("Task failed: {}", e)),
    }
}

pub fn read_file_mapped(path: &Path) -> Result<Mmap, ReadFileError> {
    if !path.is_file() {
        return Err(ReadFileError::Invalid);
    }
    if !path.exists() {
        return Err(ReadFileError::NotFound);
    }
    if path.metadata().map_err(ReadFileError::Io)?.len() == 0 {
        return Err(ReadFileError::Empty);
    }
    let file = fs::File::open(path).map_err(ReadFileError::Io)?;
    if file.try_lock_shared().is_err() {
        return Err(ReadFileError::Locked);
    }
    let mmap = unsafe {
        MmapOptions::new()
            .len(file.metadata().map_err(ReadFileError::Io)?.len() as usize)
            .map(&file)
            .map_err(ReadFileError::Io)?
    };
    Ok(mmap)
}

/// `path_str`'s thumbnail with `adjustments` (its edit, as the cache key
/// was made from), `dimension` pixels on its longer side.
pub fn generate_thumbnail_data(
    path_str: &str,
    adjustments: &Value,
    gpu_context: Option<&GpuContext>,
    app_handle: &AppHandle,
    dimension: u32,
) -> anyhow::Result<DynamicImage> {
    let (source_path, _) = parse_virtual_path(path_str);
    // A video stands in the grid as a frame from itself. None of the
    // editing pipeline below applies to it.
    if is_video_file(&source_path) {
        return crate::video::poster_frame(&source_path);
    }
    let state = app_handle.state::<AppState>();
    let context = gpu_context
        .ok_or_else(|| anyhow::anyhow!("Thumbnails need the GPU, which failed to initialize"))?;
    // Its own caches and a speed demosaic: a thumbnail is small and must
    // never evict the photo the editor is working on.
    crate::color_engine::application::render_thumbnail(
        context,
        &state,
        path_str,
        adjustments,
        dimension,
    )
    .map(|f| DynamicImage::ImageRgba8(f.preview_rgba8()))
}

/// A photo's edit as its thumbnail is made and cached from: the sidecar's
/// adjustments (as stored, not migrated), with its rating and whether it
/// counts as edited. A missing or unreadable sidecar is no edit.
struct ThumbnailEdit {
    adjustments: Value,
    bytes: Vec<u8>,
    rating: u8,
    is_edited: bool,
}

impl ThumbnailEdit {
    fn read(path_str: &str, keys: &ThumbnailKeys) -> Self {
        let (_, sidecar_path) = parse_virtual_path(path_str);
        let meta = fs::read_to_string(&sidecar_path)
            .ok()
            .and_then(|content| serde_json::from_str::<ImageMetadata>(&content).ok());
        let Some(meta) = meta else {
            return Self {
                adjustments: Value::Null,
                bytes: Vec::new(),
                rating: 0,
                is_edited: false,
            };
        };
        let is_raw = crate::formats::is_raw_file(path_str);
        let tm = if is_raw {
            keys.tonemapper_raw
        } else {
            keys.tonemapper_other
        };
        Self {
            bytes: serde_json::to_vec(&meta.adjustments).unwrap_or_default(),
            is_edited: crate::image_processing::is_image_edited(&meta.adjustments, is_raw, tm),
            rating: meta.rating,
            adjustments: meta.adjustments,
        }
    }
}

/// `path_str`'s cached thumbnail for its current edit, as the data URL the
/// grid shows, when there is one. Reads only its sidecar and the cache.
fn cached_thumbnail(
    path_str: &str,
    thumb_cache_dir: &Path,
    keys: &ThumbnailKeys,
) -> Option<(String, u8, bool)> {
    let edit = ThumbnailEdit::read(path_str, keys);
    let cache_path = thumb_cache_dir.join(format!("{}.jpg", keys.key(path_str, &edit.bytes)?));
    let data = fs::read(&cache_path).ok()?;
    touch_cached_thumbnail(&cache_path);
    Some((thumbnail_data_url(&data), edit.rating, edit.is_edited))
}

fn thumbnail_data_url(jpeg: &[u8]) -> String {
    format!(
        "data:image/jpeg;base64,{}",
        general_purpose::STANDARD.encode(jpeg)
    )
}

/// Tell the grid a photo's thumbnail is ready. Its rating is not sent: it
/// was read before a render that can take seconds, and a rating given
/// meanwhile must not be undone by it.
fn emit_thumbnail(app_handle: &AppHandle, path: &str, data: &str, is_edited: bool) {
    let _ = app_handle.emit(
        "thumbnail-generated",
        serde_json::json!({ "path": path, "data": data, "is_edited": is_edited }),
    );
}

fn encode_thumbnail(image: &DynamicImage, target_width: u32) -> Result<Vec<u8>> {
    let thumbnail = crate::image_processing::downscale_f32_image(image, target_width, target_width);
    let mut buf = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 75);
    encoder.encode_image(&thumbnail.to_rgb8())?;
    Ok(buf.into_inner())
}

/// Whether `path` already has a thumbnail cached for its current edit (reads
/// only its edit file and the cache, never the photo).
pub(crate) fn has_cached_thumbnail(
    path_str: &str,
    thumb_cache_dir: &Path,
    keys: &ThumbnailKeys,
) -> bool {
    let edit = ThumbnailEdit::read(path_str, keys);
    keys.key(path_str, &edit.bytes)
        .is_some_and(|h| thumb_cache_dir.join(format!("{h}.jpg")).exists())
}

pub(crate) fn generate_single_thumbnail_and_cache(
    path_str: &str,
    thumb_cache_dir: &Path,
    gpu_context: Option<&GpuContext>,
    force_regenerate: bool,
    app_handle: &AppHandle,
    settings: &AppSettings,
) -> Option<(String, u8, bool)> {
    let keys = ThumbnailKeys::new(&app_handle.state::<AppState>(), settings);
    let edit = ThumbnailEdit::read(path_str, &keys);
    let (rating, is_edited) = (edit.rating, edit.is_edited);
    let cache_hash = keys.key(path_str, &edit.bytes)?;

    let cache_filename = format!("{}.jpg", cache_hash);
    let cache_path = thumb_cache_dir.join(cache_filename);

    if !force_regenerate && let Ok(data) = fs::read(&cache_path) {
        touch_cached_thumbnail(&cache_path);
        return Some((thumbnail_data_url(&data), rating, is_edited));
    }

    // A photo only in the cloud would download in full to be read: it is
    // drawn from its smart preview when it has one, and otherwise keeps its
    // cloud placeholder (any thumbnail made while it was here, above, still
    // shows).
    if crate::cloud_files::is_cloud_only_str(path_str) {
        let (source, _) = parse_virtual_path(path_str);
        if !(crate::smart_preview::may_stand_in(&source)
            && crate::smart_preview::is_current(&source))
        {
            return None;
        }
    }

    let target_width = thumbnail_width(settings);

    if let Ok(thumb_image) = generate_thumbnail_data(
        path_str,
        &edit.adjustments,
        gpu_context,
        app_handle,
        target_width,
    ) && let Ok(thumb_data) = encode_thumbnail(&thumb_image, target_width)
    {
        write_cached_thumbnail(&cache_path, &thumb_data);
        return Some((thumbnail_data_url(&thumb_data), rating, is_edited));
    }
    None
}

/// Thumbnail requests made before the latest cancel (a folder left, the
/// queue cleared) belong to an earlier generation: a render of one still
/// under way when it finishes is neither shown nor counted.
static THUMBNAIL_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current generation, starting a new one when a cancel was asked for
/// (`cancel_thumbnail_generation` raises the token).
fn thumbnail_generation(state: &AppState) -> u64 {
    use std::sync::atomic::Ordering;
    if state
        .thumbnail_cancellation_token
        .swap(false, Ordering::SeqCst)
    {
        THUMBNAIL_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
    THUMBNAIL_GENERATION.load(Ordering::SeqCst)
}

/// How many photos are thumbnailed (or batch-rendered) at once: the
/// setting, as the thumbnail workers read it.
fn thumbnail_worker_count(settings: &AppSettings) -> usize {
    settings.thumbnail_worker_threads.unwrap_or(4).clamp(1, 16) as usize
}

/// Run a batch's per-photo work (`par_iter`s inside `work`) on at most as
/// many threads as thumbnails are made with, instead of every core: each
/// photo is a full render, and a dozen at once would hold gigabytes and
/// starve the folder listing.
fn in_batch_pool<R: Send>(settings: &AppSettings, work: impl FnOnce() -> R + Send) -> R {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(thumbnail_worker_count(settings))
        .thread_name(|i| format!("batch-render-{i}"))
        .build()
    {
        Ok(pool) => pool.install(work),
        Err(e) => {
            log::warn!("Batch pool unavailable ({e}); using the shared one");
            work()
        }
    }
}

pub fn start_thumbnail_workers(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<crate::AppState>();
    let manager = state.thumbnail_manager.clone();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let thread_count = thumbnail_worker_count(&settings);

    // Once the app has settled: thumbnails unused for months, and the
    // least recently used beyond the cache's size, go.
    if let Ok(dir) = get_thumb_cache_dir(&app_handle) {
        thread::spawn(move || {
            thread::sleep(std::time::Duration::from_secs(30));
            prune_thumbnail_cache_soon(dir);
        });
    }

    for _ in 0..thread_count {
        let app_clone = app_handle.clone();
        let manager_clone = manager.clone();

        std::thread::spawn(move || {
            loop {
                let path_to_process: String = {
                    let mut queue = manager_clone.queue.lock().unwrap();
                    while queue.is_empty() {
                        queue = manager_clone.cvar.wait(queue).unwrap();
                    }
                    let path = queue.pop_back().unwrap();

                    let mut processing = manager_clone.processing_now.lock().unwrap();
                    if processing.contains(&path) {
                        let state = app_clone.state::<crate::AppState>();
                        increment_thumbnail_progress(&state, &app_clone);
                        continue;
                    }
                    processing.insert(path.clone());
                    path
                };

                let state = app_clone.state::<crate::AppState>();
                let generation = thumbnail_generation(&state);
                let gpu_context =
                    crate::gpu_processing::get_or_init_gpu_context(&state, &app_clone).ok();
                // Read for each photo: a thumbnail size changed meanwhile is
                // the size it is made and encoded at.
                let settings = load_settings(app_clone.clone()).unwrap_or_default();

                if let Ok(cache_dir) = get_thumb_cache_dir(&app_clone) {
                    let result = generate_single_thumbnail_and_cache(
                        &path_to_process,
                        &cache_dir,
                        gpu_context.as_ref(),
                        false,
                        &app_clone,
                        &settings,
                    );

                    if thumbnail_generation(&state) == generation {
                        if let Some((thumbnail_data, _, is_edited)) = result {
                            emit_thumbnail(
                                &app_clone,
                                &path_to_process,
                                &thumbnail_data,
                                is_edited,
                            );
                        }
                        increment_thumbnail_progress(&state, &app_clone);
                    }
                }
                manager_clone
                    .processing_now
                    .lock()
                    .unwrap()
                    .remove(&path_to_process);
            }
        });
    }
}

#[tauri::command]
pub fn update_thumbnail_queue(
    paths: Vec<String>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<crate::AppState>();

    if paths.is_empty() {
        let mut queue = state.thumbnail_manager.queue.lock().unwrap();
        queue.clear();
        // Clearing is a cancel: what is still being made is for photos no
        // longer shown.
        THUMBNAIL_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut tracker = state.thumbnail_progress.lock().unwrap();
        tracker.total = 0;
        tracker.completed = 0;
        drop(tracker);

        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        state.thumbnail_manager.cvar.notify_all();
        return Ok(());
    }

    let mut unique_paths = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if seen.insert(path.clone()) {
            unique_paths.push(path);
        }
    }
    let generation = thumbnail_generation(&state);

    // Off the main thread: thumbnails already cached are sent straight
    // away, rather than waiting in the queue behind RAWs being decoded;
    // only the rest are queued.
    let app = app_handle.clone();
    thread::spawn(move || {
        let state = app.state::<crate::AppState>();
        let keys = ThumbnailKeys::current(&app);
        let cache_dir = get_thumb_cache_dir(&app).ok();
        let mut misses = Vec::new();
        for path in &unique_paths {
            if thumbnail_generation(&state) != generation {
                return;
            }
            match cache_dir
                .as_deref()
                .and_then(|dir| cached_thumbnail(path, dir, &keys))
            {
                Some((data, _, is_edited)) => emit_thumbnail(&app, path, &data, is_edited),
                None => misses.push(path.clone()),
            }
        }
        queue_thumbnails(&app, generation, &seen, misses);
        // Cloud photos asked for here are on screen: their previews come
        // first (outside the queue's lock: this reads each one's flags).
        crate::cloud_builder::request(&app, &unique_paths);
    });
    Ok(())
}

/// Queue `misses` (of the photos `requested`) for the thumbnail workers,
/// unless a cancel came since `generation`.
fn queue_thumbnails(
    app_handle: &AppHandle,
    generation: u64,
    requested: &HashSet<String>,
    misses: Vec<String>,
) {
    let state = app_handle.state::<crate::AppState>();
    let mut queue = state.thumbnail_manager.queue.lock().unwrap();
    if thumbnail_generation(&state) != generation {
        return;
    }
    queue.retain(|p| !requested.contains(p));

    // The oldest requests make way; the app is told, so it asks again for
    // any it still shows.
    let mut dropped = Vec::new();
    while queue.len() + misses.len() > 500 {
        match queue.pop_front() {
            Some(path) => dropped.push(path),
            None => break,
        }
    }
    if !dropped.is_empty() {
        let _ = app_handle.emit("thumbnails-dropped", &dropped);
    }

    for path in misses {
        queue.push_back(path);
    }

    let queue_len = queue.len();
    drop(queue);

    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total = tracker.completed + queue_len;

    let current = tracker.completed;
    let total = tracker.total;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );

    state.thumbnail_manager.cvar.notify_all();
}

pub fn add_to_thumbnail_queue(state: &AppState, count: usize, app_handle: &AppHandle) {
    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total += count;
    let current = tracker.completed;
    let total = tracker.total;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );
}

pub fn increment_thumbnail_progress(state: &AppState, app_handle: &AppHandle) {
    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.completed += 1;
    let current = tracker.completed;
    let total = tracker.total;

    if current >= total {
        tracker.total = 0;
        tracker.completed = 0;
        drop(tracker);

        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        let _ = app_handle.emit("thumbnail-generation-complete", true);
    } else {
        drop(tracker);
        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": current, "total": total }),
        );
    }
}

pub fn resolve_lens_params_in_adjustments(
    adjustments: &mut Value,
    exif_data: &Option<HashMap<String, String>>,
    lens_db: Option<&crate::lens_correction::LensDatabase>,
) {
    if let Some(map) = adjustments.as_object_mut() {
        let mode = map
            .get("lensCorrectionMode")
            .and_then(|v| v.as_str())
            .unwrap_or("manual");

        if mode == "auto" {
            if let Some(exif) = exif_data {
                let exif_maker = exif.get("Make").map(|s| s.as_str()).unwrap_or("");
                let exif_model = exif.get("LensModel").map(|s| s.as_str()).unwrap_or("");
                if let Some(db) = lens_db {
                    if let Some((detected_maker, detected_model)) =
                        crate::lens_correction::find_best_lens_match(db, exif_maker, exif_model)
                    {
                        map.insert(
                            "lensMaker".to_string(),
                            serde_json::to_value(&detected_maker).unwrap(),
                        );
                        map.insert(
                            "lensModel".to_string(),
                            serde_json::to_value(&detected_model).unwrap(),
                        );
                    } else {
                        map.remove("lensMaker");
                        map.remove("lensModel");
                    }
                }
            } else {
                map.remove("lensMaker");
                map.remove("lensModel");
            }
        }

        if let Some(db) = lens_db {
            let has_valid_lens = match (
                map.get("lensMaker").and_then(|v| v.as_str()),
                map.get("lensModel").and_then(|v| v.as_str()),
            ) {
                (Some(maker), Some(model)) if !maker.is_empty() && !model.is_empty() => {
                    let mut focal_length = 50.0;
                    let mut aperture = None;
                    let mut distance = None;

                    if let Some(exif) = exif_data {
                        if let Some(fl_str) = exif
                            .get("FocalLength")
                            .or(exif.get("FocalLengthIn35mmFilm"))
                            && let Ok(fl) = fl_str.replace(" mm", "").trim().parse::<f32>()
                        {
                            focal_length = fl;
                        }
                        if let Some(ap_str) = exif.get("ApertureValue").or(exif.get("FNumber"))
                            && let Ok(ap) = ap_str.replace("f/", "").trim().parse::<f32>()
                        {
                            aperture = Some(ap);
                        }
                        if let Some(dist_str) = exif.get("SubjectDistance")
                            && let Ok(dist) = dist_str.replace(" m", "").trim().parse::<f32>()
                        {
                            distance = Some(dist);
                        }
                    }

                    if let Some(params) = crate::lens_correction::resolve_lens_params(
                        db,
                        maker,
                        model,
                        focal_length,
                        aperture,
                        distance,
                    ) {
                        map.insert(
                            "lensDistortionParams".to_string(),
                            serde_json::to_value(params).unwrap(),
                        );
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };

            if !has_valid_lens {
                map.remove("lensDistortionParams");
            }
        }
    }
}

/// Writes a frame grabbed from a video next to it, as a PNG the editor
/// can then treat as an ordinary photograph.
#[tauri::command]
pub fn save_video_frame(
    video_path: String,
    png_base64: String,
    time_seconds: f64,
    frame_number: Option<u64>,
) -> Result<String, String> {
    let source = std::path::Path::new(&video_path);
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or("That video has no file name")?;
    let parent = source.parent().ok_or("That video has no folder")?;
    let bytes = general_purpose::STANDARD
        .decode(png_base64.as_bytes())
        .map_err(|e| format!("Could not read the frame: {e}"))?;
    // Name by frame number (or by time when the rate is unknown) so
    // grabbing several frames does not overwrite, and the names sort in
    // playback order.
    let stamp = match frame_number {
        Some(frame) => format!("{frame:05}"),
        None => format!("{:.3}", time_seconds.max(0.0)).replace('.', "s"),
    };
    let mut target = parent.join(format!("{stem}_frame_{stamp}.png"));
    let mut n = 2;
    while target.exists() {
        target = parent.join(format!("{stem}_frame_{stamp}_{n}.png"));
        n += 1;
    }
    fs::write(&target, bytes).map_err(|e| format!("Could not write {target:?}: {e}"))?;
    crate::versions::record_version(&target, source, crate::versions::FRAME_KIND);
    Ok(target.to_string_lossy().to_string())
}

/// The address the player streams a video from: a local HTTP endpoint that
/// AVFoundation, which WebKit plays media through, can actually open.
#[tauri::command]
pub fn video_stream_url(path: String) -> Result<String, String> {
    crate::video_server::stream_url(std::path::Path::new(&path))
}

/// Duration, size and codecs for a video, so the library can label it.
/// Returns empty fields rather than failing when the system has nothing.
#[tauri::command]
pub fn load_video_info(path: String) -> Result<crate::video::VideoInfo, String> {
    crate::video::video_info(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_supported_file_types() -> Result<serde_json::Value, String> {
    let raw_extensions: Vec<&str> = crate::formats::RAW_EXTENSIONS
        .iter()
        .map(|(ext, _)| *ext)
        .collect();
    let non_raw_extensions: Vec<&str> = crate::formats::NON_RAW_EXTENSIONS.to_vec();

    Ok(serde_json::json!({
        "raw": raw_extensions,
        "nonRaw": non_raw_extensions
    }))
}

#[tauri::command]
pub fn create_folder(path: String) -> Result<(), String> {
    let path_obj = Path::new(&path);
    if let (Some(parent), Some(new_folder_name_os)) = (path_obj.parent(), path_obj.file_name())
        && let Some(new_folder_name) = new_folder_name_os.to_str()
        && parent.exists()
    {
        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
            if let Ok(entry) = entry
                && entry.file_name().to_string_lossy().to_lowercase()
                    == new_folder_name.to_lowercase()
            {
                return Err("A folder with that name already exists.".to_string());
            }
        }
    }
    fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    crate::journal::record(
        format!("New folder {}", folder_name(Path::new(&path))),
        vec![crate::journal::Action::FolderCreated {
            path: PathBuf::from(&path),
        }],
    );
    Ok(())
}

#[tauri::command]
pub fn rename_folder(path: String, new_name: String, app_handle: AppHandle) -> Result<(), String> {
    let p = Path::new(&path);
    if !p.is_dir() {
        return Err("Path is not a directory.".to_string());
    }
    if let Some(parent) = p.parent() {
        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
            if let Ok(entry) = entry
                && entry.file_name().to_string_lossy().to_lowercase() == new_name.to_lowercase()
                && entry.path() != p
            {
                return Err("A folder with that name already exists.".to_string());
            }
        }
        let new_path = parent.join(&new_name);
        fs::rename(p, &new_path).map_err(|e| e.to_string())?;
        crate::journal::record(
            format!("Rename folder {} to {new_name}", folder_name(p)),
            vec![crate::journal::Action::Moved {
                from: p.to_path_buf(),
                to: new_path.clone(),
            }],
        );

        let new_folder_str = new_path.to_string_lossy().into_owned();
        sync_album_path_changes(&app_handle, None, None, Some((&path, &new_folder_str)));

        Ok(())
    } else {
        Err("Could not determine parent directory.".to_string())
    }
}

#[tauri::command]
pub fn delete_folder(path: String, app_handle: AppHandle) -> Result<(), String> {
    let actions = crate::journal::trash_all(&[PathBuf::from(&path)])?;
    crate::journal::record(
        format!("Delete folder {}", folder_name(Path::new(&path))),
        actions,
    );

    let mut deletions = HashSet::new();
    deletions.insert(path);
    sync_album_path_changes(&app_handle, None, Some(&deletions), None);

    Ok(())
}

#[tauri::command]
pub fn duplicate_file(
    path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let (source_path, _) = parse_virtual_path(&path);
    if !source_path.is_file() {
        return Err("Source path is not a file.".to_string());
    }

    let parent = source_path
        .parent()
        .ok_or("Could not get parent directory")?;
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Could not get file stem")?;
    let extension = source_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let mut counter = 1;
    let mut dest_path;
    loop {
        let new_stem = if counter == 1 {
            format!("{}_copy", stem)
        } else {
            format!("{}_copy_{}", stem, counter - 1)
        };
        dest_path = parent.join(format!("{}.{}", new_stem, extension));
        if !dest_path.exists() {
            break;
        }
        counter += 1;
    }

    fs::copy(&source_path, &dest_path).map_err(|e| e.to_string())?;
    let mut actions = vec![crate::journal::Action::Created {
        path: dest_path.clone(),
    }];

    // Its edits and XMP come along; its virtual copies stay with the original.
    let name = source_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    for companion in companion_files(&source_path) {
        let file = companion
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let is_virtual_copy = file != format!("{name}.dri")
            && file.starts_with(&format!("{name}."))
            && file.ends_with(".dri");
        if is_virtual_copy {
            continue;
        }
        let target = companion_target(&companion, &source_path, &dest_path);
        if !target.exists() {
            fs::copy(&companion, &target).map_err(|e| e.to_string())?;
            actions.push(crate::journal::Action::Created { path: target });
        }
    }
    crate::journal::record("Duplicate 1 photo", actions);

    let dest_path_str = dest_path.to_string_lossy().into_owned();

    if let Some(album_id) = target_album_id {
        let _ = add_to_album(album_id, vec![dest_path_str.clone()], app_handle);
    }

    Ok(dest_path_str)
}

/// The files that belong to `image` and travel with it: its app sidecar,
/// its virtual copies' sidecars, the legacy `.rrexif`, a cloud photo's
/// smart preview (`.dripreview`), and its XMP when it owns one (a JPEG
/// never takes its RAW twin's `IMG.xmp`, see `xmp::sidecar_for`).
pub fn companion_files(image: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let (Some(parent), Some(name)) = (image.parent(), image.file_name()) else {
        return found;
    };
    let name = name.to_string_lossy().into_owned();
    let primary = format!("{name}.dri");
    let legacy_exif = format!("{name}.rrexif");
    let smart_preview = format!("{name}.dripreview");
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.filter_map(Result::ok) {
            let entry_name = entry.file_name().to_string_lossy().into_owned();
            // A copy's sidecar is exactly `{name}.{id}.dri`, so another
            // photo named `{name}.png` never has its sidecar taken.
            let is_copy = matches!(
                split_sidecar_name(&entry_name),
                Some((photo, Some(_))) if photo == name
            );
            let is_sidecar = entry_name == primary
                || is_copy
                || entry_name == legacy_exif
                || entry_name == smart_preview;
            if is_sidecar && entry.path().is_file() {
                found.push(entry.path());
            }
        }
    }
    if let Some((xmp, true)) = crate::xmp::sidecar_for(image) {
        found.push(xmp);
    }
    // Other editors' settings beside the photo: darktable's `IMG.CR3.xmp`
    // (when Lightroom's `IMG.xmp` is there too), RawTherapee's `.pp3`,
    // DxO's `.dop` and ON1's `.on1`, so they travel with it as well. Matched
    // against the folder's real names, ignoring case, so a drive that ignores
    // case never yields one file twice.
    let stem = image.file_stem().unwrap_or_default().to_string_lossy();
    let wanted: Vec<String> = [
        format!("{name}.xmp"),
        format!("{name}.pp3"),
        format!("{name}.dop"),
        format!("{name}.on1"),
        format!("{stem}.on1"),
    ]
    .iter()
    .map(|n| n.to_lowercase())
    .collect();
    let have: Vec<String> = found
        .iter()
        .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .collect();
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.filter_map(Result::ok) {
            let lower = entry.file_name().to_string_lossy().to_lowercase();
            if wanted.contains(&lower) && !have.contains(&lower) && entry.path().is_file() {
                found.push(entry.path());
            }
        }
    }
    found
}

/// Where `companion` goes when `old_image` becomes `new_image`: sidecars
/// named after the whole file name (`IMG.CR3.dri`, `IMG.CR3.xmp`) or
/// after its stem (`IMG.xmp`) follow the new name.
pub fn companion_target(companion: &Path, old_image: &Path, new_image: &Path) -> PathBuf {
    let parent = new_image.parent().unwrap_or_else(|| Path::new(""));
    let file = companion.file_name().unwrap_or_default().to_string_lossy();
    let old_name = old_image.file_name().unwrap_or_default().to_string_lossy();
    let new_name = new_image.file_name().unwrap_or_default().to_string_lossy();
    if let Some(rest) = file.strip_prefix(&*old_name) {
        return parent.join(format!("{new_name}{rest}"));
    }
    let old_stem = old_image.file_stem().unwrap_or_default().to_string_lossy();
    let new_stem = new_image.file_stem().unwrap_or_default().to_string_lossy();
    if let Some(rest) = file.strip_prefix(&format!("{old_stem}.")) {
        return parent.join(format!("{new_stem}.{rest}"));
    }
    parent.join(&*file)
}

/// "3 photos", "1 photo".
fn photos(n: usize) -> String {
    format!("{n} photo{}", if n == 1 { "" } else { "s" })
}

/// A folder's name for an operation's label.
fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A photo plus everything that travels with it.
pub(crate) fn find_all_associated_files(source_image_path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = vec![source_image_path.to_path_buf()];
    files.extend(companion_files(source_image_path));
    Ok(files)
}

/// `name_copy_1.ext`, `name_copy_2.ext`… — the first that does not exist
/// in `folder`, so a copy never replaces a file already there.
fn free_copy_name(folder: &Path, image: &Path) -> PathBuf {
    let stem = image.file_stem().unwrap_or_default().to_string_lossy();
    let ext = image
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut n = 1;
    loop {
        let candidate = folder.join(format!("{stem}_copy_{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// After `old` was renamed to `new` in the same folder, point versions
/// made from it (restores, denoised copies…) at the new name.
fn relink_versions(old: &Path, new: &Path) {
    let (Some(parent), Some(old_name), Some(new_name)) =
        (old.parent(), old.file_name(), new.file_name())
    else {
        return;
    };
    let (old_name, new_name) = (old_name.to_string_lossy(), new_name.to_string_lossy());
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "dri") {
            continue;
        }
        let relinked = crate::exif_processing::modify_sidecar(&path, |meta| {
            if meta.derived_from.as_deref() != Some(&*old_name) {
                return false;
            }
            meta.derived_from = Some(new_name.to_string());
            true
        });
        if let Err(e) = relinked {
            log::warn!("{e}");
        }
    }
}

#[tauri::command]
pub fn copy_files(
    source_paths: Vec<String>,
    destination_folder: String,
) -> Result<Vec<String>, String> {
    let dest_path = Path::new(&destination_folder);
    if !dest_path.is_dir() {
        return Err(format!(
            "Destination is not a folder: {}",
            destination_folder
        ));
    }

    let unique_source_images: HashSet<PathBuf> = source_paths
        .iter()
        .map(|p| parse_virtual_path(p).0)
        .collect();

    let mut created = Vec::new();
    let mut actions = Vec::new();
    for source_image_path in unique_source_images {
        let file_name = source_image_path
            .file_name()
            .ok_or("Could not get file name")?;
        // Same name where free; never over an existing file.
        let mut dest_image = dest_path.join(file_name);
        if dest_image.exists() {
            dest_image = free_copy_name(dest_path, &source_image_path);
        }
        fs::copy(&source_image_path, &dest_image).map_err(|e| e.to_string())?;
        actions.push(crate::journal::Action::Created {
            path: dest_image.clone(),
        });
        for companion in companion_files(&source_image_path) {
            let target = companion_target(&companion, &source_image_path, &dest_image);
            if !target.exists() {
                fs::copy(&companion, &target).map_err(|e| e.to_string())?;
                actions.push(crate::journal::Action::Created { path: target });
            }
        }
        created.push(dest_image.to_string_lossy().into_owned());
    }
    crate::journal::record(
        format!(
            "Copy {} to {}",
            photos(created.len()),
            folder_name(dest_path)
        ),
        actions,
    );
    Ok(created)
}

#[tauri::command]
pub fn move_files(
    source_paths: Vec<String>,
    destination_folder: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    let dest_path = Path::new(&destination_folder);
    if !dest_path.is_dir() {
        return Err(format!(
            "Destination is not a folder: {}",
            destination_folder
        ));
    }

    let unique_source_images: HashSet<PathBuf> = source_paths
        .iter()
        .map(|p| parse_virtual_path(p).0)
        .collect();

    // Everything is planned and checked before anything moves.
    let mut moves = Vec::new();
    let mut photo_moves = Vec::new();
    for source_image_path in &unique_source_images {
        let source_parent = source_image_path
            .parent()
            .ok_or("Could not get parent directory")?;
        if source_parent == dest_path {
            return Err("Cannot move files into the same folder they are already in.".to_string());
        }
        let file_name = source_image_path
            .file_name()
            .ok_or("Could not get file name")?;
        let dest_image_path = dest_path.join(file_name);
        for file in find_all_associated_files(source_image_path)? {
            let target = companion_target(&file, source_image_path, &dest_image_path);
            moves.push((file, target));
        }
        photo_moves.push((source_image_path.clone(), dest_image_path));
    }
    check_move_targets(&moves)?;

    let label = format!(
        "Move {} to {}",
        photos(photo_moves.len()),
        folder_name(dest_path)
    );
    let done = crate::journal::move_all(&moves)
        .map_err(|(error, stranded)| record_partial(&label, &stranded, &app_handle, error))?;

    // Originals copied across volumes go to the Trash once everything has
    // arrived. Undo brings the moved files back, so these are not journalled.
    let copied: Vec<PathBuf> = done
        .iter()
        .filter(|d| d.copied)
        .map(|d| d.from.clone())
        .collect();
    let (_, trash_problem) = crate::journal::trash_each(&copied);

    crate::journal::record(
        label,
        done.iter().map(crate::journal::Done::action).collect(),
    );

    let renames: HashMap<String, String> = photo_moves
        .iter()
        .map(|(from, to)| {
            (
                from.to_string_lossy().into_owned(),
                to.to_string_lossy().into_owned(),
            )
        })
        .collect();
    sync_album_path_changes(&app_handle, Some(&renames), None, None);

    match trash_problem {
        None => Ok(()),
        Some(problem) => Err(format!(
            "The photos were copied to {}, but the originals stayed. {problem}",
            folder_name(dest_path)
        )),
    }
}

/// Check a batch of moves or renames before any happens: no file may land
/// on one already there, or two on the same name (compared ignoring case,
/// as the Mac's drives do), so nothing is ever overwritten.
fn check_move_targets(moves: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let mut wanted = HashSet::new();
    for (from, to) in moves {
        if to != from && to.exists() {
            return Err(format!("A file named {} already exists.", to.display()));
        }
        if !wanted.insert(to.to_string_lossy().to_lowercase()) {
            return Err(format!("Two files would both be named {}.", to.display()));
        }
    }
    Ok(())
}

/// A batch move or rename failed part-way, and `stranded` could not be put
/// back: record them so Undo can, and point albums at the photos among
/// them. Returns the message to show.
fn record_partial(
    label: &str,
    stranded: &[crate::journal::Done],
    app_handle: &AppHandle,
    error: String,
) -> String {
    if stranded.is_empty() {
        return format!("{error}. Nothing was changed.");
    }
    crate::journal::record(
        format!("{label} (incomplete)"),
        stranded.iter().map(crate::journal::Done::action).collect(),
    );
    let renames: HashMap<String, String> = stranded
        .iter()
        .filter(|d| !d.copied && is_supported_media_file(&d.to))
        .map(|d| {
            (
                d.from.to_string_lossy().into_owned(),
                d.to.to_string_lossy().into_owned(),
            )
        })
        .collect();
    if !renames.is_empty() {
        sync_album_path_changes(app_handle, Some(&renames), None, None);
    }
    format!(
        "{error}. {} file(s) could not be put back; Undo can try again.",
        stranded.len()
    )
}

// Off the main thread: the save renders an interpretation snapshot, and the
// window must not wait for it.
#[tauri::command(async)]
pub fn save_metadata_and_update_thumbnail(
    path: String,
    adjustments: Value,
    app_handle: AppHandle,
    state: tauri::State<AppState>,
) -> Result<(), String> {
    let (_, sidecar_path) = parse_virtual_path(&path);

    // Refuse before the slow work below if the sidecar is unreadable: the
    // save would otherwise replace everything else in it with defaults.
    let metadata = crate::exif_processing::try_load_sidecar(&sidecar_path)?;

    let mut final_adjustments = crate::color_engine::migration::normalize(&adjustments)
        .map(|edits| edits.into_owned())
        .unwrap_or_else(|_| adjustments.clone());
    {
        let lens_db_guard = state.lens_db.lock().unwrap();
        resolve_lens_params_in_adjustments(
            &mut final_adjustments,
            &metadata.exif,
            lens_db_guard.as_deref(),
        );
    }

    // The interpretation snapshot is diagnostic. A photo that cannot be
    // interpreted right now (a pinned transform missing, a decode problem)
    // still gets its edits saved; the snapshot records the error instead.
    {
        final_adjustments["v3Input"] = match crate::color_engine::application::input_snapshot(
            &state,
            &path,
            &final_adjustments,
        ) {
            Ok(report) => report,
            Err(error) => {
                log::warn!("Saving edits without a v3 interpretation snapshot: {error:#}");
                serde_json::json!({"schema": 1, "error": format!("{error:#}")})
            }
        };
    }
    let xmp_sync = load_settings(app_handle.clone())
        .ok()
        .filter(|settings| settings.enable_xmp_sync.unwrap_or(true))
        .map(|settings| settings.create_xmp_if_missing.unwrap_or(false));

    // Only the edits are replaced: a rating or label saved meanwhile stays.
    crate::exif_processing::modify_sidecar(&sidecar_path, |metadata| {
        metadata.adjustments = final_adjustments;
        // XMP first: writing it records its time in the sidecar saved here.
        if let Some(create_if_missing) = xmp_sync {
            crate::xmp::push(&path, metadata, create_if_missing);
        }
        true
    })?;

    let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();
    let app_handle_clone = app_handle.clone();
    let path_clone = path.clone();

    add_to_thumbnail_queue(&state, 1, &app_handle);

    thread::spawn(move || {
        let state = app_handle_clone.state::<AppState>();
        let settings = load_settings(app_handle_clone.clone()).unwrap_or_default();

        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle_clone) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!(
                    "Unable to initialize thumbnail cache directory for '{}': {}",
                    path_clone,
                    e
                );
                emit_thumbnail_cache_setup_error(&app_handle_clone, &path_clone, &e);
                increment_thumbnail_progress(&state, &app_handle_clone);
                return;
            }
        };

        let result = generate_single_thumbnail_and_cache(
            &path_clone,
            &thumb_cache_dir,
            gpu_context.as_ref(),
            true,
            &app_handle_clone,
            &settings,
        );

        if let Some((thumbnail_data, rating, is_edited)) = result {
            let _ = app_handle_clone.emit(
                "thumbnail-generated",
                serde_json::json!({ "path": path_clone, "data": thumbnail_data, "rating": rating, "is_edited": is_edited }),
            );
        }

        increment_thumbnail_progress(&state, &app_handle_clone);
    });

    Ok(())
}

/// Lay pasted v3 settings (only the ones chosen) over a photo's own, one
/// level deeper for its detail and effects, so a paste never resets the
/// photo's other v3 settings.
fn merge_pasted_v3(target: &mut Value, pasted: &Value) {
    let Some(pasted) = pasted.as_object() else {
        return;
    };
    if !target.is_object() {
        *target = serde_json::json!({});
    }
    let target = target.as_object_mut().expect("just made an object");
    for (k, v) in pasted {
        match (target.get_mut(k), v.as_object()) {
            (Some(existing), Some(fields))
                if (k == "detail" || k == "effects") && existing.is_object() =>
            {
                let existing = existing.as_object_mut().expect("checked");
                for (field, value) in fields {
                    existing.insert(field.clone(), value.clone());
                }
            }
            _ => {
                target.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Make the thumbnails of a batch just changed (pasted, reset, auto)
/// again, at most as many at once as thumbnails are made with.
fn rerender_batch_thumbnails(app_handle: &AppHandle, settings: &AppSettings, paths: &[String]) {
    let state = app_handle.state::<AppState>();
    let thumb_cache_dir = match resolve_thumbnail_cache_dir(app_handle) {
        Ok(dir) => dir,
        Err(e) => {
            log::warn!("Unable to initialize thumbnail cache directory: {}", e);
            for path in paths {
                emit_thumbnail_cache_setup_error(app_handle, path, &e);
            }
            for _ in 0..paths.len() {
                increment_thumbnail_progress(&state, app_handle);
            }
            return;
        }
    };

    let gpu_context = gpu_processing::get_or_init_gpu_context(&state, app_handle).ok();

    in_batch_pool(settings, || {
        paths.par_iter().for_each(|path_str| {
            let result = generate_single_thumbnail_and_cache(
                path_str,
                &thumb_cache_dir,
                gpu_context.as_ref(),
                true,
                app_handle,
                settings,
            );
            if let Some((thumbnail_data, _, is_edited)) = result {
                emit_thumbnail(app_handle, path_str, &thumbnail_data, is_edited);
            }
            increment_thumbnail_progress(&state, app_handle);
        });
    });
}

/// A photo's frame as its crop and mask shapes measure it, in pixels: its
/// decoded size, turned by its quarter turns (`pasted`'s, when the paste
/// sets them). None when it cannot be known.
fn pasted_frame(
    state: &AppState,
    path: &str,
    own: &Value,
    pasted: &serde_json::Map<String, Value>,
) -> Option<(f64, f64)> {
    let report = crate::color_engine::application::input_snapshot(state, path, own)
        .map_err(|e| log::warn!("Pasting onto {path} without fitting its crop and masks: {e:#}"))
        .ok()?;
    let (w, h) = (report["width"].as_f64()?, report["height"].as_f64()?);
    if w < 1.0 || h < 1.0 {
        return None;
    }
    let steps = pasted
        .get("orientationSteps")
        .or_else(|| own.get("orientationSteps"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(if steps % 2 == 1 { (h, w) } else { (w, h) })
}

/// Whether a paste carries shapes measured in its source photo's pixels.
fn pastes_shapes(pasted: &serde_json::Map<String, Value>) -> bool {
    pasted.get("crop").is_some_and(|c| !c.is_null())
        || pasted
            .get("masks")
            .and_then(Value::as_array)
            .is_some_and(|m| !m.is_empty())
}

/// Crops and mask shapes are kept in pixels of the photo they were drawn
/// on. Pasted onto a photo of another size or shape (`from` → `to`, each a
/// frame's width and height) they are moved to the same place in its
/// frame: positions as a share of the frame, sizes (radii, brush widths,
/// a gradient's range) by the change in scale. A crop locked to an aspect
/// ratio keeps that ratio, as large as fits where the share puts it; a
/// free crop takes the same share of each side.
fn fit_pasted_shapes(
    pasted: &mut serde_json::Map<String, Value>,
    from: (f64, f64),
    to: (f64, f64),
) {
    let (sx, sy) = (to.0 / from.0, to.1 / from.1);
    let scale = (sx * sy).sqrt();
    let aspect_locked = pasted
        .get("aspectRatio")
        .and_then(Value::as_f64)
        .is_some_and(|a| a > 0.0);

    if let Some(crop) = pasted.get_mut("crop").and_then(Value::as_object_mut)
        && crop.get("unit").and_then(Value::as_str) != Some("%")
    {
        let num = |k: &str| crop.get(k).and_then(Value::as_f64);
        if let (Some(x), Some(y), Some(w), Some(h)) =
            (num("x"), num("y"), num("width"), num("height"))
            && w > 0.0
            && h > 0.0
        {
            let (mut tw, mut th) = (w * sx, h * sy);
            if aspect_locked {
                let aspect = w / h;
                if tw / th > aspect {
                    tw = th * aspect;
                } else {
                    th = tw / aspect;
                }
            }
            let (tw, th) = (tw.min(to.0), th.min(to.1));
            let cx = (x + w / 2.0) * sx;
            let cy = (y + h / 2.0) * sy;
            let tx = (cx - tw / 2.0).clamp(0.0, to.0 - tw);
            let ty = (cy - th / 2.0).clamp(0.0, to.1 - th);
            for (k, v) in [("x", tx), ("y", ty), ("width", tw), ("height", th)] {
                crop.insert(k.to_string(), serde_json::json!(v));
            }
        }
    }

    let Some(masks) = pasted.get_mut("masks").and_then(Value::as_array_mut) else {
        return;
    };
    let scale_by = |v: &mut Value, k: &str, by: f64| {
        if let Some(n) = v.get(k).and_then(Value::as_f64) {
            v[k] = serde_json::json!(n * by);
        }
    };
    for container in masks.iter_mut() {
        let Some(sub_masks) = container.get_mut("subMasks").and_then(Value::as_array_mut) else {
            continue;
        };
        for sub_mask in sub_masks.iter_mut() {
            let Some(params) = sub_mask.get_mut("parameters").filter(|p| p.is_object()) else {
                continue;
            };
            for k in ["centerX", "startX", "endX", "targetX"] {
                scale_by(params, k, sx);
            }
            for k in ["centerY", "startY", "endY", "targetY"] {
                scale_by(params, k, sy);
            }
            for k in ["radiusX", "radiusY", "range"] {
                scale_by(params, k, scale);
            }
            if let Some(samples) = params.get_mut("samples").and_then(Value::as_array_mut) {
                for point in samples.iter_mut() {
                    scale_by(point, "x", sx);
                    scale_by(point, "y", sy);
                }
            }
            if let Some(lines) = params.get_mut("lines").and_then(Value::as_array_mut) {
                for line in lines.iter_mut() {
                    scale_by(line, "brushSize", scale);
                    if let Some(points) = line.get_mut("points").and_then(Value::as_array_mut) {
                        for point in points.iter_mut() {
                            scale_by(point, "x", sx);
                            scale_by(point, "y", sy);
                        }
                    }
                }
            }
        }
    }
}

/// Leave out of pasted masks the AI selections made of the source photo
/// (a subject, the sky, a depth map): their mattes are of its picture, not
/// another's. A mask left with no shapes goes too. Returns how many were
/// left out.
fn drop_pasted_mattes(pasted: &mut serde_json::Map<String, Value>) -> usize {
    let Some(masks) = pasted.get_mut("masks").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut dropped = 0;
    masks.retain_mut(|container| {
        let Some(sub_masks) = container.get_mut("subMasks").and_then(Value::as_array_mut) else {
            return true;
        };
        let before = sub_masks.len();
        sub_masks.retain(|sub_mask| {
            sub_mask["parameters"]["maskDataBase64"]
                .as_str()
                .is_none_or(str::is_empty)
        });
        dropped += before - sub_masks.len();
        before == 0 || !sub_masks.is_empty()
    });
    dropped
}

#[tauri::command]
pub async fn apply_adjustments_to_paths(
    paths: Vec<String>,
    adjustments: Value,
    source_path: Option<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);

    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    // The edits are saved before this returns (the editor reads back what
    // the paste made of its own photo); the thumbnails follow.
    let app = app_handle.clone();
    let batch_settings = settings.clone();
    let targets = paths.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let lens_db = state.lens_db.lock().unwrap().clone();
        let empty = serde_json::Map::new();
        let pasted_map = adjustments.as_object().unwrap_or(&empty);
        // Shapes are fitted to each photo's frame when the photo they come
        // from is known.
        let source = source_path
            .filter(|_| pastes_shapes(pasted_map))
            .map(|source| {
                let (real, sidecar) = parse_virtual_path(&source);
                let own = crate::exif_processing::load_sidecar(&sidecar).adjustments;
                (real, pasted_frame(&state, &source, &own, pasted_map))
            });

        in_batch_pool(&batch_settings, || {
            targets.par_iter().for_each(|path| {
                let (real_path, sidecar_path) = parse_virtual_path(path);
                let mut pasted = pasted_map.clone();
                if let Some((source_real, source_frame)) = &source
                    && *source_real != real_path
                {
                    let dropped = drop_pasted_mattes(&mut pasted);
                    if dropped > 0 {
                        log::info!(
                            "Pasting onto {path}: {dropped} AI selections of another photo left out"
                        );
                    }
                    let own = crate::exif_processing::load_sidecar(&sidecar_path).adjustments;
                    if let Some(from) = source_frame
                        && let Some(to) = pasted_frame(&state, path, &own, &pasted)
                    {
                        fit_pasted_shapes(&mut pasted, *from, to);
                    }
                }
                // The photo's EXIF, for the lens profile its edit may name,
                // while its sidecar is being saved anyway.
                let exif = crate::exif_processing::exif_for_saving(&real_path);

                let saved = crate::exif_processing::modify_sidecar(&sidecar_path, |existing| {
                    let mut new_adjustments = std::mem::take(&mut existing.adjustments);
                    if new_adjustments.is_null() {
                        new_adjustments = serde_json::json!({});
                    }

                    if let Some(new_map) = new_adjustments.as_object_mut() {
                        for (k, v) in &pasted {
                            if k == "v3" {
                                merge_pasted_v3(new_map.entry("v3").or_insert(Value::Null), v);
                            } else {
                                new_map.insert(k.clone(), v.clone());
                            }
                        }
                    }

                    if existing.exif.is_none() {
                        existing.exif = exif;
                    }
                    resolve_lens_params_in_adjustments(
                        &mut new_adjustments,
                        &existing.exif,
                        lens_db.as_deref(),
                    );

                    existing.adjustments = new_adjustments;

                    if enable_xmp_sync {
                        crate::xmp::push(path, existing, create_xmp_if_missing);
                    }
                    true
                });
                if let Err(e) = saved {
                    log::warn!("Not pasting onto {path}: {e}");
                }
            });
        });
    })
    .await
    .map_err(|e| format!("Task failed: {e}"))?;

    tauri::async_runtime::spawn_blocking(move || {
        rerender_batch_thumbnails(&app_handle, &settings, &paths);
    });
    Ok(())
}

#[tauri::command]
pub async fn reset_adjustments_for_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);

    tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

        paths.par_iter().for_each(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);

            let reset = crate::exif_processing::modify_sidecar(&sidecar_path, |existing| {
                existing.adjustments = serde_json::json!({});
                if enable_xmp_sync {
                    crate::xmp::push(path, existing, create_xmp_if_missing);
                }
                true
            });
            if let Err(e) = reset {
                log::warn!("Not resetting {path}: {e}");
            }
        });

        rerender_batch_thumbnails(&app_handle, &settings, &paths);
    });

    Ok(())
}

#[tauri::command]
pub async fn apply_auto_adjustments_to_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);

    tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

        let state = app_handle.state::<AppState>();
        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!("Unable to initialize thumbnail cache directory: {}", e);
                for path in &paths {
                    emit_thumbnail_cache_setup_error(&app_handle, path, &e);
                }
                for _ in 0..paths.len() {
                    increment_thumbnail_progress(&state, &app_handle);
                }
                return;
            }
        };

        let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();

        // Each photo is decoded, analysed and rendered: at most as many at
        // once as thumbnails are made with.
        in_batch_pool(&settings, || {
            paths.par_iter().for_each(|path| {
                // Writes the auto adjustments into the sidecar; the picture it
                // decoded on the way is not needed again.
                let _: Option<DynamicImage> = (|| -> Result<DynamicImage, String> {
                    let (source_path, sidecar_path) = parse_virtual_path(path);
                    let source_path_str = source_path.to_string_lossy().to_string();

                    let file_bytes = fs::read(&source_path).map_err(|e| e.to_string())?;
                    let image = image_loader::load_base_image_from_bytes(
                        &file_bytes,
                        &source_path_str,
                        true,
                        &settings,
                        None,
                    )
                    .map_err(|e| e.to_string())?;

                    let auto_results = perform_auto_analysis(&image);
                    let auto_adjustments_json = auto_results_to_json(&auto_results);

                    crate::exif_processing::modify_sidecar(&sidecar_path, |existing_metadata| {
                        if existing_metadata.adjustments.is_null() {
                            existing_metadata.adjustments = serde_json::json!({});
                        }

                        if let (Some(existing_map), Some(auto_map)) = (
                            existing_metadata.adjustments.as_object_mut(),
                            auto_adjustments_json.as_object(),
                        ) {
                            for (k, v) in auto_map {
                                if k == "sectionVisibility" {
                                    if let Some(existing_vis_val) = existing_map.get_mut(k) {
                                        if let (Some(existing_vis), Some(auto_vis)) =
                                            (existing_vis_val.as_object_mut(), v.as_object())
                                        {
                                            for (vis_k, vis_v) in auto_vis {
                                                existing_vis.insert(vis_k.clone(), vis_v.clone());
                                            }
                                        }
                                    } else {
                                        existing_map.insert(k.clone(), v.clone());
                                    }
                                } else {
                                    existing_map.insert(k.clone(), v.clone());
                                }
                            }
                        }

                        if enable_xmp_sync {
                            crate::xmp::push(path, existing_metadata, create_xmp_if_missing);
                        }
                        true
                    })?;
                    Ok(image)
                })()
                .map_err(|e| eprintln!("Failed to apply auto adjustments to {}: {}", path, e))
                .ok();

                let result = generate_single_thumbnail_and_cache(
                    path,
                    &thumb_cache_dir,
                    gpu_context.as_ref(),
                    true,
                    &app_handle,
                    &settings,
                );

                if let Some((thumbnail_data, _, is_edited)) = result {
                    emit_thumbnail(&app_handle, path, &thumbnail_data, is_edited);
                }

                increment_thumbnail_progress(&state, &app_handle);
            });
        });
    });

    Ok(())
}

#[tauri::command]

pub fn set_color_label_for_paths(
    paths: Vec<String>,
    color: Option<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    let failures: Vec<String> = paths
        .par_iter()
        .filter_map(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);
            crate::exif_processing::modify_sidecar(&sidecar_path, |metadata| {
                let mut tags = metadata.tags.take().unwrap_or_default();
                tags.retain(|tag| !tag.starts_with(COLOR_TAG_PREFIX));

                if let Some(c) = &color
                    && !c.is_empty()
                {
                    tags.push(format!("{}{}", COLOR_TAG_PREFIX, c));
                }

                if !tags.is_empty() {
                    metadata.tags = Some(tags);
                }

                if !path.contains("?vc=") {
                    crate::finder_tags::set_color_label(
                        Path::new(path),
                        color.as_deref().filter(|c| !c.is_empty()),
                    );
                }

                if enable_xmp_sync {
                    crate::xmp::push(path, metadata, create_xmp_if_missing);
                }
                true
            })
            .err()
        })
        .collect();

    sidecar_failures(failures)
}

#[tauri::command]
pub fn set_rating_for_paths(
    paths: Vec<String>,
    rating: u8,
    app_handle: AppHandle,
) -> Result<(), String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    let failures: Vec<String> = paths
        .par_iter()
        .filter_map(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);
            crate::exif_processing::modify_sidecar(&sidecar_path, |metadata| {
                metadata.rating = rating;
                if enable_xmp_sync {
                    crate::xmp::push(path, metadata, create_xmp_if_missing);
                }
                true
            })
            .err()
        })
        .collect();

    sidecar_failures(failures)
}

/// Flag photos while culling: "pick", "reject", or none to clear.
#[tauri::command]
pub fn set_flag_for_paths(
    paths: Vec<String>,
    flag: Option<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let flag = flag.filter(|f| f == "pick" || f == "reject");
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    let failures: Vec<String> = paths
        .par_iter()
        .filter_map(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);
            crate::exif_processing::modify_sidecar(&sidecar_path, |metadata| {
                metadata.flag = flag.clone();
                if enable_xmp_sync {
                    crate::xmp::push(path, metadata, create_xmp_if_missing);
                }
                true
            })
            .err()
        })
        .collect();

    sidecar_failures(failures)
}

#[tauri::command]
pub fn load_metadata(path: String, app_handle: AppHandle) -> Result<ImageMetadata, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);

    let (_, sidecar_path) = parse_virtual_path(&path);
    // An unreadable sidecar opens as unedited; saving over it is refused.
    Ok(listed_sidecar(&path, &sidecar_path, enable_xmp_sync, None))
}

fn get_presets_path(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    let presets_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("presets");

    if !presets_dir.exists() {
        fs::create_dir_all(&presets_dir).map_err(|e| e.to_string())?;
    }

    Ok(presets_dir.join("presets.json"))
}

#[tauri::command]
pub fn load_presets(app_handle: AppHandle) -> Result<Vec<PresetItem>, String> {
    let path = get_presets_path(&app_handle)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_presets(presets: Vec<PresetItem>, app_handle: AppHandle) -> Result<(), String> {
    let path = get_presets_path(&app_handle)?;
    write_presets_file(&path, &presets)
}

/// Save the presets whole or not at all (a crash mid-save must not leave
/// half a file, and every preset lost). A presets file that cannot be read
/// is kept aside first: the app then shows no presets, and the next save
/// would otherwise replace them all with what is left.
fn write_presets_file(path: &Path, presets: &[PresetItem]) -> Result<(), String> {
    let unreadable = fs::read_to_string(path)
        .ok()
        .filter(|c| !c.trim().is_empty())
        .is_some_and(|c| serde_json::from_str::<Vec<PresetItem>>(&c).is_err());
    if unreadable {
        let stamp = Utc::now().format("%Y%m%d-%H%M%S");
        let aside = path.with_file_name(format!("presets.unreadable-{stamp}.json"));
        fs::copy(path, &aside).map_err(|e| {
            format!("The presets file could not be read, nor kept aside before saving: {e}")
        })?;
        log::warn!(
            "The presets file could not be read; kept as {} before saving",
            aside.display()
        );
    }
    let json_string = serde_json::to_string_pretty(presets).map_err(|e| e.to_string())?;
    crate::exif_processing::write_atomically(path, json_string.as_bytes(), true)
        .map_err(|e| e.to_string())
}

fn get_internal_library_root_path(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    #[cfg(not(target_os = "android"))]
    {
        let library_dir = app_handle
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("library");

        if !library_dir.exists() {
            fs::create_dir_all(&library_dir).map_err(|e| e.to_string())?;
        }
        Ok(library_dir)
    }
    #[cfg(target_os = "android")]
    {
        crate::android_integration::get_android_internal_library_root()
    }
}

#[tauri::command]
pub fn get_or_create_internal_library_root(app_handle: AppHandle) -> Result<String, String> {
    let library_root = get_internal_library_root_path(&app_handle)?;

    Ok(library_root.to_string_lossy().to_string())
}

#[tauri::command]
pub fn handle_import_presets_from_file(
    file_path: String,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    let content =
        fs::read_to_string(file_path).map_err(|e| format!("Failed to read preset file: {}", e))?;
    let imported_preset_file: PresetFile = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse preset file: {}", e))?;

    let mut current_presets = load_presets(app_handle.clone())?;

    let mut current_names: HashSet<String> = current_presets
        .iter()
        .map(|item| match item {
            PresetItem::Preset(p) => p.name.clone(),
            PresetItem::Folder(f) => f.name.clone(),
        })
        .collect();

    for mut imported_item in imported_preset_file.presets {
        let (current_name, _new_id) = match &mut imported_item {
            PresetItem::Preset(p) => {
                p.id = Uuid::new_v4().to_string();
                (p.name.clone(), p.id.clone())
            }
            PresetItem::Folder(f) => {
                f.id = Uuid::new_v4().to_string();
                for child in &mut f.children {
                    child.id = Uuid::new_v4().to_string();
                }
                (f.name.clone(), f.id.clone())
            }
        };

        let mut new_name = current_name.clone();
        let mut counter = 1;
        while current_names.contains(&new_name) {
            new_name = format!("{} ({})", current_name, counter);
            counter += 1;
        }

        match &mut imported_item {
            PresetItem::Preset(p) => p.name = new_name.clone(),
            PresetItem::Folder(f) => f.name = new_name.clone(),
        }

        current_names.insert(new_name);
        current_presets.push(imported_item);
    }

    save_presets(current_presets.clone(), app_handle)?;
    Ok(current_presets)
}

#[tauri::command]
pub fn handle_import_legacy_presets_from_file(
    file_path: String,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    let content = fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read legacy preset file: {}", e))?;

    let xmp_content = if file_path.to_lowercase().ends_with(".lrtemplate") {
        let re = Regex::new(r#"(?s)s.xmp = "(.*)""#).unwrap();
        if let Some(caps) = re.captures(&content) {
            caps.get(1)
                .map(|m| m.as_str().replace(r#"\""#, r#"""#))
                .unwrap_or(content)
        } else {
            content
        }
    } else {
        content
    };

    let converted_preset = preset_converter::convert_xmp_to_preset(&xmp_content)?;

    let mut current_presets = load_presets(app_handle.clone())?;

    let current_names: HashSet<String> = current_presets
        .iter()
        .flat_map(|item| match item {
            PresetItem::Preset(p) => vec![p.name.clone()],
            PresetItem::Folder(f) => {
                let mut names = vec![f.name.clone()];
                names.extend(f.children.iter().map(|c| c.name.clone()));
                names
            }
        })
        .collect();

    let mut new_name = converted_preset.name.clone();
    let mut counter = 1;
    while current_names.contains(&new_name) {
        new_name = format!("{} ({})", converted_preset.name, counter);
        counter += 1;
    }

    let mut final_preset = converted_preset;
    final_preset.name = new_name;

    current_presets.push(PresetItem::Preset(final_preset));

    save_presets(current_presets.clone(), app_handle)?;
    Ok(current_presets)
}

#[tauri::command]
pub fn handle_export_presets_to_file(
    presets_to_export: Vec<PresetItem>,
    file_path: String,
) -> Result<(), String> {
    let preset_file = ExportPresetFile {
        creator: "Anonymous",
        presets: &presets_to_export,
    };

    let json_string = serde_json::to_string_pretty(&preset_file)
        .map_err(|e| format!("Failed to serialize presets: {}", e))?;
    fs::write(file_path, json_string).map_err(|e| format!("Failed to write preset file: {}", e))
}

#[tauri::command]
pub fn save_community_preset(
    name: String,
    adjustments: Value,
    app_handle: AppHandle,
    include_masks: Option<bool>,
    include_crop_transform: Option<bool>,
    preset_type: Option<String>,
) -> Result<(), String> {
    let mut current_presets = load_presets(app_handle.clone())?;

    let community_folder_name = "Community";
    let community_folder_id = match current_presets.iter_mut().find(|item| {
        if let PresetItem::Folder(f) = item {
            f.name == community_folder_name
        } else {
            false
        }
    }) {
        Some(PresetItem::Folder(folder)) => folder.id.clone(),
        _ => {
            let new_folder_id = Uuid::new_v4().to_string();
            let new_folder = PresetItem::Folder(PresetFolder {
                id: new_folder_id.clone(),
                name: community_folder_name.to_string(),
                children: Vec::new(),
            });
            current_presets.insert(0, new_folder);
            new_folder_id
        }
    };

    let new_preset = Preset {
        id: Uuid::new_v4().to_string(),
        name,
        adjustments,
        include_masks,
        include_crop_transform,
        preset_type: preset_type.or(Some("style".to_string())),
    };

    if let Some(PresetItem::Folder(folder)) = current_presets.iter_mut().find(|item| {
        if let PresetItem::Folder(f) = item {
            f.id == community_folder_id
        } else {
            false
        }
    }) {
        folder.children.retain(|p| p.name != new_preset.name);
        folder.children.push(new_preset);
    }

    save_presets(current_presets, app_handle)
}

#[tauri::command]
pub fn clear_all_sidecars(root_path: String) -> Result<usize, String> {
    if !Path::new(&root_path).exists() {
        return Err(format!("Root path does not exist: {}", root_path));
    }

    let mut deleted_count = 0;
    let walker = WalkDir::new(root_path).into_iter();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file()
            && let Some(extension) = path.extension()
            && (extension == "dri" || extension == "rrexif")
        {
            if fs::remove_file(path).is_ok() {
                deleted_count += 1;
            } else {
                eprintln!("Failed to delete sidecar file: {:?}", path);
            }
        }
    }

    Ok(deleted_count)
}

#[tauri::command]
pub fn clear_thumbnail_cache(app_handle: AppHandle) -> Result<(), String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");

    if thumb_cache_dir.exists() {
        fs::remove_dir_all(&thumb_cache_dir)
            .map_err(|e| format!("Failed to remove thumbnail cache: {}", e))?;
    }

    fs::create_dir_all(&thumb_cache_dir)
        .map_err(|e| format!("Failed to recreate thumbnail cache directory: {}", e))?;

    // The grid drops the thumbnails it shows, and asks for them again.
    let _ = app_handle.emit("thumbnail-cache-cleared", ());
    Ok(())
}

#[tauri::command]
pub fn show_in_finder(path: String) -> Result<(), String> {
    let (source_path, _) = parse_virtual_path(&path);

    #[cfg(target_os = "windows")]
    {
        let source_path_str = source_path.to_string_lossy().to_string();
        Command::new("explorer")
            .args(["/select,", &source_path_str])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "macos")]
    {
        let source_path_str = source_path.to_string_lossy().to_string();
        Command::new("open")
            .args(["-R", &source_path_str])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(parent) = source_path.parent() {
            Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map_err(|e| e.to_string())?;
        } else {
            return Err("Could not get parent directory".into());
        }
    }

    #[cfg(target_os = "android")]
    {
        return Err("Show in File Manager is not natively supported via CLI on Android.".into());
    }

    #[cfg(target_os = "ios")]
    {
        return Err("Show in File Manager is not supported on iOS.".into());
    }

    Ok(())
}

/// Send `files` to the Trash as one undoable step, and drop from albums
/// the photos in `deletions` that went. Anything the Trash refused stays
/// where it is (nothing is deleted outright), and the error says so.
fn trash_as_one_step(
    label: String,
    files: HashSet<PathBuf>,
    deletions: HashSet<String>,
    app_handle: &AppHandle,
) -> Result<(), String> {
    let files: Vec<PathBuf> = files.into_iter().collect();
    let (actions, problem) = crate::journal::trash_each(&files);
    crate::journal::record(label, actions);

    let gone: HashSet<String> = deletions
        .into_iter()
        .filter(|p| {
            let (source, sidecar) = parse_virtual_path(p);
            if p.contains("?vc=") {
                !sidecar.exists()
            } else {
                !source.exists()
            }
        })
        .collect();
    if !gone.is_empty() {
        sync_album_path_changes(app_handle, None, Some(&gone), None);
    }

    match problem {
        None => Ok(()),
        Some(problem) => Err(problem),
    }
}

#[tauri::command]
pub fn delete_files_from_disk(paths: Vec<String>, app_handle: AppHandle) -> Result<(), String> {
    let mut files_to_trash = HashSet::new();

    let mut deletions = HashSet::new();

    for path_str in paths {
        let (source_path, sidecar_path) = parse_virtual_path(&path_str);
        deletions.insert(path_str.clone());

        if path_str.contains("?vc=") {
            if sidecar_path.exists() {
                files_to_trash.insert(sidecar_path);
            }
        } else if source_path.exists() {
            files_to_trash.extend(find_all_associated_files(&source_path)?);
        }
    }

    if files_to_trash.is_empty() {
        return Ok(());
    }

    trash_as_one_step(
        format!("Delete {} and associated files", photos(deletions.len())),
        files_to_trash,
        deletions,
        &app_handle,
    )
}

/// The photos in `folder_files` named exactly `stems` (the name without
/// its last extension: `Party 2024.01.05 001` for
/// `Party 2024.01.05 001.jpg`, never every `Party 2024.*`).
fn same_name_photos(folder_files: &[PathBuf], stems: &HashSet<String>) -> Vec<PathBuf> {
    folder_files
        .iter()
        .filter(|f| is_supported_media_file(f))
        .filter(|f| {
            f.file_stem()
                .is_some_and(|s| stems.contains(&*s.to_string_lossy()))
        })
        .cloned()
        .collect()
}

/// Delete photos together with the other photos of exactly the same name
/// (a RAW's JPEG twin, a Live Photo's video), each with everything that
/// travels with it: sidecars, XMP it owns, smart previews.
#[tauri::command]
pub fn delete_files_with_associated(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    let mut files_to_trash = HashSet::new();
    let mut deletions = HashSet::new();
    let mut stems_by_folder: HashMap<PathBuf, HashSet<String>> = HashMap::new();

    for path_str in &paths {
        deletions.insert(path_str.clone());
        let (source_path, sidecar_path) = parse_virtual_path(path_str);
        if path_str.contains("?vc=") {
            // A virtual copy has no file of its own but its sidecar.
            if sidecar_path.exists() {
                files_to_trash.insert(sidecar_path);
            }
            continue;
        }
        if let (Some(parent), Some(stem)) = (source_path.parent(), source_path.file_stem()) {
            stems_by_folder
                .entry(parent.to_path_buf())
                .or_default()
                .insert(stem.to_string_lossy().into_owned());
        }
    }

    let mut photo_count = 0;
    for (folder, stems) in stems_by_folder {
        let folder_files: Vec<PathBuf> = fs::read_dir(&folder)
            .map_err(|e| format!("Could not read {}: {e}", folder.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        for photo in same_name_photos(&folder_files, &stems) {
            files_to_trash.extend(find_all_associated_files(&photo)?);
            deletions.insert(photo.to_string_lossy().into_owned());
            photo_count += 1;
        }
    }

    if files_to_trash.is_empty() {
        return Ok(());
    }

    trash_as_one_step(
        format!("Delete {} with files of the same name", photos(photo_count)),
        files_to_trash,
        deletions,
        &app_handle,
    )
}

pub fn get_thumb_cache_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");
    if !thumb_cache_dir.exists() {
        fs::create_dir_all(&thumb_cache_dir).map_err(|e| e.to_string())?;
    }
    Ok(thumb_cache_dir)
}

pub fn get_cached_or_generate_thumbnail_image(
    path_str: &str,
    app_handle: &AppHandle,
    gpu_context: Option<&GpuContext>,
) -> Result<DynamicImage> {
    let thumb_cache_dir = get_thumb_cache_dir(app_handle).map_err(|e| anyhow::anyhow!(e))?;
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let target_width = thumbnail_width(&settings);
    let keys = ThumbnailKeys::new(&app_handle.state::<AppState>(), &settings);
    let edit = ThumbnailEdit::read(path_str, &keys);
    let render = || {
        generate_thumbnail_data(
            path_str,
            &edit.adjustments,
            gpu_context,
            app_handle,
            target_width,
        )
    };

    if let Some(cache_hash) = keys.key(path_str, &edit.bytes) {
        let cache_filename = format!("{}.jpg", cache_hash);
        let cache_path = thumb_cache_dir.join(cache_filename);

        if cache_path.exists() {
            if let Ok(image) = image::open(&cache_path) {
                touch_cached_thumbnail(&cache_path);
                return Ok(image);
            }
            eprintln!(
                "Could not open cached thumbnail, regenerating: {:?}",
                cache_path
            );
        }

        let thumb_image = render()?;
        let thumb_data = encode_thumbnail(&thumb_image, target_width)?;
        write_cached_thumbnail(&cache_path, &thumb_data);

        Ok(thumb_image)
    } else {
        render()
    }
}

#[tauri::command]
pub async fn import_files(
    source_paths: Vec<String>,
    destination_folder: String,
    settings: ImportSettings,
    app_handle: AppHandle,
) -> Result<(), String> {
    let total_files = source_paths.len();
    let _ = app_handle.emit("import-start", serde_json::json!({ "total": total_files }));

    tauri::async_runtime::spawn_blocking(move || {
        // One file failing (a locked file, a full disk for one shot) does
        // not stop the rest; a summary at the end says what happened.
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut failed: Vec<String> = Vec::new();
        let mut actions = Vec::new();
        let mut imported_paths: Vec<String> = Vec::new();
        for (i, source_path_str) in source_paths.iter().enumerate() {
            let _ = app_handle.emit(
                "import-progress",
                serde_json::json!({ "current": i, "total": total_files, "path": source_path_str }),
            );

            let import_result: Result<Option<PathBuf>, String> = (|| {
                #[cfg(target_os = "android")]
                if is_android_content_uri(source_path_str) {
                    let resolved_name = resolve_android_content_uri_name(source_path_str)?;
                    let source_bytes = read_android_content_uri(source_path_str)?;
                    let source_name_path = Path::new(&resolved_name);
                    let file_date = exif_processing::get_creation_date_from_bytes(
                        &resolved_name,
                        &source_bytes,
                    );

                    let mut final_dest_folder = PathBuf::from(&destination_folder);
                    if settings.organize_by_date {
                        let date_format_str = settings
                            .date_folder_format
                            .replace("YYYY", "%Y")
                            .replace("MM", "%m")
                            .replace("DD", "%d");
                        let subfolder = file_date.format(&date_format_str).to_string();
                        final_dest_folder.push(subfolder);
                    }

                    fs::create_dir_all(&final_dest_folder)
                        .map_err(|e| format!("Failed to create destination folder: {}", e))?;

                    let new_stem = generate_filename_from_template(
                        &settings.filename_template,
                        source_name_path,
                        i + 1,
                        total_files,
                        &file_date,
                    );
                    let extension = source_name_path
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    let new_filename = format!("{}.{}", new_stem, extension);
                    let dest_file_path = final_dest_folder.join(new_filename);

                    if dest_file_path.exists() {
                        return Err(format!(
                            "File already exists at destination: {}",
                            dest_file_path.display()
                        ));
                    }

                    fs::write(&dest_file_path, source_bytes).map_err(|e| e.to_string())?;

                    if settings.delete_after_import {
                        log::info!(
                            "Skipping delete_after_import for Android content URI source: {}",
                            source_path_str
                        );
                    }

                    return Ok(Some(dest_file_path));
                }

                let (source_path, _) = parse_virtual_path(source_path_str);
                if !source_path.exists() {
                    return Err(format!("Source file not found: {}", source_path_str));
                }

                let file_date = exif_processing::get_creation_date_from_path(&source_path);

                let mut final_dest_folder = PathBuf::from(&destination_folder);
                if settings.organize_by_date {
                    let date_format_str = settings
                        .date_folder_format
                        .replace("YYYY", "%Y")
                        .replace("MM", "%m")
                        .replace("DD", "%d");
                    let subfolder = file_date.format(&date_format_str).to_string();
                    final_dest_folder.push(subfolder);
                }

                fs::create_dir_all(&final_dest_folder)
                    .map_err(|e| format!("Failed to create destination folder: {}", e))?;

                let new_stem = generate_filename_from_template(
                    &settings.filename_template,
                    &source_path,
                    i + 1,
                    total_files,
                    &file_date,
                );
                let extension = source_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let new_filename = format!("{}.{}", new_stem, extension);
                let dest_file_path = final_dest_folder.join(new_filename);

                if dest_file_path.exists() {
                    // Same name and size: imported before, skip it quietly.
                    let same_size = fs::metadata(&dest_file_path)
                        .ok()
                        .zip(fs::metadata(&source_path).ok())
                        .is_some_and(|(a, b)| a.len() == b.len());
                    if same_size {
                        return Ok(None);
                    }
                    return Err(format!(
                        "A different file is already at {}",
                        dest_file_path.display()
                    ));
                }

                fs::copy(&source_path, &dest_file_path).map_err(|e| e.to_string())?;
                // Everything that travels with the photo comes too, renamed to
                // match: this app's sidecars, and Lightroom's (or another
                // editor's) settings, so its edits can be brought over.
                let companions = companion_files(&source_path);
                for companion in &companions {
                    let target = companion_target(companion, &source_path, &dest_file_path);
                    if !target.exists() {
                        fs::copy(companion, &target).map_err(|e| e.to_string())?;
                    }
                }

                if settings.delete_after_import {
                    // To the Trash only: where there is none, the originals
                    // stay (the import itself has succeeded).
                    let mut originals = vec![source_path.clone()];
                    originals.extend(companions.iter().filter(|c| c.exists()).cloned());
                    if let (_, Some(problem)) = crate::journal::trash_each(&originals) {
                        log::warn!("Imported, but kept the original: {problem}");
                    }
                }

                Ok(Some(dest_file_path))
            })();

            match import_result {
                Ok(Some(dest)) => {
                    imported += 1;
                    imported_paths.push(dest.to_string_lossy().into_owned());
                    actions.push(crate::journal::Action::Created { path: dest });
                }
                Ok(None) => skipped += 1,
                Err(e) => {
                    log::warn!("Failed to import {}: {}", source_path_str, e);
                    let name = Path::new(source_path_str)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| source_path_str.clone());
                    failed.push(format!("{name}: {e}"));
                }
            }
        }

        crate::journal::record(format!("Import {}", photos(imported)), actions);
        let _ = app_handle.emit(
            "import-progress",
            serde_json::json!({ "current": total_files, "total": total_files, "path": "" }),
        );
        if imported == 0 && !failed.is_empty() {
            let _ = app_handle.emit("import-error", failed.join("\n"));
        } else {
            let _ = app_handle.emit(
                "import-complete",
                serde_json::json!({
                    "imported": imported,
                    "skipped": skipped,
                    "failed": failed,
                    "paths": imported_paths,
                }),
            );
        }
    });

    Ok(())
}

pub fn generate_filename_from_template(
    template: &str,
    original_path: &std::path::Path,
    sequence: usize,
    total: usize,
    file_date: &DateTime<Utc>,
) -> String {
    let stem = original_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let sequence_str = format!(
        "{:0width$}",
        sequence,
        width = total.to_string().len().max(1)
    );
    let local_date = file_date.with_timezone(&chrono::Local);

    let mut result = template.to_string();
    result = result.replace("{original_filename}", stem);
    result = result.replace("{sequence}", &sequence_str);
    result = result.replace("{YYYY}", &local_date.format("%Y").to_string());
    result = result.replace("{MM}", &local_date.format("%m").to_string());
    result = result.replace("{DD}", &local_date.format("%d").to_string());
    result = result.replace("{hh}", &local_date.format("%H").to_string());
    result = result.replace("{mm}", &local_date.format("%M").to_string());

    result
}

#[tauri::command]
pub fn rename_files(
    paths: Vec<String>,
    name_template: String,
    app_handle: AppHandle,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Each photo once (a virtual copy renames its photo), in the order given.
    let mut photo_renames: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(paths.len());
    for (i, path_str) in paths.iter().enumerate() {
        let (original_path, _) = parse_virtual_path(path_str);
        if !original_path.exists() {
            return Err(format!("File not found: {}", path_str));
        }
        if photo_renames.iter().any(|(o, _)| *o == original_path) {
            continue;
        }

        let parent = original_path
            .parent()
            .ok_or("Could not get parent directory")?;
        let extension = original_path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let file_date = exif_processing::get_creation_date_from_path(&original_path);

        let new_stem = generate_filename_from_template(
            &name_template,
            &original_path,
            i + 1,
            paths.len(),
            &file_date,
        );
        let new_filename = format!("{}.{}", new_stem, extension);
        let new_path = parent.join(new_filename);
        photo_renames.push((original_path, new_path));
    }

    // Everything that travels with each photo follows its new name, and
    // the whole batch is checked first: no new name may already be taken
    // (another photo's sidecar included) or be given twice.
    let mut moves = Vec::new();
    for (original_path, new_path) in &photo_renames {
        if new_path == original_path {
            continue;
        }
        moves.push((original_path.clone(), new_path.clone()));
        for companion in companion_files(original_path) {
            let target = companion_target(&companion, original_path, new_path);
            if target != companion {
                moves.push((companion, target));
            }
        }
    }
    check_move_targets(&moves)?;

    let label = format!("Rename {}", photos(photo_renames.len()));
    let done = crate::journal::move_all(&moves)
        .map_err(|(error, stranded)| record_partial(&label, &stranded, &app_handle, error))?;

    for (old, new) in &photo_renames {
        if old != new {
            relink_versions(old, new);
        }
    }
    crate::journal::record(
        label,
        done.iter().map(crate::journal::Done::action).collect(),
    );

    let renames: HashMap<String, String> = done
        .iter()
        .map(|d| {
            (
                d.from.to_string_lossy().into_owned(),
                d.to.to_string_lossy().into_owned(),
            )
        })
        .collect();
    sync_album_path_changes(&app_handle, Some(&renames), None, None);

    Ok(photo_renames
        .into_iter()
        .map(|(_, new)| new)
        .filter(|new| is_supported_image_file(new))
        .map(|new| new.to_string_lossy().into_owned())
        .collect())
}

#[tauri::command]
pub fn create_virtual_copy(
    source_virtual_path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let (source_path, source_sidecar_path) = parse_virtual_path(&source_virtual_path);

    let new_copy_id = Uuid::new_v4().to_string()[..6].to_string();
    let new_virtual_path = format!("{}?vc={}", source_path.to_string_lossy(), new_copy_id);
    let (_, new_sidecar_path) = parse_virtual_path(&new_virtual_path);

    if source_sidecar_path.exists() {
        fs::copy(&source_sidecar_path, &new_sidecar_path)
            .map_err(|e| format!("Failed to copy sidecar file: {}", e))?;
    } else {
        crate::exif_processing::write_sidecar(&new_sidecar_path, &ImageMetadata::default())?;
    }
    crate::journal::record(
        "Create virtual copy",
        vec![crate::journal::Action::Created {
            path: new_sidecar_path,
        }],
    );

    if let Some(album_id) = target_album_id {
        let _ = add_to_album(album_id, vec![new_virtual_path.clone()], app_handle);
    }

    Ok(new_virtual_path)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoOutcome {
    /// What was undone, e.g. "Move 3 photos to Tahiti".
    pub label: String,
    /// Anything that could not be put back.
    pub problems: Vec<String>,
}

/// Undo the most recent file operation (move, rename, copy, delete…).
#[tauri::command]
pub fn undo_file_operation(app_handle: AppHandle) -> Result<UndoOutcome, String> {
    let report = crate::journal::undo_last()?;
    let mut renames = HashMap::new();
    for (gone, back) in &report.moved_back {
        let (gone_s, back_s) = (
            gone.to_string_lossy().into_owned(),
            back.to_string_lossy().into_owned(),
        );
        if back.is_dir() {
            sync_album_path_changes(&app_handle, None, None, Some((&gone_s, &back_s)));
        } else if is_supported_media_file(back) {
            if gone.parent() == back.parent() {
                relink_versions(gone, back);
            }
            renames.insert(gone_s, back_s);
        }
    }
    if !renames.is_empty() {
        sync_album_path_changes(&app_handle, Some(&renames), None, None);
    }
    Ok(UndoOutcome {
        label: report.label,
        problems: report.problems,
    })
}

/// The session's file operations, newest first.
#[tauri::command]
pub fn file_operation_history() -> Vec<crate::journal::HistoryEntry> {
    crate::journal::history()
}

/// Follow changes made outside the app under these library roots.
#[tauri::command]
pub fn watch_library_roots(roots: Vec<String>, app_handle: AppHandle) -> Result<(), String> {
    // Before the library has loaded its roots, watch the saved ones.
    let roots = if roots.is_empty() {
        load_settings(app_handle.clone())
            .map(|s| s.root_folders)
            .unwrap_or_default()
    } else {
        roots
    };
    let roots: Vec<PathBuf> = roots.into_iter().map(PathBuf::from).collect();
    crate::watcher::watch_roots(app_handle, &roots)
}

/// The folder the library has open (checked by polling on network shares).
#[tauri::command]
pub fn watch_open_folder(path: Option<String>, app_handle: AppHandle) {
    crate::watcher::watch_open_folder(app_handle, path.map(PathBuf::from));
}

#[cfg(test)]
mod companion_tests {
    use super::*;

    fn touch(p: &Path) {
        fs::write(p, p.file_name().unwrap().to_string_lossy().as_bytes()).unwrap();
    }

    #[test]
    fn sidecars_follow_the_new_name() {
        let d = PathBuf::from("/p");
        let old = d.join("DSC1.ARW");
        let new = d.join("Tahiti_001.ARW");
        let t = |c: &str| companion_target(&d.join(c), &old, &new);
        assert_eq!(t("DSC1.ARW.dri"), d.join("Tahiti_001.ARW.dri"));
        assert_eq!(
            t("DSC1.ARW.a1b2c3.dri"),
            d.join("Tahiti_001.ARW.a1b2c3.dri")
        );
        assert_eq!(t("DSC1.xmp"), d.join("Tahiti_001.xmp"));
        assert_eq!(t("DSC1.ARW.xmp"), d.join("Tahiti_001.ARW.xmp"));
        let elsewhere = PathBuf::from("/q/DSC1.ARW");
        assert_eq!(
            companion_target(&d.join("DSC1.xmp"), &old, &elsewhere),
            PathBuf::from("/q/DSC1.xmp")
        );
    }

    #[test]
    fn other_editors_sidecars_travel_too() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in [
            "DSC2.ARW",
            "DSC2.xmp",
            "DSC2.ARW.xmp",
            "DSC2.ARW.pp3",
            "DSC2.ARW.dop",
            "DSC2.on1",
            "DSC20.ARW.pp3",
        ] {
            touch(&p(n));
        }
        let mut found = companion_files(&p("DSC2.ARW"));
        found.sort();
        let mut expected = vec![
            p("DSC2.xmp"),
            p("DSC2.ARW.xmp"),
            p("DSC2.ARW.pp3"),
            p("DSC2.ARW.dop"),
            p("DSC2.on1"),
        ];
        expected.sort();
        assert_eq!(found, expected);
        // Renamed on import, each keeps its tie to the photo.
        let new = p("Tahiti_001.ARW");
        let names: Vec<_> = expected
            .iter()
            .map(|c| {
                companion_target(c, &p("DSC2.ARW"), &new)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        for n in [
            "Tahiti_001.xmp",
            "Tahiti_001.ARW.xmp",
            "Tahiti_001.ARW.pp3",
            "Tahiti_001.ARW.dop",
            "Tahiti_001.on1",
        ] {
            assert!(names.contains(&n.to_string()), "{n} in {names:?}");
        }
    }

    #[test]
    fn a_raw_takes_its_xmp_and_a_jpeg_twin_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in [
            "DSC1.ARW",
            "DSC1.JPG",
            "DSC1.xmp",
            "DSC1.ARW.dri",
            "DSC1.ARW.a1b2c3.dri",
            "DSC1.JPG.dri",
            "DSC10.ARW.dri",
        ] {
            touch(&p(n));
        }
        let mut raw = companion_files(&p("DSC1.ARW"));
        raw.sort();
        assert_eq!(
            raw,
            [p("DSC1.ARW.a1b2c3.dri"), p("DSC1.ARW.dri"), p("DSC1.xmp")]
        );
        assert_eq!(companion_files(&p("DSC1.JPG")), [p("DSC1.JPG.dri")]);
    }

    #[test]
    fn copying_into_the_same_folder_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in ["DSC1.ARW", "DSC1.xmp", "DSC1.ARW.dri"] {
            touch(&p(n));
        }
        let folder = dir.path().to_string_lossy().to_string();
        let made =
            copy_files(vec![p("DSC1.ARW").to_string_lossy().into()], folder.clone()).unwrap();
        assert_eq!(made, [p("DSC1_copy_1.ARW").to_string_lossy().to_string()]);
        assert!(p("DSC1_copy_1.xmp").exists());
        assert!(p("DSC1_copy_1.ARW.dri").exists());
        // The originals are untouched.
        assert_eq!(fs::read_to_string(p("DSC1.xmp")).unwrap(), "DSC1.xmp");
        copy_files(vec![p("DSC1.ARW").to_string_lossy().into()], folder).unwrap();
        assert!(p("DSC1_copy_2.ARW").exists());
    }

    #[test]
    fn moving_on_one_volume_is_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.ARW");
        let dst = dir.path().join("sub");
        fs::create_dir(&dst).unwrap();
        touch(&src);
        assert!(
            !crate::journal::move_or_copy(&src, &dst.join("a.ARW")).unwrap(),
            "renamed, not copied"
        );
        assert!(!src.exists() && dst.join("a.ARW").exists());
    }

    #[test]
    fn versions_follow_a_renamed_original() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        touch(&p("DSC1_Restored.png"));
        crate::versions::record_version(&p("DSC1_Restored.png"), &p("DSC1.ARW"), "Restored");
        relink_versions(&p("DSC1.ARW"), &p("Tahiti_001.ARW"));
        let meta = crate::exif_processing::load_sidecar(&p("DSC1_Restored.png.dri"));
        assert_eq!(meta.derived_from.as_deref(), Some("Tahiti_001.ARW"));
    }
}

#[cfg(test)]
mod folder_tree_tests {
    use super::*;

    /// Home's shallow tree lists two levels below a library folder without
    /// looking inside the second level (each look lists a cloud folder from
    /// its server); the full tree still says which have subfolders.
    #[test]
    fn a_shallow_tree_does_not_look_below_two_levels() {
        let root = std::env::temp_dir().join("darkroom-folder-tree-test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("2022/trip/day one")).unwrap();
        let root_str = root.to_string_lossy().into_owned();
        let trip = |check_unscanned| {
            let tree = get_folder_tree_sync(
                root_str.clone(),
                vec![root_str.clone()],
                false,
                check_unscanned,
            )
            .unwrap();
            let year = tree.children.iter().find(|c| c.name == "2022").unwrap();
            let trip = year.children.iter().find(|c| c.name == "trip").unwrap();
            trip.has_subdirs
        };
        assert!(trip(true));
        assert!(!trip(false));
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod paste_tests {
    use super::*;

    /// Pasting Warmth keeps the photo's own sharpening and vignette, and a
    /// pasted vignette amount keeps its own vignette shape.
    #[test]
    fn pasted_v3_settings_merge_into_the_photos_own() {
        let mut photo = serde_json::json!({
            "temperature": 0, "tint": 0,
            "detail": {"sharpening": 40, "clarity": 10},
            "effects": {"vignette_amount": -20, "vignette_feather": 80},
        });
        merge_pasted_v3(
            &mut photo,
            &serde_json::json!({"temperature": 25, "tint": -8, "effects": {"vignette_amount": -40}}),
        );
        assert_eq!(photo["temperature"], 25);
        assert_eq!(photo["tint"], -8);
        assert_eq!(photo["detail"]["sharpening"], 40);
        assert_eq!(photo["detail"]["clarity"], 10);
        assert_eq!(photo["effects"]["vignette_amount"], -40);
        assert_eq!(photo["effects"]["vignette_feather"], 80);

        let mut fresh = Value::Null;
        merge_pasted_v3(&mut fresh, &serde_json::json!({"temperature": 25}));
        assert_eq!(fresh, serde_json::json!({"temperature": 25}));
    }
}

#[cfg(test)]
mod library_safety_tests {
    use super::*;

    fn touch(p: &Path) {
        fs::write(p, b"x").unwrap();
    }

    /// Sidecar names after the `.dri` suffix replaced the 7-letter
    /// `.rrdata`: copies are found again, and short names never panic.
    #[test]
    fn sidecar_names_split_into_photo_and_copy() {
        assert_eq!(split_sidecar_name("IMG.CR3.dri"), Some(("IMG.CR3", None)));
        assert_eq!(
            split_sidecar_name("IMG.CR3.a1b2c3.dri"),
            Some(("IMG.CR3", Some("a1b2c3")))
        );
        assert_eq!(split_sidecar_name("a.dri"), Some(("a", None)));
        assert_eq!(split_sidecar_name(".dri"), None);
        assert_eq!(split_sidecar_name("dri"), None);
        assert_eq!(split_sidecar_name("IMG.jpg"), None);
        // Not six hex digits: part of the photo's own name.
        assert_eq!(
            split_sidecar_name("IMG.v2.jpg.dri"),
            Some(("IMG.v2.jpg", None))
        );
        assert_eq!(split_sidecar_name("é.dri"), Some(("é", None)));
    }

    /// Only exactly the same name goes: `Party 2024.01.05 001.jpg` takes
    /// its RAW twin, never every `Party 2024.*` photo.
    #[test]
    fn same_name_means_the_whole_name_without_its_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        let files: Vec<PathBuf> = [
            "Party 2024.01.05 001.jpg",
            "Party 2024.01.05 001.ARW",
            "Party 2024.01.05 002.jpg",
            "Party 2024.jpg",
            "Party 2024.01.05 001.jpg.dri",
        ]
        .iter()
        .map(|n| p(n))
        .collect();
        let stems: HashSet<String> = ["Party 2024.01.05 001".to_string()].into();
        let mut found = same_name_photos(&files, &stems);
        found.sort();
        assert_eq!(
            found,
            [p("Party 2024.01.05 001.ARW"), p("Party 2024.01.05 001.jpg")]
        );
    }

    /// A cloud photo's smart preview travels with it; a photo whose name
    /// starts with another's never has its sidecar taken.
    #[test]
    fn companions_include_the_smart_preview_and_nothing_of_others() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        for n in [
            "IMG.jpg",
            "IMG.jpg.dri",
            "IMG.jpg.a1b2c3.dri",
            "IMG.jpg.dripreview",
            "IMG.jpg.png",
            "IMG.jpg.png.dri",
        ] {
            touch(&p(n));
        }
        let mut found = companion_files(&p("IMG.jpg"));
        found.sort();
        assert_eq!(
            found,
            [
                p("IMG.jpg.a1b2c3.dri"),
                p("IMG.jpg.dri"),
                p("IMG.jpg.dripreview")
            ]
        );
    }

    /// A batch is refused before anything moves when a new name is taken
    /// (another photo's sidecar included) or given twice.
    #[test]
    fn batch_targets_are_checked_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        touch(&p("A.ARW"));
        touch(&p("B.ARW"));
        touch(&p("NEW.xmp"));

        assert!(check_move_targets(&[(p("A.ARW"), p("C.ARW"))]).is_ok());
        assert!(check_move_targets(&[(p("A.ARW"), p("B.ARW"))]).is_err());
        assert!(check_move_targets(&[(p("A.xmp"), p("NEW.xmp"))]).is_err());
        assert!(check_move_targets(&[(p("A.ARW"), p("C.ARW")), (p("B.ARW"), p("c.arw"))]).is_err());
    }
}

#[cfg(test)]
mod library_grid_tests {
    use super::*;

    fn keys() -> ThumbnailKeys {
        ThumbnailKeys::new(&AppState::default(), &AppSettings::default())
    }

    /// A thumbnail's key follows the photo through a rename or a move, and
    /// changes with its edit, its contents and the size thumbnails are
    /// made at; two photos never share one.
    #[test]
    fn thumbnail_keys_follow_the_photo_not_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.JPG");
        let b = dir.path().join("B.JPG");
        fs::write(&a, b"one photo").unwrap();
        fs::write(&b, b"one photo").unwrap();
        let k = keys();
        let key = |p: &Path, edit: &[u8]| k.key(&p.to_string_lossy(), edit).unwrap();

        let before = key(&a, b"{}");
        assert_eq!(before, key(&a, b"{}"));
        assert_ne!(
            before,
            key(&b, b"{}"),
            "two files, even alike, have two keys"
        );
        assert_ne!(
            before,
            key(&a, br#"{"exposure":1}"#),
            "an edit is a new key"
        );
        let copy = format!("{}?vc=a1b2c3", a.to_string_lossy());
        assert_ne!(
            before,
            k.key(&copy, b"{}").unwrap(),
            "a virtual copy has its own"
        );

        // Renamed and moved into another folder: the same photo.
        let moved_dir = dir.path().join("Renamed folder");
        fs::create_dir(&moved_dir).unwrap();
        let moved = moved_dir.join("A2.JPG");
        fs::rename(&a, &moved).unwrap();
        assert_eq!(before, key(&moved, b"{}"));

        // Rewritten (even to the same length): a new key.
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&moved, b"two photo").unwrap();
        assert_ne!(before, key(&moved, b"{}"));

        // Thumbnails made at another size are not served.
        let bigger = ThumbnailKeys::new(
            &AppState::default(),
            &AppSettings {
                thumbnail_resolution: Some(1440),
                ..AppSettings::default()
            },
        );
        assert_ne!(
            key(&b, b"{}"),
            bigger.key(&b.to_string_lossy(), b"{}").unwrap()
        );
        assert!(k.key(&a.to_string_lossy(), b"{}").is_none(), "gone");
    }

    /// Another rendering transform installed is a new key for every photo.
    #[test]
    fn thumbnail_keys_follow_the_installed_transform() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("P.JPG");
        let cube = dir.path().join("output.cube");
        fs::write(&photo, b"p").unwrap();
        fs::write(&cube, b"LUT_3D_SIZE 2").unwrap();
        let state = AppState::default();
        let settings = AppSettings::default();
        let path = photo.to_string_lossy();
        let without = ThumbnailKeys::new(&state, &settings)
            .key(&path, b"")
            .unwrap();
        *state.output_transform.lock().unwrap() = Some(cube.clone());
        let with = ThumbnailKeys::new(&state, &settings)
            .key(&path, b"")
            .unwrap();
        assert_ne!(without, with);
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&cube, b"LUT_3D_SIZE 3").unwrap();
        let replaced = ThumbnailKeys::new(&state, &settings)
            .key(&path, b"")
            .unwrap();
        assert_ne!(with, replaced);
    }

    #[test]
    fn the_cache_keeps_what_was_used_lately_within_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now();
        let day = std::time::Duration::from_secs(24 * 60 * 60);
        let put = |name: &str, days_ago: u32| {
            let p = dir.path().join(name);
            fs::write(&p, vec![0u8; 100]).unwrap();
            fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(now - day * days_ago)
                .unwrap();
            p
        };
        let ancient = put("ancient.jpg", 200);
        let old = put("old.jpg", 30);
        let older = put("older.jpg", 40);
        let recent = put("recent.jpg", 1);
        let today = put("today.jpg", 0);
        let other = put("notes.txt", 300);

        // Under the size: only the one unused for months goes.
        let (removed, _) = prune_thumbnail_cache(dir.path(), 10_000, THUMBNAIL_CACHE_MAX_AGE, now);
        assert_eq!(removed, 1);
        assert!(!ancient.exists() && old.exists() && other.exists());

        // Over it: the least recently used go until well under.
        let (removed, freed) = prune_thumbnail_cache(dir.path(), 350, THUMBNAIL_CACHE_MAX_AGE, now);
        assert_eq!((removed, freed), (2, 200));
        assert!(!older.exists() && !old.exists());
        assert!(recent.exists() && today.exists());
    }

    /// A listing leaves out dot files (`._DSC1.ARW` from a card) and, when
    /// recursive, hidden folders.
    #[test]
    fn listings_leave_out_hidden_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for n in ["DSC1.ARW", "._DSC1.ARW", ".hidden.jpg", "IMG.jpg"] {
            fs::write(root.join(n), b"x").unwrap();
        }
        fs::create_dir_all(root.join("Sub")).unwrap();
        fs::create_dir_all(root.join(".Trashes")).unwrap();
        fs::write(root.join("Sub/._IMG2.JPG"), b"x").unwrap();
        fs::write(root.join("Sub/IMG2.JPG"), b"x").unwrap();
        fs::write(root.join(".Trashes/DSC9.ARW"), b"x").unwrap();
        let settings = AppSettings {
            enable_xmp_sync: Some(false),
            ..AppSettings::default()
        };
        let root_str = root.to_string_lossy().into_owned();
        let names = |list: Vec<ImageFile>| {
            let mut n: Vec<String> = list
                .iter()
                .map(|f| f.path.strip_prefix(root_str.as_str()).unwrap().to_string())
                .collect();
            n.sort();
            n
        };
        assert_eq!(
            names(list_folder(root, &settings).unwrap()),
            ["/DSC1.ARW", "/IMG.jpg"]
        );
        assert_eq!(
            names(list_tree(root, &settings)),
            ["/DSC1.ARW", "/IMG.jpg", "/Sub/IMG2.JPG"]
        );
        let until = std::time::Instant::now() + FOLDER_COUNT_BUDGET;
        assert_eq!(unscanned_image_count(root, 0, until), 3);
        // A listing writes nothing into the folder.
        assert!(!root.join("IMG.jpg.dri").exists());
    }

    /// A listing returns the EXIF a sidecar already holds.
    #[test]
    fn listings_carry_the_sidecars_exif() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("IMG.jpg");
        fs::write(&photo, b"x").unwrap();
        let meta = ImageMetadata {
            exif: Some(HashMap::from([("Model".to_string(), "A7".to_string())])),
            ..ImageMetadata::default()
        };
        crate::exif_processing::write_sidecar(&dir.path().join("IMG.jpg.dri"), &meta).unwrap();
        let listed = list_folder(dir.path(), &AppSettings::default()).unwrap();
        assert_eq!(listed[0].exif.as_ref().unwrap()["Model"], "A7");
    }

    fn crop(x: f64, y: f64, w: f64, h: f64) -> Value {
        serde_json::json!({ "x": x, "y": y, "width": w, "height": h, "unit": "px" })
    }

    fn fitted(pasted: Value, from: (f64, f64), to: (f64, f64)) -> serde_json::Map<String, Value> {
        let mut pasted = pasted.as_object().unwrap().clone();
        fit_pasted_shapes(&mut pasted, from, to);
        pasted
    }

    #[test]
    fn a_pasted_crop_takes_the_same_place_in_another_frame() {
        // Twice the resolution, same shape: everything doubles.
        let p = fitted(
            serde_json::json!({ "crop": crop(100.0, 50.0, 300.0, 200.0), "aspectRatio": 1.5 }),
            (600.0, 400.0),
            (1200.0, 800.0),
        );
        assert_eq!(p["crop"], crop(200.0, 100.0, 600.0, 400.0));

        // A 3:2 crop of a landscape pasted onto a portrait keeps its 3:2,
        // as wide as the frame's share allows, centred where it was.
        let p = fitted(
            serde_json::json!({ "crop": crop(0.0, 0.0, 600.0, 400.0), "aspectRatio": 1.5 }),
            (600.0, 400.0),
            (400.0, 600.0),
        );
        let c = &p["crop"];
        let (w, h) = (c["width"].as_f64().unwrap(), c["height"].as_f64().unwrap());
        assert!((w / h - 1.5).abs() < 1e-9 && (w - 400.0).abs() < 1e-9);
        assert!((c["y"].as_f64().unwrap() - (600.0 - h) / 2.0).abs() < 1e-9);

        // A free crop takes the same share of each side.
        let p = fitted(
            serde_json::json!({ "crop": crop(60.0, 40.0, 300.0, 200.0), "aspectRatio": null }),
            (600.0, 400.0),
            (400.0, 600.0),
        );
        for (k, v) in [
            ("x", 40.0),
            ("y", 60.0),
            ("width", 200.0),
            ("height", 300.0),
        ] {
            assert!((p["crop"][k].as_f64().unwrap() - v).abs() < 1e-9, "{k}");
        }

        // The same frame: unchanged. A crop in percent: unchanged.
        let same = serde_json::json!({ "crop": crop(10.0, 20.0, 30.0, 40.0), "aspectRatio": 1.0 });
        assert_eq!(
            fitted(same.clone(), (600.0, 400.0), (600.0, 400.0)),
            *same.as_object().unwrap()
        );
        let percent = serde_json::json!({
            "crop": { "x": 10, "y": 10, "width": 50, "height": 50, "unit": "%" }
        });
        assert_eq!(
            fitted(percent.clone(), (600.0, 400.0), (300.0, 900.0)),
            *percent.as_object().unwrap()
        );
    }

    #[test]
    fn pasted_masks_are_scaled_and_other_photos_mattes_left_out() {
        let masks = serde_json::json!({ "masks": [
            { "id": "m1", "subMasks": [
                { "type": "radial", "parameters": {
                    "centerX": 300.0, "centerY": 200.0, "radiusX": 100.0, "radiusY": 50.0,
                    "rotation": 10.0 } },
                { "type": "linear", "parameters": {
                    "startX": 0.0, "startY": 0.0, "endX": 600.0, "endY": 400.0, "range": 50.0 } },
                { "type": "brush", "parameters": { "lines": [
                    { "brushSize": 20.0, "points": [ { "x": 60.0, "y": 40.0 } ] } ] } },
                { "type": "ai-subject", "parameters": { "startX": 1.0, "maskDataBase64": "AAAA" } }
            ] },
            { "id": "m2", "subMasks": [
                { "type": "ai-sky", "parameters": { "maskDataBase64": "BBBB" } } ] },
            { "id": "m3", "subMasks": [] }
        ] });
        let mut p = fitted(masks, (600.0, 400.0), (1200.0, 800.0));
        let subs = &p["masks"][0]["subMasks"];
        assert_eq!(subs[0]["parameters"]["centerX"], 600.0);
        assert_eq!(subs[0]["parameters"]["radiusY"], 100.0);
        assert_eq!(subs[0]["parameters"]["rotation"], 10.0);
        assert_eq!(subs[1]["parameters"]["endY"], 800.0);
        assert_eq!(subs[1]["parameters"]["range"], 100.0);
        assert_eq!(subs[2]["parameters"]["lines"][0]["points"][0]["x"], 120.0);
        assert_eq!(subs[2]["parameters"]["lines"][0]["brushSize"], 40.0);

        assert_eq!(drop_pasted_mattes(&mut p), 2);
        let masks = p["masks"].as_array().unwrap();
        // The sky-only mask is gone; an empty one stays as it was.
        assert_eq!(masks.len(), 2);
        assert_eq!(masks[0]["subMasks"].as_array().unwrap().len(), 3);
        assert_eq!(masks[1]["id"], "m3");
    }

    /// A presets file that cannot be read is kept before a save replaces it.
    #[test]
    fn an_unreadable_presets_file_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        fs::write(&path, b"[{ not json").unwrap();
        write_presets_file(&path, &[]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), "[]");
        let kept: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("presets.unreadable-")
            })
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(fs::read(kept[0].path()).unwrap(), b"[{ not json");
        // A readable one is simply replaced.
        write_presets_file(&path, &[]).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

#[cfg(test)]
mod listing_bench {
    use super::*;

    /// Times a folder's listing as it is and as it was (each photo's XMP
    /// twins asked of the disk). `DRI_BENCH_DIR`: a folder of JPEGs.
    /// `cargo test --lib listing_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn listing_a_big_folder() {
        let Ok(dir) = std::env::var("DRI_BENCH_DIR") else {
            return;
        };
        let dir = Path::new(&dir);
        let settings = AppSettings::default();
        for round in 0..3 {
            let t = std::time::Instant::now();
            let now = list_folder(dir, &settings).unwrap();
            let new = t.elapsed();

            let t = std::time::Instant::now();
            let files: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .flatten()
                .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                .filter(|(n, _)| is_supported_media_file(n))
                .collect();
            let before: Vec<ImageFile> = files
                .into_par_iter()
                .flat_map(|(name, path)| {
                    // As before: no folder names, so XMP twins are asked of the disk.
                    list_photo(&path, &name, vec![None], &settings, None)
                })
                .collect();
            let old = t.elapsed();
            assert_eq!(now.len(), before.len());
            println!(
                "round {round}: {} photos: listing {new:?} now, {old:?} with per-photo disk checks",
                now.len()
            );
        }
    }
}
