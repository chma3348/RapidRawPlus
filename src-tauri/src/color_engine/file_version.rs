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
static HASHES: OnceLock<Mutex<HashMap<Key, String>>> = OnceLock::new();
pub(crate) fn digest(path: &Path, fresh: bool) -> Result<(String, Option<Vec<u8>>)> {
    let before = version(path)?;
    let key = (path.to_path_buf(), before.clone());
    let cache = HASHES.get_or_init(|| Mutex::new(HashMap::new()));
    if cfg!(unix)
        && !fresh
        && let Some(hash) = cache.lock().ok().and_then(|c| c.get(&key).cloned())
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
        cache.retain(|(p, _), _| p != path);
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, hash.clone());
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
}
