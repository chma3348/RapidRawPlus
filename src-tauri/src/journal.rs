//! A record of file operations, so each can be undone.
//!
//! Moving, renaming, copying, duplicating and deleting photos (and
//! creating, renaming or deleting folders) each add one entry listing
//! exactly which files went where. Undo replays an entry backwards: moved
//! files go back, created files go to the Trash, trashed files come back
//! out of it. Deleting goes through the Trash, and on macOS through
//! Finder's own call, which says where each file landed so it can be put
//! back exactly.
//!
//! The journal lasts for the session, like an editor's undo history.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A file or folder moved or renamed. Undo moves it back.
    Moved { from: PathBuf, to: PathBuf },
    /// A file the operation made. Undo sends it to the Trash.
    Created { path: PathBuf },
    /// A file or folder sent to the Trash, and where it is there (when
    /// known). Undo puts it back.
    Trashed { from: PathBuf, at: Option<PathBuf> },
    /// A folder the operation made. Undo removes it if still empty.
    FolderCreated { path: PathBuf },
}

#[derive(Debug, Clone)]
struct Operation {
    id: u64,
    label: String,
    at_ms: u64,
    actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: u64,
    pub label: String,
    pub at_ms: u64,
    pub undoable: bool,
}

/// What an undo did, for the caller to report and to update albums.
#[derive(Debug, Default)]
pub struct UndoReport {
    pub label: String,
    /// Files or folders now back at their old path: (where it was, where
    /// it is again).
    pub moved_back: Vec<(PathBuf, PathBuf)>,
    pub problems: Vec<String>,
}

const KEEP: usize = 100;
static JOURNAL: Mutex<Vec<Operation>> = Mutex::new(Vec::new());
static NEXT_ID: Mutex<u64> = Mutex::new(1);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn undoable(actions: &[Action]) -> bool {
    actions
        .iter()
        .all(|a| !matches!(a, Action::Trashed { at: None, .. }))
}

/// Add an operation. Empty ones are not recorded.
pub fn record(label: impl Into<String>, actions: Vec<Action>) {
    if actions.is_empty() {
        return;
    }
    let id = {
        let mut next = NEXT_ID.lock().unwrap();
        let id = *next;
        *next += 1;
        id
    };
    let mut journal = JOURNAL.lock().unwrap();
    journal.push(Operation {
        id,
        label: label.into(),
        at_ms: now_ms(),
        actions,
    });
    if journal.len() > KEEP {
        let excess = journal.len() - KEEP;
        journal.drain(..excess);
    }
}

/// The session's operations, newest first.
pub fn history() -> Vec<HistoryEntry> {
    JOURNAL
        .lock()
        .unwrap()
        .iter()
        .rev()
        .map(|op| HistoryEntry {
            id: op.id,
            label: op.label.clone(),
            at_ms: op.at_ms,
            undoable: undoable(&op.actions),
        })
        .collect()
}

/// Move one file or folder: a rename on the same volume (instant,
/// nothing copied), otherwise a copy. Returns whether it was copied, so
/// the caller removes the original once everything has arrived.
pub fn move_or_copy(from: &Path, to: &Path) -> Result<bool, String> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices && from.is_file() => {
            std::fs::copy(from, to)
                .map(|_| true)
                .map_err(|e| format!("Could not copy {} to {}: {e}", from.display(), to.display()))
        }
        Err(e) => Err(format!(
            "Could not move {} to {}: {e}",
            from.display(),
            to.display()
        )),
    }
}

/// Send `path` to the Trash. Returns where it is in the Trash when the
/// system says (macOS), so it can be put back.
pub fn trash(path: &Path) -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "macos")]
    {
        macos::trash_item(path)
    }
    #[cfg(all(
        not(target_os = "macos"),
        any(target_os = "windows", target_os = "linux")
    ))]
    {
        trash::delete(path).map(|_| None).map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = path;
        Err("This platform has no Trash".to_string())
    }
}

/// Trash every path, falling back to deleting outright where a volume
/// has no Trash (some network shares) — such deletions cannot be undone.
pub fn trash_all(paths: &[PathBuf]) -> Result<Vec<Action>, String> {
    let mut actions = Vec::with_capacity(paths.len());
    for path in paths {
        match trash(path) {
            Ok(at) => actions.push(Action::Trashed {
                from: path.clone(),
                at,
            }),
            Err(e) => {
                log::warn!(
                    "Could not move {} to the Trash ({e}); deleting it permanently.",
                    path.display()
                );
                if path.is_dir() {
                    std::fs::remove_dir_all(path)
                } else {
                    std::fs::remove_file(path)
                }
                .map_err(|e| format!("Failed to delete {}: {e}", path.display()))?;
                actions.push(Action::Trashed {
                    from: path.clone(),
                    at: None,
                });
            }
        }
    }
    Ok(actions)
}

/// Undo the newest operation.
pub fn undo_last() -> Result<UndoReport, String> {
    let op = JOURNAL.lock().unwrap().pop().ok_or("Nothing to undo")?;
    let mut report = UndoReport {
        label: op.label.clone(),
        ..Default::default()
    };
    for action in op.actions.iter().rev() {
        let result = match action {
            Action::Moved { from, to } => undo_move(from, to).map(|()| {
                report.moved_back.push((to.clone(), from.clone()));
            }),
            Action::Created { path } => {
                if path.exists() {
                    trash(path).map(|_| ())
                } else {
                    Ok(())
                }
            }
            Action::Trashed { from, at: Some(at) } => put_back(at, from),
            Action::Trashed { from, at: None } => Err(format!(
                "{} was deleted permanently and cannot be restored",
                from.display()
            )),
            Action::FolderCreated { path } => match std::fs::remove_dir(path) {
                Ok(()) => Ok(()),
                Err(_) if !path.exists() => Ok(()),
                Err(_) => Err(format!(
                    "{} is no longer empty, so it was kept",
                    path.display()
                )),
            },
        };
        if let Err(e) = result {
            report.problems.push(e);
        }
    }
    Ok(report)
}

fn undo_move(from: &Path, to: &Path) -> Result<(), String> {
    if !to.exists() {
        return Err(format!("{} is no longer there", to.display()));
    }
    if from.exists() {
        return Err(format!(
            "{} exists again, so {} was left where it is",
            from.display(),
            to.display()
        ));
    }
    if let Some(parent) = from.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if move_or_copy(to, from)? {
        // Came back across volumes as a copy; remove the one we moved.
        std::fs::remove_file(to).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn put_back(at: &Path, from: &Path) -> Result<(), String> {
    if !at.exists() {
        return Err(format!(
            "{} is no longer in the Trash",
            from.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    if from.exists() {
        return Err(format!("{} exists again", from.display()));
    }
    if let Some(parent) = from.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    move_or_copy(at, from).and_then(|copied| {
        if copied {
            std::fs::remove_file(at).map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use objc::runtime::{BOOL, NO, Object};
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::{CStr, CString};
    use std::path::{Path, PathBuf};

    /// `-[NSFileManager trashItemAtURL:resultingItemURL:error:]`, which
    /// moves the item to the right Trash for its volume and says where.
    pub fn trash_item(path: &Path) -> Result<Option<PathBuf>, String> {
        let c_path = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        unsafe {
            let pool: *mut Object = msg_send![class!(NSAutoreleasePool), new];
            let result = (|| {
                let ns_path: *mut Object =
                    msg_send![class!(NSString), stringWithUTF8String: c_path.as_ptr()];
                if ns_path.is_null() {
                    return Err("could not name the file for the Trash".to_string());
                }
                let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: ns_path];
                let manager: *mut Object = msg_send![class!(NSFileManager), defaultManager];
                let mut resulting: *mut Object = std::ptr::null_mut();
                let mut error: *mut Object = std::ptr::null_mut();
                let ok: BOOL = msg_send![manager, trashItemAtURL: url
                                                 resultingItemURL: &mut resulting
                                                 error: &mut error];
                if ok == NO {
                    let message = if error.is_null() {
                        "the Trash refused it".to_string()
                    } else {
                        let desc: *mut Object = msg_send![error, localizedDescription];
                        ns_string(desc).unwrap_or_else(|| "the Trash refused it".into())
                    };
                    return Err(message);
                }
                if resulting.is_null() {
                    return Ok(None);
                }
                let ns: *mut Object = msg_send![resulting, path];
                Ok(ns_string(ns).map(PathBuf::from))
            })();
            let () = msg_send![pool, drain];
            result
        }
    }

    unsafe fn ns_string(ns: *mut Object) -> Option<String> {
        if ns.is_null() {
            return None;
        }
        let utf8: *const std::os::raw::c_char = unsafe { msg_send![ns, UTF8String] };
        if utf8.is_null() {
            return None;
        }
        Some(
            unsafe { CStr::from_ptr(utf8) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // The journal is process-wide; keep these tests in one function so
    // they do not interleave.
    #[test]
    fn operations_undo_in_reverse() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        fs::write(p("a.ARW"), b"a").unwrap();
        fs::create_dir(p("sub")).unwrap();

        // A move, undone.
        assert!(!move_or_copy(&p("a.ARW"), &p("sub/a.ARW")).unwrap());
        record(
            "Move 1 photo to sub",
            vec![Action::Moved {
                from: p("a.ARW"),
                to: p("sub/a.ARW"),
            }],
        );
        assert_eq!(history()[0].label, "Move 1 photo to sub");
        let report = undo_last().unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert!(p("a.ARW").exists() && !p("sub/a.ARW").exists());

        // A copy, undone: the copy goes to the Trash.
        fs::write(p("a_copy_1.ARW"), b"a").unwrap();
        record(
            "Copy",
            vec![Action::Created {
                path: p("a_copy_1.ARW"),
            }],
        );
        assert!(undo_last().unwrap().problems.is_empty());
        assert!(!p("a_copy_1.ARW").exists());

        // A delete, undone: back out of the Trash where the system says.
        #[cfg(target_os = "macos")]
        {
            let actions = trash_all(&[p("a.ARW")]).unwrap();
            assert!(!p("a.ARW").exists());
            assert!(matches!(&actions[0], Action::Trashed { at: Some(_), .. }));
            record("Delete 1 photo", actions);
            let report = undo_last().unwrap();
            assert!(report.problems.is_empty(), "{:?}", report.problems);
            assert_eq!(fs::read(p("a.ARW")).unwrap(), b"a");
        }

        // A folder made, undone only while empty.
        fs::create_dir(p("new")).unwrap();
        record("New folder", vec![Action::FolderCreated { path: p("new") }]);
        undo_last().unwrap();
        assert!(!p("new").exists());

        assert!(record_nothing_is_ignored());
    }

    fn record_nothing_is_ignored() -> bool {
        let before = history().len();
        record("nothing", vec![]);
        history().len() == before
    }
}
