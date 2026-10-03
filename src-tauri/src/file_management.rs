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

fn compute_thumbnail_cache_hash(path_str: &str, adjustments_bytes: &[u8]) -> Option<String> {
    let (source_path, _) = parse_virtual_path(path_str);

    let img_mod_time = fs::metadata(&source_path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"v3-only-thumbnails-2026-10-01-raw-look\0");
    hasher.update(path_str.as_bytes());
    hasher.update(&img_mod_time.to_le_bytes());
    hasher.update(adjustments_bytes);
    Some(hasher.finalize().to_hex().to_string())
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

                let map = if let Some(sidecar_exif) =
                    crate::exif_processing::read_exif_sidecar(&source_path)
                {
                    sidecar_exif
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
        paths.par_iter().for_each(|path| {
            let original_path = Path::new(&path);
            let primary_path = crate::exif_processing::get_primary_sidecar_path(original_path);
            let temp_metadata = crate::exif_processing::load_sidecar(&primary_path);

            let mut exif_data = temp_metadata.exif.unwrap_or_else(|| {
                if let Some(existing) = crate::exif_processing::read_exif_sidecar(original_path) {
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

            let mut final_metadata = crate::exif_processing::load_sidecar(&primary_path);

            final_metadata.exif = Some(exif_data);
            // Caption, author and copyright are shared through XMP.
            if let Some(create_if_missing) = xmp {
                crate::xmp::push(path, &mut final_metadata, create_if_missing);
            }
            if let Ok(json) = serde_json::to_string_pretty(&final_metadata) {
                let _ = std::fs::write(&primary_path, json);
            }
        });
        Ok(())
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub fn list_images_in_dir(path: String, app_handle: AppHandle) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);

    let entries = fs::read_dir(&path).map_err(|e| e.to_string())?;
    let mut images = Vec::new();
    let mut sidecars_by_filename: HashMap<String, Vec<Option<String>>> = HashMap::new();

    for entry in entries.filter_map(Result::ok) {
        let entry_path = entry.path();
        let file_name = entry
            .file_name()
            .into_string()
            .unwrap_or_else(|os| os.to_string_lossy().into_owned());

        if file_name.ends_with(".dri") {
            let base = &file_name[..file_name.len() - 7];

            let (source_filename, copy_id) =
                if base.len() >= 7 && base.as_bytes()[base.len() - 7] == b'.' {
                    let id = &base[base.len() - 6..];
                    if id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                        (&base[..base.len() - 7], Some(id.to_string()))
                    } else {
                        (base, None)
                    }
                } else {
                    (base, None)
                };

            sidecars_by_filename
                .entry(source_filename.to_string())
                .or_default()
                .push(copy_id);
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
            let path_str = path_buf.to_string_lossy().into_owned();
            (path_str, file_name, path_buf, sidecars)
        })
        .collect();

    let result_list: Vec<ImageFile> = tasks
        .into_par_iter()
        .flat_map(|(path_str, file_name, path_buf, sidecars)| {
            let modified = fs::metadata(&path_buf)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

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

                let (is_edited, tags, rating, derived_from, derived_kind, flag) = {
                    let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

                    if enable_xmp_sync
                        && !is_virtual_copy
                        && crate::xmp::pull(&path_str, &mut metadata)
                        && let Ok(json) = serde_json::to_string_pretty(&metadata)
                    {
                        let _ = fs::write(&sidecar_path, json);
                    }

                    let is_raw = crate::formats::is_raw_file(&path_str);
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

                file_results.push(ImageFile {
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
                });
            }

            file_results
        })
        .collect();

    let mut result_list = result_list;
    link_versions(&mut result_list);
    Ok(result_list)
}

#[tauri::command]
pub fn list_images_recursive(
    path: String,
    app_handle: AppHandle,
) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);

    let root_path = Path::new(&path);
    let mut images = Vec::new();

    let mut sidecars_by_path: HashMap<PathBuf, Vec<Option<String>>> = HashMap::new();

    for entry in WalkDir::new(root_path).into_iter().filter_map(Result::ok) {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }

        let file_name = entry_path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(base) = file_name.strip_suffix(".dri") {
            let (source_filename, copy_id) =
                if base.len() >= 7 && base.as_bytes()[base.len() - 7] == b'.' {
                    let id = &base[base.len() - 6..];
                    if id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                        (&base[..base.len() - 7], Some(id.to_string()))
                    } else {
                        (base, None)
                    }
                } else {
                    (base, None)
                };

            if let Some(parent) = entry_path.parent() {
                sidecars_by_path
                    .entry(parent.join(source_filename))
                    .or_default()
                    .push(copy_id);
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
            let path_str = path_buf.to_string_lossy().into_owned();
            let file_name = path_buf
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (path_str, file_name, path_buf, sidecars)
        })
        .collect();

    let result_list: Vec<ImageFile> = tasks
        .into_par_iter()
        .flat_map(|(path_str, file_name, path_buf, sidecars)| {
            let modified = fs::metadata(&path_buf)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

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

                let (is_edited, tags, rating, derived_from, derived_kind, flag) = {
                    let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

                    if enable_xmp_sync
                        && !is_virtual_copy
                        && crate::xmp::pull(&path_str, &mut metadata)
                        && let Ok(json) = serde_json::to_string_pretty(&metadata)
                    {
                        let _ = fs::write(&sidecar_path, json);
                    }

                    let is_raw = crate::formats::is_raw_file(&path_str);
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

                file_results.push(ImageFile {
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
                });
            }

            file_results
        })
        .collect();

    let mut result_list = result_list;
    link_versions(&mut result_list);
    Ok(result_list)
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
                let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

                if enable_xmp_sync
                    && crate::xmp::pull(&virtual_path, &mut metadata)
                    && let Ok(json) = serde_json::to_string_pretty(&metadata)
                {
                    let _ = fs::write(&sidecar_path, json);
                }

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

fn scan_dir_lazy(
    path: &Path,
    expanded_folders: &HashSet<&str>,
    show_image_counts: bool,
    prefetch_one_level: bool,
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
                )?
            } else {
                let count = if show_image_counts {
                    WalkDir::new(&current_path)
                        .into_iter()
                        .filter_map(Result::ok)
                        .filter(|e| {
                            e.file_type().is_file()
                                && crate::formats::is_supported_image_file(e.path())
                        })
                        .count()
                } else {
                    0
                };
                (Vec::new(), count)
            };

            let has_any_subdirs = if should_scan {
                grand_children.iter().any(|c| c.is_dir)
            } else {
                has_subdirs(&current_path)
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

    let (children, own_count) = scan_dir_lazy(root_path, &expanded_set, show_image_counts, true)
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
        let (children, _) = scan_dir_lazy(root_path, &empty_set, show_image_counts, false)
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
        get_folder_tree_sync(path, expanded_folders, show_image_counts)
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
) -> Result<Vec<FolderNode>, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let results: Vec<Result<FolderNode, String>> = paths
            .par_iter()
            .map(|path| {
                get_folder_tree_sync(path.clone(), expanded_folders.clone(), show_image_counts)
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

pub fn generate_thumbnail_data(
    path_str: &str,
    gpu_context: Option<&GpuContext>,
    app_handle: &AppHandle,
) -> anyhow::Result<DynamicImage> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);
    // A video stands in the grid as a frame from itself. None of the
    // editing pipeline below applies to it.
    if is_video_file(&source_path) {
        return crate::video::poster_frame(&source_path);
    }
    let metadata: Option<ImageMetadata> = fs::read_to_string(sidecar_path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok());

    let adjustments = metadata
        .as_ref()
        .map_or(serde_json::Value::Null, |m| m.adjustments.clone());

    let state = app_handle.state::<AppState>();
    let context = gpu_context
        .ok_or_else(|| anyhow::anyhow!("Thumbnails need the GPU, which failed to initialize"))?;
    let dimension = load_settings(app_handle.clone())
        .unwrap_or_default()
        .thumbnail_resolution
        .unwrap_or(720);
    // Its own caches and a speed demosaic: a thumbnail is small and must
    // never evict the photo the editor is working on.
    crate::color_engine::application::render_thumbnail(
        context,
        &state,
        path_str,
        &adjustments,
        dimension,
    )
    .map(|f| DynamicImage::ImageRgba8(f.preview_rgba8()))
}

fn encode_thumbnail(image: &DynamicImage, target_width: u32) -> Result<Vec<u8>> {
    let thumbnail = crate::image_processing::downscale_f32_image(image, target_width, target_width);
    let mut buf = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 75);
    encoder.encode_image(&thumbnail.to_rgb8())?;
    Ok(buf.into_inner())
}

fn generate_single_thumbnail_and_cache(
    path_str: &str,
    thumb_cache_dir: &Path,
    gpu_context: Option<&GpuContext>,
    force_regenerate: bool,
    app_handle: &AppHandle,
    settings: &AppSettings,
) -> Option<(String, u8, bool)> {
    let (_, sidecar_path) = parse_virtual_path(path_str);

    let (rating, is_edited, adjustments_bytes) =
        if let Ok(content) = fs::read_to_string(&sidecar_path) {
            if let Ok(meta) = serde_json::from_str::<ImageMetadata>(&content) {
                let is_raw = crate::formats::is_raw_file(path_str);
                let tm = crate::image_processing::resolve_tonemapper_override(settings, is_raw);

                (
                    meta.rating,
                    crate::image_processing::is_image_edited(&meta.adjustments, is_raw, tm),
                    serde_json::to_vec(&meta.adjustments).unwrap_or_default(),
                )
            } else {
                (0, false, Vec::new())
            }
        } else {
            (0, false, Vec::new())
        };

    let cache_hash = compute_thumbnail_cache_hash(path_str, &adjustments_bytes)?;

    let cache_filename = format!("{}.jpg", cache_hash);
    let cache_path = thumb_cache_dir.join(cache_filename);

    if !force_regenerate
        && cache_path.exists()
        && let Ok(data) = fs::read(&cache_path)
    {
        let base64_str = general_purpose::STANDARD.encode(&data);
        return Some((
            format!("data:image/jpeg;base64,{}", base64_str),
            rating,
            is_edited,
        ));
    }

    let target_width = settings.thumbnail_resolution.unwrap_or(720);

    if let Ok(thumb_image) = generate_thumbnail_data(path_str, gpu_context, app_handle)
        && let Ok(thumb_data) = encode_thumbnail(&thumb_image, target_width)
    {
        let _ = fs::write(&cache_path, &thumb_data);
        let base64_str = general_purpose::STANDARD.encode(&thumb_data);
        return Some((
            format!("data:image/jpeg;base64,{}", base64_str),
            rating,
            is_edited,
        ));
    }
    None
}

pub fn start_thumbnail_workers(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<crate::AppState>();
    let manager = state.thumbnail_manager.clone();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let thread_count = settings.thumbnail_worker_threads.unwrap_or(4).clamp(1, 16);

    for _ in 0..thread_count {
        let app_clone = app_handle.clone();
        let manager_clone = manager.clone();
        let worker_settings = settings.clone();

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
                let gpu_context =
                    crate::gpu_processing::get_or_init_gpu_context(&state, &app_clone).ok();

                if let Ok(cache_dir) = get_thumb_cache_dir(&app_clone) {
                    let result = generate_single_thumbnail_and_cache(
                        &path_to_process,
                        &cache_dir,
                        gpu_context.as_ref(),
                        false,
                        &app_clone,
                        &worker_settings,
                    );

                    if let Some((thumbnail_data, rating, is_edited)) = result {
                        let _ = app_clone.emit(
                            "thumbnail-generated",
                            serde_json::json!({
                                "path": path_to_process,
                                "data": thumbnail_data,
                                "rating": rating,
                                "is_edited": is_edited
                            }),
                        );
                    }
                    increment_thumbnail_progress(&state, &app_clone);
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

    let mut queue = state.thumbnail_manager.queue.lock().unwrap();

    if paths.is_empty() {
        queue.clear();
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
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if seen.insert(path.clone()) {
            unique_paths.push(path);
        }
    }

    queue.retain(|p| !seen.contains(p));

    while queue.len() + unique_paths.len() > 500 {
        queue.pop_front();
    }

    for path in unique_paths {
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
    Ok(())
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
/// its virtual copies' sidecars, the legacy `.rrexif`, and its XMP when it
/// owns one (a JPEG never takes its RAW twin's `IMG.xmp`).
pub fn companion_files(image: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let (Some(parent), Some(name)) = (image.parent(), image.file_name()) else {
        return found;
    };
    let name = name.to_string_lossy().into_owned();
    let primary = format!("{name}.dri");
    let copies_prefix = format!("{name}.");
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.filter_map(Result::ok) {
            let entry_name = entry.file_name().to_string_lossy().into_owned();
            let is_sidecar = entry_name == primary
                || (entry_name.starts_with(&copies_prefix) && entry_name.ends_with(".dri"))
                || entry_name == format!("{name}.rrexif");
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
        let mut meta = crate::exif_processing::load_sidecar(&path);
        if meta.derived_from.as_deref() == Some(&*old_name) {
            meta.derived_from = Some(new_name.to_string());
            if let Ok(json) = serde_json::to_string_pretty(&meta) {
                let _ = fs::write(&path, json);
            }
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

    let mut all_files_to_trash = Vec::new();
    let mut renames = HashMap::new();
    let mut actions = Vec::new();

    for source_image_path in unique_source_images {
        let source_parent = source_image_path
            .parent()
            .ok_or("Could not get parent directory")?;
        if source_parent == dest_path {
            return Err("Cannot move files into the same folder they are already in.".to_string());
        }

        let files_to_move = find_all_associated_files(&source_image_path)?;
        let dest_image_path = dest_path.join(source_image_path.file_name().unwrap());
        let targets: Vec<PathBuf> = files_to_move
            .iter()
            .map(|f| companion_target(f, &source_image_path, &dest_image_path))
            .collect();

        if let Some(clash) = targets.iter().find(|t| t.exists()) {
            return Err(format!(
                "File already exists at destination: {}",
                clash.display()
            ));
        }

        for (from, to) in files_to_move.iter().zip(&targets) {
            if crate::journal::move_or_copy(from, to)? {
                all_files_to_trash.push(from.clone());
            }
            actions.push(crate::journal::Action::Moved {
                from: from.clone(),
                to: to.clone(),
            });
        }

        renames.insert(
            source_image_path.to_string_lossy().into_owned(),
            dest_image_path.to_string_lossy().into_owned(),
        );
    }

    // Originals copied across volumes go to the Trash once everything has
    // arrived. Undo brings the moved files back, so these are not journalled.
    if !all_files_to_trash.is_empty() {
        crate::journal::trash_all(&all_files_to_trash)?;
    }

    crate::journal::record(
        format!(
            "Move {} to {}",
            photos(renames.len()),
            folder_name(dest_path)
        ),
        actions,
    );

    sync_album_path_changes(&app_handle, Some(&renames), None, None);

    Ok(())
}

#[tauri::command]
pub fn save_metadata_and_update_thumbnail(
    path: String,
    adjustments: Value,
    app_handle: AppHandle,
    state: tauri::State<AppState>,
) -> Result<(), String> {
    let (_, sidecar_path) = parse_virtual_path(&path);

    let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

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
    metadata.adjustments = final_adjustments;

    // XMP first: writing it records its time in the sidecar saved below.
    if let Ok(settings) = load_settings(app_handle.clone())
        && settings.enable_xmp_sync.unwrap_or(true)
    {
        let create_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        crate::xmp::push(&path, &mut metadata, create_if_missing);
    }

    let json_string = serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?;
    std::fs::write(&sidecar_path, json_string).map_err(|e| e.to_string())?;

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

#[tauri::command]
pub async fn apply_adjustments_to_paths(
    paths: Vec<String>,
    adjustments: Value,
    app_handle: AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);

    tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

        let lens_db = app_handle
            .state::<AppState>()
            .lens_db
            .lock()
            .unwrap()
            .clone();

        paths.par_iter().for_each(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);

            let mut existing_metadata = crate::exif_processing::load_sidecar(&sidecar_path);

            let mut new_adjustments = existing_metadata.adjustments;
            if new_adjustments.is_null() {
                new_adjustments = serde_json::json!({});
            }

            if let (Some(new_map), Some(pasted_map)) =
                (new_adjustments.as_object_mut(), adjustments.as_object())
            {
                for (k, v) in pasted_map {
                    new_map.insert(k.clone(), v.clone());
                }
            }

            resolve_lens_params_in_adjustments(
                &mut new_adjustments,
                &existing_metadata.exif,
                lens_db.as_deref(),
            );

            existing_metadata.adjustments = new_adjustments;

            if enable_xmp_sync {
                crate::xmp::push(path, &mut existing_metadata, create_xmp_if_missing);
            }

            if let Ok(json_string) = serde_json::to_string_pretty(&existing_metadata) {
                let _ = std::fs::write(&sidecar_path, json_string);
            }
        });

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

        paths.par_iter().for_each(|path_str| {
            let result = generate_single_thumbnail_and_cache(
                path_str,
                &thumb_cache_dir,
                gpu_context.as_ref(),
                                true,
                &app_handle,
                &settings,
            );

            if let Some((thumbnail_data, rating, is_edited)) = result {
                let _ = app_handle.emit(
                    "thumbnail-generated",
                    serde_json::json!({ "path": path_str, "data": thumbnail_data, "rating": rating, "is_edited": is_edited  }),
                );
            }

            increment_thumbnail_progress(&state, &app_handle);
        });
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

            let mut existing_metadata = crate::exif_processing::load_sidecar(&sidecar_path);

            existing_metadata.adjustments = serde_json::json!({});

            if enable_xmp_sync {
                crate::xmp::push(path, &mut existing_metadata, create_xmp_if_missing);
            }

            if let Ok(json_string) = serde_json::to_string_pretty(&existing_metadata) {
                let _ = std::fs::write(&sidecar_path, json_string);
            }
        });

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

        paths.par_iter().for_each(|path_str| {
            let result = generate_single_thumbnail_and_cache(
                path_str,
                &thumb_cache_dir,
                gpu_context.as_ref(),
                                true,
                &app_handle,
                &settings,
            );

            if let Some((thumbnail_data, rating, is_edited)) = result {
                let _ = app_handle.emit(
                    "thumbnail-generated",
                    serde_json::json!({ "path": path_str, "data": thumbnail_data, "rating": rating, "is_edited": is_edited }),
                );
            }

            increment_thumbnail_progress(&state, &app_handle);
        });
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

                let mut existing_metadata = crate::exif_processing::load_sidecar(&sidecar_path);

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
                    crate::xmp::push(path, &mut existing_metadata, create_xmp_if_missing);
                }

                if let Ok(json_string) = serde_json::to_string_pretty(&existing_metadata) {
                    let _ = std::fs::write(&sidecar_path, json_string);
                }
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

            if let Some((thumbnail_data, rating, is_edited)) = result {
                let _ = app_handle.emit(
                    "thumbnail-generated",
                    serde_json::json!({ "path": path, "data": thumbnail_data, "rating": rating, "is_edited": is_edited  }),
                );
            }

            increment_thumbnail_progress(&state, &app_handle);
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

    paths.par_iter().for_each(|path| {
        let (_, sidecar_path) = parse_virtual_path(path);

        let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

        let mut tags = metadata.tags.unwrap_or_default();
        tags.retain(|tag| !tag.starts_with(COLOR_TAG_PREFIX));

        if let Some(c) = &color
            && !c.is_empty()
        {
            tags.push(format!("{}{}", COLOR_TAG_PREFIX, c));
        }

        if tags.is_empty() {
            metadata.tags = None;
        } else {
            metadata.tags = Some(tags);
        }

        if !path.contains("?vc=") {
            crate::finder_tags::set_color_label(
                Path::new(path),
                color.as_deref().filter(|c| !c.is_empty()),
            );
        }

        if enable_xmp_sync {
            crate::xmp::push(path, &mut metadata, create_xmp_if_missing);
        }

        if let Ok(json_string) = serde_json::to_string_pretty(&metadata) {
            let _ = std::fs::write(&sidecar_path, json_string);
        }
    });

    Ok(())
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

    paths.par_iter().for_each(|path| {
        let (_, sidecar_path) = parse_virtual_path(path);

        let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

        metadata.rating = rating;

        if enable_xmp_sync {
            crate::xmp::push(path, &mut metadata, create_xmp_if_missing);
        }

        if let Ok(json_string) = serde_json::to_string_pretty(&metadata) {
            let _ = std::fs::write(&sidecar_path, json_string);
        }
    });

    Ok(())
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

    paths.par_iter().for_each(|path| {
        let (_, sidecar_path) = parse_virtual_path(path);
        let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);
        metadata.flag = flag.clone();

        if enable_xmp_sync {
            crate::xmp::push(path, &mut metadata, create_xmp_if_missing);
        }

        if let Ok(json_string) = serde_json::to_string_pretty(&metadata) {
            let _ = std::fs::write(&sidecar_path, json_string);
        }
    });

    Ok(())
}

#[tauri::command]
pub fn load_metadata(path: String, app_handle: AppHandle) -> Result<ImageMetadata, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(true);

    let (_, sidecar_path) = parse_virtual_path(&path);
    let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

    if enable_xmp_sync
        && crate::xmp::pull(&path, &mut metadata)
        && let Ok(json) = serde_json::to_string_pretty(&metadata)
    {
        let _ = fs::write(&sidecar_path, json);
    }

    Ok(metadata)
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
    let json_string = serde_json::to_string_pretty(&presets).map_err(|e| e.to_string())?;
    fs::write(path, json_string).map_err(|e| e.to_string())
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
        } else {
            if source_path.exists() {
                match find_all_associated_files(&source_path) {
                    Ok(associated_files) => {
                        for file in associated_files {
                            files_to_trash.insert(file);
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "Could not find associated files for {}: {}",
                            source_path.display(),
                            e
                        );
                    }
                }
            }
        }
    }

    if files_to_trash.is_empty() {
        return Ok(());
    }

    let final_paths_to_delete: Vec<PathBuf> = files_to_trash.into_iter().collect();
    let actions = crate::journal::trash_all(&final_paths_to_delete)?;
    crate::journal::record(
        format!("Delete {} and associated files", photos(deletions.len())),
        actions,
    );

    sync_album_path_changes(&app_handle, None, Some(&deletions), None);

    Ok(())
}

#[tauri::command]
pub fn delete_files_with_associated(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    let mut stems_to_delete = HashSet::new();
    let mut parent_dirs = HashSet::new();
    let mut deletions = HashSet::new();

    for path_str in &paths {
        deletions.insert(path_str.clone());
        let (source_path, _) = parse_virtual_path(path_str);
        if let Some(file_name) = source_path.file_name().and_then(|s| s.to_str())
            && let Some(stem) = file_name.split('.').next()
        {
            stems_to_delete.insert(stem.to_string());
        }
        if let Some(parent) = source_path.parent() {
            parent_dirs.insert(parent.to_path_buf());
        }
    }

    if stems_to_delete.is_empty() {
        return Ok(());
    }

    let mut files_to_trash = HashSet::new();

    for parent_dir in parent_dirs {
        if let Ok(entries) = fs::read_dir(parent_dir) {
            for entry in entries.filter_map(Result::ok) {
                let entry_path = entry.path();
                if !entry_path.is_file() {
                    continue;
                }

                let entry_filename = entry.file_name();
                let entry_filename_str = entry_filename.to_string_lossy();

                if let Some(base_stem) = entry_filename_str.split('.').next()
                    && stems_to_delete.contains(base_stem)
                    && (is_supported_image_file(entry_filename_str.as_ref())
                        || entry_filename_str.ends_with(".dri")
                        || entry_filename_str.ends_with(".rrexif"))
                {
                    files_to_trash.insert(entry_path);
                }
            }
        }
    }

    if files_to_trash.is_empty() {
        return Ok(());
    }

    let final_paths_to_delete: Vec<PathBuf> = files_to_trash.into_iter().collect();
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    if let Err(trash_error) = trash::delete_all(&final_paths_to_delete) {
        log::warn!(
            "Failed to move files to trash: {}. Falling back to permanent delete.",
            trash_error
        );
        for path in final_paths_to_delete {
            if path.is_file() {
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete file {}: {}", path.display(), e))?;
            }
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    for path in final_paths_to_delete {
        if path.is_file() {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to delete file {}: {}", path.display(), e))?;
        }
    }

    sync_album_path_changes(&app_handle, None, Some(&deletions), None);

    Ok(())
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

pub fn get_cache_key_hash(path_str: &str) -> Option<String> {
    let (_, sidecar_path) = parse_virtual_path(path_str);

    let adjustments_bytes = if let Ok(content) = fs::read_to_string(&sidecar_path) {
        if let Ok(meta) = serde_json::from_str::<ImageMetadata>(&content) {
            serde_json::to_vec(&meta.adjustments).unwrap_or_default()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    compute_thumbnail_cache_hash(path_str, &adjustments_bytes)
}

pub fn get_cached_or_generate_thumbnail_image(
    path_str: &str,
    app_handle: &AppHandle,
    gpu_context: Option<&GpuContext>,
) -> Result<DynamicImage> {
    let thumb_cache_dir = get_thumb_cache_dir(app_handle).map_err(|e| anyhow::anyhow!(e))?;
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let target_width = settings.thumbnail_resolution.unwrap_or(720);

    if let Some(cache_hash) = get_cache_key_hash(path_str) {
        let cache_filename = format!("{}.jpg", cache_hash);
        let cache_path = thumb_cache_dir.join(cache_filename);

        if cache_path.exists() {
            if let Ok(image) = image::open(&cache_path) {
                return Ok(image);
            }
            eprintln!(
                "Could not open cached thumbnail, regenerating: {:?}",
                cache_path
            );
        }

        let thumb_image = generate_thumbnail_data(path_str, gpu_context, app_handle)?;
        let thumb_data = encode_thumbnail(&thumb_image, target_width)?;
        fs::write(&cache_path, &thumb_data)?;

        Ok(thumb_image)
    } else {
        generate_thumbnail_data(path_str, gpu_context, app_handle)
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
                    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
                    {
                        if let Err(trash_error) = trash::delete(&source_path) {
                            log::warn!(
                                "Failed to trash source file {}: {}. Deleting permanently.",
                                source_path.display(),
                                trash_error
                            );
                            fs::remove_file(&source_path).map_err(|e| e.to_string())?;
                        }
                        for companion in companions.iter().filter(|c| c.exists()) {
                            if let Err(trash_error) = trash::delete(companion) {
                                log::warn!(
                                    "Failed to trash source sidecar {}: {}. Deleting permanently.",
                                    companion.display(),
                                    trash_error
                                );
                                fs::remove_file(companion).map_err(|e| e.to_string())?;
                            }
                        }
                    }

                    #[cfg(not(any(
                        target_os = "windows",
                        target_os = "macos",
                        target_os = "linux"
                    )))]
                    {
                        fs::remove_file(&source_path).map_err(|e| e.to_string())?;
                        for companion in companions.iter().filter(|c| c.exists()) {
                            fs::remove_file(companion).map_err(|e| e.to_string())?;
                        }
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

    let mut operations: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut final_new_paths = Vec::with_capacity(paths.len());
    let mut renames = HashMap::new();

    for (i, path_str) in paths.iter().enumerate() {
        let (original_path, _) = parse_virtual_path(path_str);
        if !original_path.exists() {
            return Err(format!("File not found: {}", path_str));
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

        if new_path.exists() && new_path != original_path {
            return Err(format!(
                "A file with the name {} already exists.",
                new_path.display()
            ));
        }

        operations.insert(original_path, new_path);
    }

    // Everything that travels with each photo follows its new name.
    let image_renames: Vec<(PathBuf, PathBuf)> = operations
        .iter()
        .map(|(o, n)| (o.clone(), n.clone()))
        .collect();
    let mut sidecar_operations: HashMap<PathBuf, PathBuf> = HashMap::new();
    for (original_path, new_path) in &operations {
        for companion in companion_files(original_path) {
            let target = companion_target(&companion, original_path, new_path);
            if target != companion {
                sidecar_operations.insert(companion, target);
            }
        }
    }
    operations.extend(sidecar_operations);

    let mut actions = Vec::new();
    for (old_path, new_path) in operations {
        fs::rename(&old_path, &new_path).map_err(|e| {
            format!(
                "Failed to rename {} to {}: {}",
                old_path.display(),
                new_path.display(),
                e
            )
        })?;

        let old_str = old_path.to_string_lossy().into_owned();
        let new_str = new_path.to_string_lossy().into_owned();
        actions.push(crate::journal::Action::Moved {
            from: old_path.clone(),
            to: new_path.clone(),
        });

        renames.insert(old_str, new_str.clone());

        if is_supported_image_file(&new_path) {
            final_new_paths.push(new_str);
        }
    }

    for (old, new) in &image_renames {
        relink_versions(old, new);
    }
    crate::journal::record(format!("Rename {}", photos(image_renames.len())), actions);

    sync_album_path_changes(&app_handle, Some(&renames), None, None);

    Ok(final_new_paths)
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
        let default_metadata = ImageMetadata::default();
        let json_string =
            serde_json::to_string_pretty(&default_metadata).map_err(|e| e.to_string())?;
        fs::write(&new_sidecar_path, json_string).map_err(|e| e.to_string())?;
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
