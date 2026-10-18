//! Photos kept by a cloud service (Nextcloud, Dropbox, iCloud Drive…) that
//! are not on this Mac.
//!
//! Those services use macOS's file provider: a photo not downloaded is a
//! placeholder, marked "dataless", that downloads itself the moment anything
//! reads it. Asking whether a file is dataless only reads its flags, so it
//! never starts a download. The library uses this to leave cloud-only photos
//! alone (no thumbnail or camera data read from them) until one is opened,
//! instead of downloading a whole library to make thumbnails.

use std::path::Path;

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
}
