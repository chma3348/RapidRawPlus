//! The generative engine's large models (generative fill, AI upscaling):
//! listed, downloaded, imported and deleted from the app.
//!
//! Each model is downloaded from its publisher (Hugging Face), never from a
//! copy of ours, under the publisher's own licence, which the app shows. A
//! model whose licence restricts use (FLUX.1 [dev]: non-commercial) is only
//! downloaded once the user has accepted it. Downloads resume after an
//! interruption, check there is room first, and are checked against the
//! publisher's SHA-256 before they are put in place.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Emitter;

/// The catalog, bundled with the app.
const CATALOG: &str = include_str!("../engine_models.json");

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    name: String,
    /// What it is for, shown under its name.
    purpose: String,
    /// The features that need it.
    needed_for: Vec<String>,
    /// Where it lives, relative to the engine's models folder.
    file: String,
    /// The publisher's Hugging Face repository and the file's path in it.
    repo: String,
    path: String,
    sha256: String,
    size: u64,
    licence: String,
    licence_url: String,
    /// The user must accept the licence before it is downloaded.
    #[serde(default)]
    must_accept: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineModel {
    id: String,
    name: String,
    purpose: String,
    needed_for: Vec<String>,
    size_bytes: u64,
    installed: bool,
    downloading: bool,
    /// Bytes of an unfinished download kept to resume from.
    partial_bytes: u64,
    licence: String,
    licence_url: String,
    must_accept: bool,
    source_url: String,
}

/// A large file in the models folder that is not in the catalog (a LoRA
/// added by hand, say): it can be deleted, not downloaded.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherModelFile {
    path: String,
    size_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineModels {
    models: Vec<EngineModel>,
    others: Vec<OtherModelFile>,
    /// Everything in the models folder, in bytes.
    total_bytes: u64,
    /// Free space on the disk holding it.
    free_bytes: Option<u64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    id: String,
    downloaded: u64,
    total: u64,
    /// "downloading", "verifying", "done", "failed" or "cancelled".
    stage: String,
    message: Option<String>,
}

fn catalog() -> Vec<Entry> {
    serde_json::from_str(CATALOG).expect("bundled engine_models.json must parse")
}

/// Downloads in flight, each with its cancel flag.
static ACTIVE: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn models_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(crate::comfy_engine::engine_dir(app)
        .map_err(|e| e.to_string())?
        .join("ComfyUI/models"))
}

fn source_url(e: &Entry) -> String {
    format!(
        "https://huggingface.co/{}/resolve/main/{}?download=true",
        e.repo, e.path
    )
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    target.with_file_name(name)
}

/// Files the catalog places, or that are part of the engine's layout.
fn known(dir: &Path, entries: &[Entry], path: &Path) -> bool {
    entries.iter().any(|e| dir.join(&e.file) == path)
        || path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("put_"))
}

fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        // Follow the models folder's own links (it is one into Documents).
        match fs::metadata(&p) {
            Ok(m) if m.is_dir() => walk(&p, out),
            Ok(m) => out.push((p, m.len())),
            Err(_) => {}
        }
    }
}

#[cfg(unix)]
fn free_space(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0)
        .then(|| s.f_bavail as u64 * s.f_frsize as u64)
}

#[cfg(not(unix))]
fn free_space(_path: &Path) -> Option<u64> {
    None
}

fn listing(app: &tauri::AppHandle) -> Result<EngineModels, String> {
    let dir = models_dir(app)?;
    let entries = catalog();
    let active = ACTIVE.lock().unwrap();
    let models = entries
        .iter()
        .map(|e| {
            let target = dir.join(&e.file);
            EngineModel {
                id: e.id.clone(),
                name: e.name.clone(),
                purpose: e.purpose.clone(),
                needed_for: e.needed_for.clone(),
                size_bytes: e.size,
                installed: target.is_file(),
                downloading: active.contains_key(&e.id),
                partial_bytes: fs::metadata(part_path(&target)).map_or(0, |m| m.len()),
                licence: e.licence.clone(),
                licence_url: e.licence_url.clone(),
                must_accept: e.must_accept,
                source_url: format!("https://huggingface.co/{}", e.repo),
            }
        })
        .collect();
    let mut files = Vec::new();
    walk(&dir, &mut files);
    let total_bytes = files.iter().map(|f| f.1).sum();
    let others = files
        .into_iter()
        .filter(|(p, size)| {
            *size > 10_000_000
                && !known(&dir, &entries, p)
                && !p.to_string_lossy().ends_with(".part")
        })
        .map(|(p, size)| OtherModelFile {
            path: p
                .strip_prefix(&dir)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned(),
            size_bytes: size,
        })
        .collect();
    Ok(EngineModels {
        models,
        others,
        total_bytes,
        free_bytes: free_space(&dir),
    })
}

#[tauri::command]
pub fn list_engine_models(app_handle: tauri::AppHandle) -> Result<EngineModels, String> {
    listing(&app_handle)
}

fn emit(app: &tauri::AppHandle, p: Progress) {
    let _ = app.emit("engine-model-progress", p);
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 8 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>())
}

/// Download one model. Resumes a `.part` file left by an interrupted try.
#[tauri::command]
pub async fn download_engine_model(
    id: String,
    accepted_licence: bool,
    app_handle: tauri::AppHandle,
) -> Result<EngineModels, String> {
    let entry = catalog()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or("Unknown model.")?;
    if entry.must_accept && !accepted_licence {
        return Err(format!(
            "{} is only downloaded once its licence ({}) has been accepted.",
            entry.name, entry.licence
        ));
    }
    let dir = models_dir(&app_handle)?;
    let target = dir.join(&entry.file);
    if target.is_file() {
        return listing(&app_handle);
    }
    let part = part_path(&target);
    let have = fs::metadata(&part).map_or(0, |m| m.len());
    if let Some(free) = free_space(&dir)
        && free < entry.size.saturating_sub(have) + 1_000_000_000
    {
        return Err(format!(
            "Not enough free space for {}: it needs {:.1} GB, {:.1} GB is free.",
            entry.name,
            entry.size.saturating_sub(have) as f64 / 1e9,
            free as f64 / 1e9
        ));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut active = ACTIVE.lock().unwrap();
        if active.contains_key(&id) {
            return Err(format!("{} is already downloading.", entry.name));
        }
        active.insert(id.clone(), cancel.clone());
    }
    log::info!(
        "[models] downloading {} from {}",
        entry.name,
        source_url(&entry)
    );
    let result = fetch(&app_handle, &entry, &target, &part, have, &cancel).await;
    ACTIVE.lock().unwrap().remove(&id);
    let stage = |s: &str, message: Option<String>| Progress {
        id: id.clone(),
        downloaded: 0,
        total: entry.size,
        stage: s.into(),
        message,
    };
    match result {
        Ok(()) => {
            log::info!("[models] {} downloaded and verified", entry.name);
            emit(&app_handle, stage("done", None));
            listing(&app_handle)
        }
        Err(e) if cancel.load(Ordering::Relaxed) => {
            emit(&app_handle, stage("cancelled", None));
            log::info!("[models] {} download cancelled: {e}", entry.name);
            listing(&app_handle)
        }
        Err(e) => {
            log::warn!("[models] {} download failed: {e}", entry.name);
            emit(&app_handle, stage("failed", Some(e.clone())));
            Err(e)
        }
    }
}

async fn fetch(
    app: &tauri::AppHandle,
    entry: &Entry,
    target: &Path,
    part: &Path,
    mut have: u64,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if have >= entry.size {
        // A finished download that was never put in place: verify it.
        have = 0;
        let _ = fs::remove_file(part);
    }
    let client = reqwest::Client::builder()
        .user_agent("DarkroomIndex")
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(source_url(entry));
    if have > 0 {
        request = request.header("Range", format!("bytes={have}-"));
    }
    let mut response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    if !(status.is_success()) {
        return Err(format!("The download was refused ({status})."));
    }
    // Without a partial answer the server is sending it all again.
    if have > 0 && status != reqwest::StatusCode::PARTIAL_CONTENT {
        have = 0;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(part)
        .map_err(|e| e.to_string())?;
    let mut downloaded = have;
    let mut last_report = std::time::Instant::now();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if last_report.elapsed().as_millis() > 250 {
            last_report = std::time::Instant::now();
            emit(
                app,
                Progress {
                    id: entry.id.clone(),
                    downloaded,
                    total: entry.size,
                    stage: "downloading".into(),
                    message: None,
                },
            );
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    emit(
        app,
        Progress {
            id: entry.id.clone(),
            downloaded,
            total: entry.size,
            stage: "verifying".into(),
            message: None,
        },
    );
    let part_owned = part.to_path_buf();
    let sha = tauri::async_runtime::spawn_blocking(move || sha256_of(&part_owned))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if sha != entry.sha256 {
        let _ = fs::remove_file(part);
        return Err(format!(
            "{} did not arrive intact (its checksum does not match the publisher's), so it was discarded. Try again.",
            entry.name
        ));
    }
    fs::rename(part, target).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn cancel_engine_model_download(id: String) {
    if let Some(flag) = ACTIVE.lock().unwrap().get(&id) {
        flag.store(true, Ordering::Relaxed);
    }
}

/// Delete a catalog model (and any unfinished download of it).
#[tauri::command]
pub fn delete_engine_model(
    id: String,
    app_handle: tauri::AppHandle,
) -> Result<EngineModels, String> {
    let entry = catalog()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or("Unknown model.")?;
    if ACTIVE.lock().unwrap().contains_key(&id) {
        return Err(format!("{} is downloading; cancel it first.", entry.name));
    }
    let target = models_dir(&app_handle)?.join(&entry.file);
    for p in [target.clone(), part_path(&target)] {
        if p.is_file() {
            fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
    }
    log::info!("[models] deleted {}", entry.name);
    listing(&app_handle)
}

/// Delete a file in the models folder that is not in the catalog.
#[tauri::command]
pub fn delete_other_engine_file(
    path: String,
    app_handle: tauri::AppHandle,
) -> Result<EngineModels, String> {
    let dir = models_dir(&app_handle)?;
    let target = dir.join(&path);
    // Only files inside the models folder, named as the listing named them.
    let inside = target
        .canonicalize()
        .ok()
        .zip(dir.canonicalize().ok())
        .is_some_and(|(t, d)| t.starts_with(&d));
    if path.contains("..") || !inside || !target.is_file() {
        return Err("Not a file in the models folder.".into());
    }
    fs::remove_file(&target).map_err(|e| e.to_string())?;
    log::info!("[models] deleted {path}");
    listing(&app_handle)
}

/// Copy catalog models from another folder (another Mac's models folder, a
/// drive), skipping the download: each file found under its catalog name is
/// checked against the publisher's checksum, then copied in place.
#[tauri::command]
pub async fn import_engine_models(
    folder: String,
    app_handle: tauri::AppHandle,
) -> Result<(EngineModels, Vec<String>), String> {
    let dir = models_dir(&app_handle)?;
    let source = PathBuf::from(folder);
    let app = app_handle.clone();
    let imported = tauri::async_runtime::spawn_blocking(move || {
        let mut files = Vec::new();
        walk(&source, &mut files);
        let mut imported = Vec::new();
        for entry in catalog() {
            let target = dir.join(&entry.file);
            if target.is_file() {
                continue;
            }
            let name = Path::new(&entry.file).file_name().unwrap_or_default();
            let Some((found, _)) = files
                .iter()
                .find(|(p, size)| p.file_name() == Some(name) && *size == entry.size)
            else {
                continue;
            };
            emit(
                &app,
                Progress {
                    id: entry.id.clone(),
                    downloaded: 0,
                    total: entry.size,
                    stage: "verifying".into(),
                    message: None,
                },
            );
            if sha256_of(found).ok().as_deref() != Some(entry.sha256.as_str()) {
                log::warn!(
                    "[models] {found:?} is not the published {}; skipped",
                    entry.name
                );
                continue;
            }
            if let Some(parent) = target.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let part = part_path(&target);
            if fs::copy(found, &part).is_ok() && fs::rename(&part, &target).is_ok() {
                log::info!("[models] imported {} from {found:?}", entry.name);
                imported.push(entry.name.clone());
            }
            emit(
                &app,
                Progress {
                    id: entry.id.clone(),
                    downloaded: entry.size,
                    total: entry.size,
                    stage: "done".into(),
                    message: None,
                },
            );
        }
        imported
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok((listing(&app_handle)?, imported))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_complete() {
        let entries = catalog();
        assert!(!entries.is_empty());
        for e in &entries {
            assert_eq!(e.sha256.len(), 64, "{}", e.id);
            assert!(
                e.size > 0 && !e.repo.is_empty() && !e.licence_url.is_empty(),
                "{}",
                e.id
            );
            assert!(!e.file.contains(".."), "{}", e.id);
        }
        // FLUX.1 [dev] is non-commercial: never downloaded unaccepted.
        assert!(
            entries
                .iter()
                .filter(|e| e.repo.contains("FLUX.1-Fill-dev"))
                .all(|e| e.must_accept)
        );
    }
}
