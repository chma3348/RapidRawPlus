//! Live updates: the library follows files changed outside the app.
//!
//! The library's root folders are watched (FSEvents on macOS, which is
//! recursive and cheap). When photos, videos or XMP sidecars appear,
//! disappear, are renamed or rewritten — by Finder, a camera import,
//! Lightroom — the app is told which folders changed, a moment after the
//! burst settles, and the grid and folder tree refresh themselves.
//!
//! What is ignored: the app's own sidecars (`.rrdata`), its temporary
//! files, hidden files, metadata-only changes (Finder tags, permissions),
//! and XMP the app itself has just written.
//!
//! Network shares do not report changes made by other computers, so the
//! open folder on a non-local volume is checked every few seconds instead.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const EVENT: &str = "library-changed";

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// Folders whose contents changed.
    pub folders: Vec<String>,
    /// Whether folders themselves appeared, vanished or were renamed.
    pub structure: bool,
}

static WATCHER: Mutex<Option<RecommendedWatcher>> = Mutex::new(None);
static POLL_GENERATION: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// The app's own writes
// ---------------------------------------------------------------------------

static OWN_WRITES: Mutex<Option<HashMap<PathBuf, Instant>>> = Mutex::new(None);

/// Note that the app just wrote `path`, so the watcher does not report it
/// back as an outside change.
pub fn note_own_write(path: &Path) {
    let mut guard = OWN_WRITES.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    map.retain(|_, t| now.duration_since(*t) < Duration::from_secs(10));
    map.insert(path.to_path_buf(), now);
}

fn written_by_us(path: &Path) -> bool {
    OWN_WRITES
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(path))
        .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
}

// ---------------------------------------------------------------------------
// Which events matter
// ---------------------------------------------------------------------------

fn is_xmp(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

/// What an event means for the library: changed folders, and whether
/// folder structure changed.
fn classify(event: &Event) -> Change {
    let mut change = Change::default();
    if matches!(
        event.kind,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_))
    ) {
        return change;
    }
    let mut folders = BTreeSet::new();
    for path in &event.paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') || name.ends_with(".rrdata") || name.ends_with(".rrexif") {
            continue;
        }
        if name.contains(".rapidraw-tmp") || written_by_us(path) {
            continue;
        }
        let is_dir = path.is_dir()
            || matches!(
                event.kind,
                EventKind::Create(notify::event::CreateKind::Folder)
                    | EventKind::Remove(notify::event::RemoveKind::Folder)
            )
            // A renamed-away or removed folder no longer exists to ask.
            || (!path.exists() && path.extension().is_none());
        if is_dir {
            change.structure = true;
            if let Some(parent) = path.parent() {
                folders.insert(parent.to_string_lossy().into_owned());
            }
            continue;
        }
        if (crate::formats::is_supported_media_file(path) || is_xmp(path))
            && let Some(parent) = path.parent()
        {
            folders.insert(parent.to_string_lossy().into_owned());
        }
    }
    change.folders = folders.into_iter().collect();
    change
}

fn merge(into: &mut Change, other: Change) {
    into.structure |= other.structure;
    for f in other.folders {
        if !into.folders.contains(&f) {
            into.folders.push(f);
        }
    }
}

// ---------------------------------------------------------------------------
// Watching
// ---------------------------------------------------------------------------

/// Watch these library roots (replacing any previous set).
pub fn watch_roots(app: AppHandle, roots: &[PathBuf]) -> Result<(), String> {
    // Events arrive with canonical paths; report them under the names the
    // library uses, which may go through a symlink.
    let aliases: Vec<(String, String)> = roots
        .iter()
        .filter_map(|r| {
            let canonical = r.canonicalize().ok()?;
            (canonical != *r).then(|| {
                (
                    canonical.to_string_lossy().into_owned(),
                    r.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    let watcher = watch_with(roots, move |mut change| {
        for folder in &mut change.folders {
            if let Some((canonical, given)) = aliases.iter().find(|(c, _)| folder.starts_with(c)) {
                *folder = format!("{given}{}", &folder[canonical.len()..]);
            }
        }
        let _ = app.emit(EVENT, &change);
    })?;
    // Dropping the old watcher ends its thread (its channel closes).
    *WATCHER.lock().unwrap() = Some(watcher);
    log::info!("Watching {} library folder(s) for changes", roots.len());
    Ok(())
}

/// Watch `roots`, calling `on_change` once per settled burst of changes.
/// The watching stops when the returned watcher is dropped.
fn watch_with(
    roots: &[PathBuf],
    on_change: impl Fn(Change) + Send + 'static,
) -> Result<RecommendedWatcher, String> {
    let (tx, rx) = mpsc::channel::<Event>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .map_err(|e| format!("could not watch the library: {e}"))?;
    for root in roots {
        if root.is_dir()
            && let Err(e) = watcher.watch(root, RecursiveMode::Recursive)
        {
            log::warn!("Not watching {}: {e}", root.display());
        }
    }

    // Collect a burst (an import, a folder move) into one report.
    std::thread::Builder::new()
        .name("library-watcher".into())
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut change = classify(&first);
                let settle = Instant::now() + Duration::from_millis(400);
                while let Some(wait) = settle.checked_duration_since(Instant::now()) {
                    match rx.recv_timeout(wait) {
                        Ok(event) => merge(&mut change, classify(&event)),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                if !change.folders.is_empty() || change.structure {
                    on_change(change);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(watcher)
}

/// Whether `path` is on this computer's own disks (not a network share).
fn is_local(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        let Ok(c) = CString::new(path.to_string_lossy().as_bytes()) else {
            return true;
        };
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut stat) } != 0 {
            return true;
        }
        (stat.f_flags & libc::MNT_LOCAL as u32) != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        true
    }
}

/// A cheap fingerprint of a folder: names, sizes and times of its media,
/// XMP and subfolders.
fn fingerprint(folder: &Path) -> (u64, u64) {
    let mut files = std::collections::hash_map::DefaultHasher::new();
    let mut dirs = std::collections::hash_map::DefaultHasher::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return (0, 0);
    };
    let mut items: Vec<_> = entries.filter_map(Result::ok).collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            entry.file_name().hash(&mut dirs);
        } else if crate::formats::is_supported_media_file(&path) || is_xmp(&path) {
            entry.file_name().hash(&mut files);
            meta.len().hash(&mut files);
            meta.modified().ok().hash(&mut files);
        }
    }
    (files.finish(), dirs.finish())
}

/// The folder the library has open. On a network share it is checked
/// every few seconds, since such shares do not report outside changes.
pub fn watch_open_folder(app: AppHandle, folder: Option<PathBuf>) {
    let generation = POLL_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(folder) = folder.filter(|f| f.is_dir() && !is_local(f)) else {
        return;
    };
    log::info!("Checking {} for changes (network volume)", folder.display());
    std::thread::spawn(move || {
        let mut last = fingerprint(&folder);
        loop {
            std::thread::sleep(Duration::from_secs(6));
            if POLL_GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            let now = fingerprint(&folder);
            if now != last {
                let _ = app.emit(
                    EVENT,
                    &Change {
                        folders: vec![folder.to_string_lossy().into_owned()],
                        structure: now.1 != last.1,
                    },
                );
                last = now;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, MetadataKind, RemoveKind};

    fn event(kind: EventKind, paths: &[&Path]) -> Event {
        let mut e = Event::new(kind);
        for p in paths {
            e = e.add_path(p.to_path_buf());
        }
        e
    }

    #[test]
    fn only_photos_xmp_and_folders_count() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let folder = d.to_string_lossy().to_string();
        let photo = d.join("DSC1.ARW");
        std::fs::write(&photo, b"x").unwrap();

        let c = classify(&event(EventKind::Create(CreateKind::File), &[&photo]));
        assert_eq!(c.folders, std::slice::from_ref(&folder));
        assert!(!c.structure);

        for ignored in [
            "DSC1.ARW.rrdata",
            ".DS_Store",
            "DSC1.xmp.rapidraw-tmp",
            "notes.txt",
        ] {
            let p = d.join(ignored);
            let c = classify(&event(EventKind::Create(CreateKind::File), &[&p]));
            assert!(c.folders.is_empty(), "{ignored} should be ignored");
        }

        // Finder tags and permissions are metadata only.
        let c = classify(&event(
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Extended)),
            &[&photo],
        ));
        assert_eq!(c, Change::default());

        // Another program rewrote the XMP; the app's own write is ignored.
        let xmp = d.join("DSC1.xmp");
        std::fs::write(&xmp, b"x").unwrap();
        let rewrite = event(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &[&xmp],
        );
        assert_eq!(classify(&rewrite).folders, std::slice::from_ref(&folder));
        note_own_write(&xmp);
        assert!(classify(&rewrite).folders.is_empty());

        // A folder removed.
        let gone = d.join("Old Shoot");
        let c = classify(&event(EventKind::Remove(RemoveKind::Folder), &[&gone]));
        assert!(c.structure);
        assert_eq!(c.folders, [folder]);
    }

    #[test]
    fn the_fingerprint_notices_new_photos() {
        let dir = tempfile::tempdir().unwrap();
        let before = fingerprint(dir.path());
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        assert_eq!(fingerprint(dir.path()), before, "other files do not count");
        std::fs::write(dir.path().join("DSC1.ARW"), b"x").unwrap();
        let after = fingerprint(dir.path());
        assert_ne!(after.0, before.0);
        assert_eq!(after.1, before.1);
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_ne!(fingerprint(dir.path()).1, before.1);
    }

    #[test]
    fn changes_made_outside_arrive_once_settled() {
        let dir = tempfile::tempdir().unwrap();
        // FSEvents reports the canonical path (/private/var/… on macOS).
        let root = dir.path().canonicalize().unwrap();
        let (tx, rx) = mpsc::channel();
        let _watcher = watch_with(std::slice::from_ref(&root), move |c| {
            let _ = tx.send(c);
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        // A burst: three photos and a sidecar, as an import writes them.
        for n in ["a.ARW", "b.ARW", "c.ARW", "a.ARW.rrdata"] {
            std::fs::write(root.join(n), b"x").unwrap();
        }
        let change = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no change reported");
        assert_eq!(change.folders, [root.to_string_lossy().to_string()]);
        // One report for the burst.
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err());
    }

    #[test]
    fn local_disks_are_local() {
        assert!(is_local(&std::env::temp_dir()));
    }
}
