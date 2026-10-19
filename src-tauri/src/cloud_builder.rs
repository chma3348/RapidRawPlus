//! Builds what cloud-only photos need on this Mac, in the background, a few
//! photos at a time: in the "previews" mode a smart preview (synced beside
//! the photo), in the "thumbnails" mode only a cached thumbnail. Each photo
//! is downloaded, used, then given back to the cloud service, so this never
//! holds more than a few photos' worth of space. Photos on screen go first,
//! then the folder last read; it can be paused.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, LazyLock, Mutex};

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::smart_preview::{self, Mode};

struct Queue {
    items: VecDeque<PathBuf>,
    /// Being built now, by one of the workers.
    building: HashSet<PathBuf>,
    done: usize,
    started: bool,
    /// Failed tries per photo: a download can time out (the service busy,
    /// or offline); it is tried again later, a few times.
    failures: std::collections::HashMap<PathBuf, u8>,
    /// When the latest photos finished, for the pace shown as time left.
    finished: VecDeque<std::time::Instant>,
}

/// Photos the pace is measured over: recent enough to follow the service's
/// speed, enough to smooth the batches the workers finish in.
const PACE_WINDOW: usize = 13;

const TRIES: u8 = 3;

/// Photos built at once. Most of the time goes to downloading, and a cloud
/// service often serves several files faster than one after another; more
/// would hold more photos (and their decoding) in memory at once.
const WORKERS: usize = 3;

static QUEUE: LazyLock<(Mutex<Queue>, Condvar)> = LazyLock::new(|| {
    (
        Mutex::new(Queue {
            items: VecDeque::new(),
            building: HashSet::new(),
            done: 0,
            started: false,
            failures: std::collections::HashMap::new(),
            finished: VecDeque::new(),
        }),
        Condvar::new(),
    )
});
static PAUSED: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    done: usize,
    total: usize,
    paused: bool,
    /// Seconds per photo lately (all workers together), once a few have
    /// finished: the first photos include starting up, and timing those
    /// made the estimate wildly long.
    seconds_per_photo: Option<f32>,
}

fn progress() -> Progress {
    let q = QUEUE.0.lock().unwrap();
    let seconds_per_photo = (q.finished.len() >= 4).then(|| {
        let span = q
            .finished
            .back()
            .unwrap()
            .duration_since(*q.finished.front().unwrap());
        span.as_secs_f32() / (q.finished.len() - 1) as f32
    });
    Progress {
        done: q.done,
        total: q.done + q.building.len() + q.items.len(),
        paused: PAUSED.load(Ordering::Relaxed),
        seconds_per_photo,
    }
}

fn emit(app: &tauri::AppHandle) {
    let _ = app.emit("cloud-previews-progress", progress());
}

/// Whether `photo` (only in the cloud) still needs something built here.
fn needs_work(photo: &Path, thumbs: Option<&Path>) -> bool {
    match smart_preview::mode_for(photo) {
        Mode::Previews => !smart_preview::is_current(photo),
        Mode::Thumbnails => thumbs.is_some_and(|t| {
            !crate::file_management::has_cached_thumbnail(&photo.to_string_lossy(), t)
        }),
        Mode::Nothing => false,
    }
}

/// Queue the cloud-only photos among `photos` (a folder just listed) that
/// need a preview or thumbnail, ahead of anything queued before.
pub fn enqueue(app: &tauri::AppHandle, photos: Vec<PathBuf>) {
    let thumbs = crate::file_management::get_thumb_cache_dir(app).ok();
    let wanted: Vec<PathBuf> = photos
        .into_iter()
        .filter(|p| crate::cloud_files::is_cloud_only(p) && needs_work(p, thumbs.as_deref()))
        .collect();
    if wanted.is_empty() {
        return;
    }
    {
        let mut q = QUEUE.0.lock().unwrap();
        q.items.retain(|p| !wanted.contains(p));
        for p in wanted.into_iter().rev() {
            if !q.building.contains(&p) {
                q.items.push_front(p);
            }
        }
        if !q.started {
            q.started = true;
            for _ in 0..WORKERS {
                let app = app.clone();
                std::thread::spawn(move || worker(app));
            }
        }
    }
    QUEUE.1.notify_all();
    emit(app);
}

fn worker(app: tauri::AppHandle) {
    loop {
        let photo = {
            let mut q = QUEUE.0.lock().unwrap();
            loop {
                if !PAUSED.load(Ordering::Relaxed)
                    && smart_preview::CARD_SEEN.load(Ordering::Relaxed)
                    && let Some(p) = q.items.pop_front()
                {
                    q.building.insert(p.clone());
                    break p;
                }
                if q.items.is_empty() && q.building.is_empty() {
                    q.done = 0;
                    q.finished.clear();
                }
                q = QUEUE.1.wait(q).unwrap();
            }
        };
        match build(&app, &photo) {
            Ok(()) => {
                {
                    let mut q = QUEUE.0.lock().unwrap();
                    q.building.remove(&photo);
                    q.failures.remove(&photo);
                    q.done += 1;
                    q.finished.push_back(std::time::Instant::now());
                    if q.finished.len() > PACE_WINDOW {
                        q.finished.pop_front();
                    }
                }
                // The library counts this folder's previews as they land.
                let _ = app.emit("cloud-preview-made", photo.to_string_lossy());
            }
            Err(e) => {
                log::warn!("[cloud] {}: {e}", photo.display());
                let again = {
                    let mut q = QUEUE.0.lock().unwrap();
                    q.building.remove(&photo);
                    let tries = q.failures.entry(photo.clone()).or_insert(0);
                    *tries += 1;
                    let again = *tries < TRIES;
                    if again {
                        q.items.push_back(photo);
                    } else {
                        q.done += 1;
                    }
                    again
                };
                // Give a busy or unreachable service a moment before the next.
                if again {
                    std::thread::sleep(std::time::Duration::from_secs(15));
                }
            }
        }
        emit(&app);
    }
}

/// Download `photo`, make what its mode keeps, show its thumbnail, and give
/// it back to the cloud.
fn build(app: &tauri::AppHandle, photo: &Path) -> Result<(), String> {
    let was_cloud_only = crate::cloud_files::is_cloud_only(photo);
    let thumbs = crate::file_management::get_thumb_cache_dir(app).ok();
    if !needs_work(photo, thumbs.as_deref()) {
        return Ok(());
    }
    let path_str = photo.to_string_lossy().into_owned();
    // Reading it downloads it.
    let bytes = std::fs::read(photo).map_err(|e| format!("could not download it: {e}"))?;
    let result = (|| {
        if smart_preview::mode_for(photo) == Mode::Previews {
            let frame = if crate::formats::is_raw_file(&path_str) {
                crate::color_engine::raw::decode_raw(&bytes, false, || Ok(()))
            } else {
                crate::color_engine::input::decode_profiled_photo(&bytes)
            }
            .map_err(|e| format!("could not decode it: {e:#}"))?;
            let exif = crate::exif_processing::read_exif_sidecar(photo)
                .or_else(|| Some(crate::exif_processing::read_exif_data(&path_str, &bytes)));
            smart_preview::write(photo, &frame, exif).map_err(|e| format!("{e:#}"))?;
        }
        drop(bytes);
        // Its thumbnail, with its edits, while the photo is here.
        let state = app.state::<crate::AppState>();
        let gpu = crate::gpu_processing::get_or_init_gpu_context(&state, app).ok();
        let settings = crate::app_settings::load_settings(app.clone()).unwrap_or_default();
        if let Some(dir) = &thumbs
            && let Some((data, rating, is_edited)) =
                crate::file_management::generate_single_thumbnail_and_cache(
                    &path_str,
                    dir,
                    gpu.as_ref(),
                    true,
                    app,
                    &settings,
                )
        {
            let _ = app.emit(
                "thumbnail-generated",
                serde_json::json!({ "path": path_str, "data": data, "rating": rating, "is_edited": is_edited }),
            );
        }
        Ok(())
    })();
    if was_cloud_only && let Err(e) = crate::cloud_files::free_local_copy(photo) {
        log::warn!(
            "[cloud] could not give {} back to the cloud: {e}",
            photo.display()
        );
    }
    result
}

/// Photos asked to be shown (their thumbnails requested: on screen in the
/// grid or on Home) go to the front: those already waiting, and cloud-only
/// ones not yet queued that have no preview (or, in the thumbnails mode, no
/// thumbnail). Home lists folders without queueing them, so this is how
/// what it shows gets built. Only file metadata is read here.
pub fn request(app: &tauri::AppHandle, photos: &[String]) {
    let thumbs = crate::file_management::get_thumb_cache_dir(app).ok();
    let wanted: Vec<PathBuf> = photos
        .iter()
        .filter(|p| !p.contains("?vc="))
        .map(PathBuf::from)
        .filter(|p| crate::cloud_files::is_cloud_only(p))
        .filter(|p| match smart_preview::mode_for(p) {
            Mode::Previews => !smart_preview::path_for(p).exists(),
            Mode::Thumbnails => thumbs.as_deref().is_some_and(|t| {
                !crate::file_management::has_cached_thumbnail(&p.to_string_lossy(), t)
            }),
            Mode::Nothing => false,
        })
        .collect();
    let wanted_set: HashSet<&Path> = wanted.iter().map(PathBuf::as_path).collect();
    let mut q = QUEUE.0.lock().unwrap();
    let mut changed = false;
    let mut front: VecDeque<PathBuf> = VecDeque::new();
    q.items.retain(|p| {
        if wanted_set.contains(p.as_path()) {
            front.push_back(p.clone());
            false
        } else {
            true
        }
    });
    for p in &wanted {
        if !front.contains(p) && !q.building.contains(p) {
            front.push_back(p.clone());
            changed = true;
        }
    }
    if front.is_empty() {
        return;
    }
    front.extend(q.items.drain(..));
    q.items = front;
    if !q.started {
        q.started = true;
        for _ in 0..WORKERS {
            let app = app.clone();
            std::thread::spawn(move || worker(app));
        }
    }
    drop(q);
    if changed {
        QUEUE.1.notify_all();
        emit(app);
    }
}

/// Settings changed (the first-time card answered, a mode changed): wake
/// the builder to look again.
pub fn wake() {
    QUEUE.1.notify_all();
}

#[tauri::command]
pub fn get_cloud_previews_progress() -> Progress {
    progress()
}

#[tauri::command]
pub fn set_cloud_previews_paused(paused: bool, app_handle: tauri::AppHandle) -> Progress {
    PAUSED.store(paused, Ordering::Relaxed);
    QUEUE.1.notify_all();
    emit(&app_handle);
    progress()
}
