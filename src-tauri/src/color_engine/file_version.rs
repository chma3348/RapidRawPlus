//! Bounded content cache. Unix identity/change-time catches replacements that
//! preserve length and mtime; other platforms conservatively reread bytes.
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Version {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
pub(crate) fn version(path: &Path) -> Result<Version> {
    let meta = std::fs::metadata(path)?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(Version {
        length: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(unix)]
        identity: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
    })
}
type Key = (PathBuf, Version);
/// Digests by file version, with when each was last used: the least
/// recently used goes when it is full, so thumbnails made in the
/// background never push out the photo the editor is on.
#[derive(Default)]
struct Hashes {
    entries: HashMap<Key, (String, u64)>,
    tick: u64,
}
const CAPACITY: usize = 64;
static HASHES: OnceLock<Mutex<Hashes>> = OnceLock::new();
pub(crate) fn digest(path: &Path, fresh: bool) -> Result<(String, Option<Vec<u8>>)> {
    let before = version(path)?;
    let key = (path.to_path_buf(), before.clone());
    let cache = HASHES.get_or_init(Default::default);
    if cfg!(unix)
        && !fresh
        && let Some(hash) = cache.lock().ok().and_then(|mut c| {
            c.tick += 1;
            let tick = c.tick;
            c.entries.get_mut(&key).map(|(hash, used)| {
                *used = tick;
                hash.clone()
            })
        })
    {
        return Ok((hash, None));
    }
    let bytes = std::fs::read(path)?;
    ensure!(
        before == version(path)?,
        "File changed while reading {}; retry",
        path.display()
    );
    let hash = blake3::hash(&bytes).to_hex().to_string();
    if let Ok(mut cache) = cache.lock() {
        cache.entries.retain(|(p, _), _| p != path);
        if cache.entries.len() >= CAPACITY
            && let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone())
        {
            cache.entries.remove(&oldest);
        }
        cache.tick += 1;
        let used = cache.tick;
        cache.entries.insert(key, (hash.clone(), used));
    }
    Ok((hash, Some(bytes)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_with_preserved_mtime_is_not_a_cache_hit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, b"abcd").unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let before = digest(&path, false).unwrap().0;
        if cfg!(unix) {
            assert!(digest(&path, false).unwrap().1.is_none());
        }
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"dcba").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert_ne!(digest(&path, false).unwrap().0, before);
        assert!(digest(&path, true).unwrap().1.is_some());
    }

    /// A file in use stays cached while many others pass through.
    #[test]
    fn the_file_in_use_outlasts_a_stream_of_others() {
        if !cfg!(unix) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("editing");
        std::fs::write(&kept, b"the open photo").unwrap();
        digest(&kept, false).unwrap();
        for i in 0..(CAPACITY * 3) {
            let other = dir.path().join(format!("thumb{i}"));
            std::fs::write(&other, format!("{i}")).unwrap();
            digest(&other, false).unwrap();
            // The editor renders again now and then.
            if i % 16 == 0 {
                assert!(digest(&kept, false).unwrap().1.is_none(), "evicted at {i}");
            }
        }
        assert!(digest(&kept, false).unwrap().1.is_none());
    }
}
