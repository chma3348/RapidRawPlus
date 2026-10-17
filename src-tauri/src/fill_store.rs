//! Saved AI fill results.
//!
//! Each fill or replace keeps what it made (its raw output and where it
//! goes) so it can be re-blended later without running the model again. The
//! finished fill lives in the photo's own edit file, so nothing here is
//! needed to show a photo; losing a run only means its blend can no longer
//! be fine-tuned.
//!
//! Runs live in the app's cache folder (macOS may reclaim it), one folder per
//! run, with an index of which photo each belongs to and when it was last
//! used. Opening a photo, or re-blending one of its fills, marks only that
//! photo's runs as used. A run unused for the configured number of days is
//! removed, and if all runs together pass the size limit, the least recently
//! used go first. The many diagnostic images a fill can produce are only
//! kept when diagnostics are switched on in Settings.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::Manager;

/// The files re-blending reads (by name, without extension). Everything else
/// a fill saves is diagnostic.
const NEEDED: &[&str] = &[
    "replace-raw-output",
    "replace-plan",
    "replace-cache-geometry",
    "replace-original-payload",
    "replace-protection",
    "replace-source-crop",
    "replace-selection-crop",
];

pub const DEFAULT_LIMIT_GB: f64 = 2.0;
pub const DEFAULT_DAYS: u32 = 30;

const DAY: u64 = 24 * 60 * 60;

/// One read-modify-write of the index at a time.
static INDEX_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Index {
    runs: BTreeMap<String, Run>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Run {
    /// The photo the fill was made on (its source path).
    image: String,
    /// Seconds since the epoch.
    last_used: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub bytes: u64,
    pub runs: usize,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn root(app: &tauri::AppHandle) -> Option<PathBuf> {
    Some(app.path().app_cache_dir().ok()?.join("fill-results"))
}

/// Where runs were kept before (in the app's data folder, every file kept).
fn legacy_root(app: &tauri::AppHandle) -> Option<PathBuf> {
    Some(app.path().app_data_dir().ok()?.join("ai-fill-debug"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 160
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// The folder for a new run's files, created.
pub fn run_dir(app: &tauri::AppHandle, run_id: &str) -> Option<PathBuf> {
    if !valid_id(run_id) {
        return None;
    }
    let dir = root(app)?.join(run_id);
    fs::create_dir_all(&dir)
        .map_err(|e| log::warn!("[fill] could not create {dir:?}: {e}"))
        .ok()?;
    Some(dir)
}

/// The folder holding an existing run, wherever it was saved.
pub fn find_run(app: &tauri::AppHandle, run_id: &str) -> Option<PathBuf> {
    if !valid_id(run_id) {
        return None;
    }
    [root(app), legacy_root(app)]
        .into_iter()
        .flatten()
        .map(|r| r.join(run_id))
        .find(|d| d.is_dir())
}

/// Whether a file a fill saves (by name, without extension) is kept:
/// always if re-blending needs it, otherwise only with diagnostics on.
pub fn keeps(app: &tauri::AppHandle, name: &str) -> bool {
    NEEDED.contains(&name) || diagnostics(app)
}

/// The saved settings, read as they are (no migrations, no writing back).
fn settings(app: &tauri::AppHandle) -> Option<crate::app_settings::AppSettings> {
    let path = crate::app_settings::get_settings_path(app).ok()?;
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn diagnostics(app: &tauri::AppHandle) -> bool {
    settings(app)
        .and_then(|s| s.keep_fill_diagnostics)
        .unwrap_or(false)
}

fn read_index(root: &Path) -> Index {
    fs::read(root.join("index.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_index(root: &Path, index: &Index) {
    let _ = fs::create_dir_all(root);
    let tmp = root.join("index.json.part");
    if let Ok(bytes) = serde_json::to_vec(index)
        && fs::write(&tmp, bytes).is_ok()
    {
        let _ = fs::rename(&tmp, root.join("index.json"));
    }
}

fn with_index(app: &tauri::AppHandle, change: impl FnOnce(&mut Index)) {
    let Some(root) = root(app) else { return };
    let _guard = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut index = read_index(&root);
    change(&mut index);
    write_index(&root, &index);
}

/// A new run, made on `image`.
pub fn register(app: &tauri::AppHandle, run_id: &str, image: &str) {
    if !valid_id(run_id) {
        return;
    }
    with_index(app, |index| {
        index.runs.insert(
            run_id.to_string(),
            Run {
                image: image.to_string(),
                last_used: now(),
            },
        );
    });
}

/// A run was used (re-blended).
pub fn touch_run(app: &tauri::AppHandle, run_id: &str) {
    with_index(app, |index| {
        if let Some(run) = index.runs.get_mut(run_id) {
            run.last_used = now();
        }
    });
}

/// A photo was opened: its runs, and only its, count as used now.
pub fn touch_image(app: &tauri::AppHandle, image: &str) {
    with_index(app, |index| {
        let t = now();
        for run in index.runs.values_mut().filter(|r| r.image == image) {
            run.last_used = t;
        }
    });
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

fn modified(dir: &Path) -> u64 {
    fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Every run on disk: (id, folder, last used, size).
fn runs_on_disk(app: &tauri::AppHandle, index: &Index) -> Vec<(String, PathBuf, u64, u64)> {
    let mut out = Vec::new();
    for root in [root(app), legacy_root(app)].into_iter().flatten() {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_dir() {
                continue;
            }
            let id = e.file_name().to_string_lossy().into_owned();
            // Runs from before the index count as used when last written.
            let last_used = index
                .runs
                .get(&id)
                .map_or_else(|| modified(&path), |r| r.last_used);
            out.push((id, path.clone(), last_used, dir_size(&path)));
        }
    }
    out
}

pub fn usage(app: &tauri::AppHandle) -> Usage {
    let index = root(app).map(|r| read_index(&r)).unwrap_or_default();
    let runs = runs_on_disk(app, &index);
    Usage {
        bytes: runs.iter().map(|r| r.3).sum(),
        runs: runs.len(),
    }
}

/// Remove runs unused for longer than the configured days, then the least
/// recently used until all fit in the size limit.
pub fn tidy(app: &tauri::AppHandle) {
    let s = settings(app);
    let days = s
        .as_ref()
        .and_then(|s| s.fill_cache_days)
        .unwrap_or(DEFAULT_DAYS);
    let limit = (s
        .as_ref()
        .and_then(|s| s.fill_cache_limit_gb)
        .unwrap_or(DEFAULT_LIMIT_GB)
        .max(0.0)
        * 1e9) as u64;
    let Some(root) = root(app) else { return };
    let _guard = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut index = read_index(&root);
    let mut runs = runs_on_disk(app, &index);
    runs.sort_by_key(|r| r.2);
    let cutoff = now().saturating_sub(u64::from(days) * DAY);
    let mut total: u64 = runs.iter().map(|r| r.3).sum();
    let (mut removed, mut freed) = (0, 0u64);
    for (id, path, last_used, size) in &runs {
        if *last_used >= cutoff && total <= limit {
            continue;
        }
        if fs::remove_dir_all(path).is_ok() {
            index.runs.remove(id);
            total = total.saturating_sub(*size);
            removed += 1;
            freed += size;
        }
    }
    // Entries whose folder is gone (cleared by macOS, say).
    let present: std::collections::HashSet<&str> = runs.iter().map(|r| r.0.as_str()).collect();
    index.runs.retain(|id, _| present.contains(id.as_str()));
    write_index(&root, &index);
    if removed > 0 {
        log::info!(
            "[fill] tidied {removed} saved fill result(s), {:.1} MB",
            freed as f64 / 1e6
        );
    }
}

/// Remove every saved run.
pub fn clear(app: &tauri::AppHandle) {
    let _guard = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for root in [root(app), legacy_root(app)].into_iter().flatten() {
        let _ = fs::remove_dir_all(&root);
    }
}

#[tauri::command]
pub fn get_fill_results_usage(app_handle: tauri::AppHandle) -> Usage {
    usage(&app_handle)
}

#[tauri::command]
pub async fn clear_fill_results(app_handle: tauri::AppHandle) -> Result<Usage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        clear(&app_handle);
        usage(&app_handle)
    })
    .await
    .map_err(|e| e.to_string())
}

/// Apply new settings now (a lower limit or fewer days frees space at once).
#[tauri::command]
pub async fn tidy_fill_results(app_handle: tauri::AppHandle) -> Result<Usage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        tidy(&app_handle);
        usage(&app_handle)
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re_blending_files_are_always_kept() {
        for name in NEEDED {
            assert!(valid_id(name));
        }
        assert!(NEEDED.contains(&"replace-raw-output"));
        assert!(!NEEDED.contains(&"run-final-full"));
        assert!(!NEEDED.contains(&"blob-01-source-crop"));
    }

    #[test]
    fn run_ids_cannot_leave_the_folder() {
        assert!(valid_id(
            "6cc09406-bf0f-4ea2-915c-ce501d228be6-1788740553598"
        ));
        assert!(!valid_id("../settings"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id(""));
    }
}
