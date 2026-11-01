//! Photos kept by a cloud service (Nextcloud, Dropbox, iCloud Drive…) that
//! are not on this Mac.
//!
//! Those services use macOS's file provider: a photo not downloaded is a
//! placeholder, marked "dataless", that downloads itself the moment anything
//! reads it. Asking whether a file is dataless only reads its flags, so it
//! never starts a download. The library uses this to leave cloud-only photos
//! alone (no thumbnail or camera data read from them) until one is opened,
//! instead of downloading a whole library to make thumbnails.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// macOS's flag for a file whose contents are not on this Mac (`SF_DATALESS`).
#[cfg(target_os = "macos")]
const SF_DATALESS: u32 = 0x4000_0000;

/// Whether `path`'s contents are only in the cloud. Never downloads it.
#[cfg(target_os = "macos")]
pub fn is_cloud_only(path: &Path) -> bool {
    use std::os::macos::fs::MetadataExt;
    treated_as_cloud(path)
        || std::fs::symlink_metadata(path).is_ok_and(|m| m.st_flags() & SF_DATALESS != 0)
}

/// For checking previews without a cloud service: `DARKROOM_CLOUD_ONLY=<folder>`
/// treats every photo in that folder as only in the cloud.
fn treated_as_cloud(path: &Path) -> bool {
    std::env::var_os("DARKROOM_CLOUD_ONLY").is_some_and(|dir| path.starts_with(dir))
}

#[cfg(not(target_os = "macos"))]
pub fn is_cloud_only(_path: &Path) -> bool {
    false
}

/// Give `path`'s local copy back to the cloud service, so the photo is only
/// in the cloud again (its space freed). For photos this app downloaded
/// itself, to make a preview or thumbnail. The service keeps the file.
#[cfg(target_os = "macos")]
pub fn free_local_copy(path: &Path) -> Result<(), String> {
    use objc::runtime::{BOOL, NO, Object};
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CString;
    let c_path = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    unsafe {
        let pool: *mut Object = msg_send![class!(NSAutoreleasePool), new];
        let result = (|| {
            let ns_path: *mut Object =
                msg_send![class!(NSString), stringWithUTF8String: c_path.as_ptr()];
            if ns_path.is_null() {
                return Err("could not name the file".to_string());
            }
            let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: ns_path];
            let manager: *mut Object = msg_send![class!(NSFileManager), defaultManager];
            let mut error: *mut Object = std::ptr::null_mut();
            let ok: BOOL = msg_send![manager, evictUbiquitousItemAtURL: url error: &mut error];
            if ok == NO {
                // macOS says why (the file busy, the service not answering,
                // not a cloud file at all): worth having in the log.
                if error.is_null() {
                    return Err("the cloud service kept its local copy".to_string());
                }
                let code: isize = msg_send![error, code];
                let domain: *mut Object = msg_send![error, domain];
                let desc: *mut Object = msg_send![error, localizedDescription];
                return Err(format!(
                    "the cloud service kept its local copy: {} ({} {code})",
                    ns_string(desc).unwrap_or_else(|| "no reason given".into()),
                    ns_string(domain).unwrap_or_default(),
                ));
            }
            Ok(())
        })();
        let () = msg_send![pool, drain];
        result
    }
}

#[cfg(target_os = "macos")]
unsafe fn ns_string(ns: *mut objc::runtime::Object) -> Option<String> {
    use objc::{msg_send, sel, sel_impl};
    if ns.is_null() {
        return None;
    }
    let utf8: *const std::os::raw::c_char = unsafe { msg_send![ns, UTF8String] };
    if utf8.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(not(target_os = "macos"))]
pub fn free_local_copy(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Whether the file itself is a dataless placeholder (its flags only; unlike
/// `is_cloud_only`, the testing switch does not count). For files the app
/// writes beside photos, such as previews synced from another Mac.
#[cfg(target_os = "macos")]
pub fn is_dataless(path: &Path) -> bool {
    use std::os::macos::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.st_flags() & SF_DATALESS != 0)
}

#[cfg(not(target_os = "macos"))]
pub fn is_dataless(_path: &Path) -> bool {
    false
}

/// What this app has to give back to the cloud.
struct Downloads {
    /// The photo open in the editor.
    open: Option<PathBuf>,
    /// Photos this app brought down and has not given back yet. Kept apart
    /// from whether each is in the cloud now: once read, a photo no longer
    /// is, and one reopened (or exported) after that must still go back.
    ours: BTreeSet<PathBuf>,
    /// Being given back by a background thread, with its retries.
    returning: BTreeSet<PathBuf>,
}

static DOWNLOADS: Mutex<Downloads> = Mutex::new(Downloads {
    open: None,
    ours: BTreeSet::new(),
    returning: BTreeSet::new(),
});

fn downloads() -> std::sync::MutexGuard<'static, Downloads> {
    DOWNLOADS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The editor moves to `photo` (only in the cloud as it opens, or not): the
/// photo to give back, if the one open before was brought down by this app.
fn switch(d: &mut Downloads, photo: &Path, cloud_only: bool) -> Option<PathBuf> {
    if cloud_only {
        d.ours.insert(photo.to_path_buf());
    }
    d.open
        .replace(photo.to_path_buf())
        .filter(|p| p != photo && d.ours.contains(p))
}

/// A photo is being opened: give back the previous one if this app brought
/// it down, and remember this one if it is only in the cloud.
pub fn opening(photo: &Path) {
    let cloud_only = is_cloud_only(photo);
    let previous = switch(&mut downloads(), photo, cloud_only);
    if let Some(previous) = previous {
        give_back_later(previous);
    }
}

/// Whether `photo` is the one open in the editor.
pub fn is_open(photo: &Path) -> bool {
    downloads().open.as_deref() == Some(photo)
}

/// This app is about to read `photo`, only in the cloud, so downloading it:
/// it is to be given back once done with.
pub fn brought_down(photo: &Path) {
    downloads().ours.insert(photo.to_path_buf());
}

/// Whether this app brought `photo` down and has not given it back yet.
pub fn is_ours(photo: &Path) -> bool {
    downloads().ours.contains(photo)
}

/// How long to wait before each further try at giving a photo back: the
/// service may be busy (still uploading an edit beside it, or offline).
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

#[derive(Debug, PartialEq, Eq)]
enum Attempt {
    /// Given back, or nothing (left) to give back.
    Done,
    /// Still being used: whatever uses it gives it back when it is done.
    Deferred,
    Failed,
}

/// One try at giving `photo` back.
fn attempt(photo: &Path) -> Attempt {
    if !is_ours(photo) {
        return Attempt::Done;
    }
    // Open in the editor, or read in full by an export or a tool: evicting
    // it now would pull it out from under them. Leaving the photo, or the
    // last of those uses ending, gives it back.
    if is_open(photo) || crate::smart_preview::original_in_use(photo) {
        return Attempt::Deferred;
    }
    if is_cloud_only(photo) {
        downloads().ours.remove(photo);
        return Attempt::Done;
    }
    match free_local_copy(photo) {
        Ok(()) => {
            downloads().ours.remove(photo);
            log::info!("[cloud] gave {} back to the cloud", photo.display());
            Attempt::Done
        }
        Err(e) => {
            log::warn!(
                "[cloud] could not give {} back to the cloud: {e}",
                photo.display()
            );
            Attempt::Failed
        }
    }
}

/// Try again later, a few times, in the background (one thread per photo).
/// A photo still not given back stays ours, so leaving it in the editor (or
/// the next export of it) tries again.
fn retry_in_background(photo: PathBuf, try_now: bool) {
    if !downloads().returning.insert(photo.clone()) {
        return;
    }
    std::thread::spawn(move || {
        let tries = try_now
            .then_some(Duration::ZERO)
            .into_iter()
            .chain(RETRY_DELAYS);
        let mut last = Attempt::Failed;
        for delay in tries {
            std::thread::sleep(delay);
            last = attempt(&photo);
            if last != Attempt::Failed {
                break;
            }
        }
        if last == Attempt::Failed {
            log::warn!(
                "[cloud] still could not give {} back; trying again when next done with it",
                photo.display()
            );
        }
        downloads().returning.remove(&photo);
    });
}

/// Give `photo` back to the cloud in the background, if this app brought it
/// down and nothing is using it; retried a few times if the service refuses.
pub fn give_back_later(photo: PathBuf) {
    brought_down(&photo);
    retry_in_background(photo, true);
}

/// Give `photo` back now (the builder, which downloads one after another and
/// must not pile them up), retrying later in the background if refused.
pub fn give_back_now(photo: &Path) {
    brought_down(photo);
    if attempt(photo) == Attempt::Failed {
        retry_in_background(photo.to_path_buf(), false);
    }
}

/// `is_cloud_only` for a library path, which may name a virtual copy
/// (`photo.ARW?vc=1a2b3c`): the copy is in the cloud when its photo is.
pub fn is_cloud_only_str(path: &str) -> bool {
    let source = path.split("?vc=").next().unwrap_or(path);
    is_cloud_only(Path::new(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_file_is_not_cloud_only() {
        let dir = std::env::temp_dir().join("darkroom-cloud-files-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("photo.jpg");
        std::fs::write(&file, b"local").unwrap();
        assert!(!is_cloud_only(&file));
        assert!(!is_cloud_only_str(&format!("{}?vc=1a2b3c", file.display())));
        assert!(!is_cloud_only(&dir.join("missing.jpg")));
    }

    fn empty() -> Downloads {
        Downloads {
            open: None,
            ours: BTreeSet::new(),
            returning: BTreeSet::new(),
        }
    }

    #[test]
    fn a_photo_brought_down_is_given_back_when_left() {
        let mut d = empty();
        let (a, b) = (Path::new("/cloud/a.ARW"), Path::new("/cloud/b.ARW"));
        assert_eq!(switch(&mut d, a, true), None);
        assert_eq!(switch(&mut d, b, false), Some(a.to_path_buf()));
        // A photo the user downloaded themselves is never given back.
        assert_eq!(switch(&mut d, a, false), None);
    }

    #[test]
    fn a_photo_reopened_after_downloading_is_still_given_back() {
        let mut d = empty();
        let (a, b) = (Path::new("/cloud/a.ARW"), Path::new("/cloud/b.ARW"));
        switch(&mut d, a, true);
        // Opening it read it, so the service could not take it back yet: it
        // is reopened while here, no longer only in the cloud.
        switch(&mut d, b, false);
        assert_eq!(switch(&mut d, a, false), None);
        assert_eq!(switch(&mut d, b, false), Some(a.to_path_buf()));
        // Reopening the same photo gives nothing back.
        assert_eq!(switch(&mut d, b, false), None);
    }

    #[test]
    fn a_photo_not_brought_down_is_left_alone() {
        let file = std::env::temp_dir().join("darkroom-cloud-files-not-ours.jpg");
        assert_eq!(attempt(&file), Attempt::Done);
    }

    /// macOS's reason comes through when it refuses (a file not kept by any
    /// cloud service, here).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_refusal_says_why() {
        let dir = std::env::temp_dir().join("darkroom-cloud-files-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("evict.jpg");
        std::fs::write(&file, b"local").unwrap();
        let e = free_local_copy(&file).unwrap_err();
        assert!(e.contains('('), "{e}");
    }
}
