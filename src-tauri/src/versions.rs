//! Versions: the files the app makes from a photo.
//!
//! Restoring, upscaling, denoising, expanding or converting a negative
//! writes a new file beside the original (`DSC03453_Restored.png`). On disk
//! these stay ordinary files, so Finder, backups and other programs see
//! them; in the app they are shown as versions of the original, one stack
//! per photo, rather than as unrelated pictures in the same folder.
//!
//! Two things tie a version to its original:
//!
//! - the version's sidecar records the original's file name and what was
//!   done (`derivedFrom`, `derivedKind`), written when the file is saved;
//! - for files made before that, or whose sidecar was lost, the name: the
//!   suffixes below after the original's stem, when a file with that stem
//!   is listed alongside it.
//!
//! Frames saved from a video are tied to their clip the same way, but the
//! library keeps them as separate pictures placed right after the clip.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Suffixes the app gives versions, as written in file names.
pub const VERSION_KINDS: &[&str] = &[
    "Restored",
    "Deblurred",
    "Upscaled",
    "Denoised",
    "Expanded",
    "Positive",
];

/// The kind recorded for a frame saved from a video.
pub const FRAME_KIND: &str = "Frame";

/// `{stem}_{kind}.{ext}` beside `source`, or `{stem}_{kind}_2.{ext}` and so
/// on when that name is taken: saving a second restore must not replace
/// the first.
pub fn free_version_path(source: &Path, kind: &str, ext: &str) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new(""));
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".into());
    let mut candidate = parent.join(format!("{stem}_{kind}.{ext}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{stem}_{kind}_{n}.{ext}"));
        n += 1;
    }
    candidate
}

/// Record in `output`'s sidecar that it was made from `source` (a real
/// path, not a virtual copy's) by `kind`. Runs after anything else that
/// writes the sidecar, so it is the last word.
pub fn record_version(output: &Path, source: &Path, kind: &str) {
    let sidecar = crate::exif_processing::get_primary_sidecar_path(output);
    let mut metadata = crate::exif_processing::load_sidecar(&sidecar);
    metadata.derived_from = source.file_name().map(|n| n.to_string_lossy().into_owned());
    metadata.derived_kind = Some(kind.to_string());
    match serde_json::to_string_pretty(&metadata) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&sidecar, json) {
                log::warn!("could not record the original of {output:?}: {e}");
            }
        }
        Err(e) => log::warn!("could not record the original of {output:?}: {e}"),
    }
}

/// Split a version's stem into (original stem, kind) by its suffix:
/// `DSC03453_Restored_2` → (`DSC03453`, `Restored`),
/// `clip_frame_00153` → (`clip`, `Frame`).
pub fn parse_version_stem(stem: &str) -> Option<(&str, &'static str)> {
    // A trailing `_2`, `_3`… added to avoid a clash.
    let base = match stem.rsplit_once('_') {
        Some((head, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            // `clip_frame_00153` also ends in digits; that is the frame
            // number, not a clash counter.
            if head.ends_with("_frame") { stem } else { head }
        }
        _ => stem,
    };
    for kind in VERSION_KINDS {
        if let Some(original) = base
            .strip_suffix(kind)
            .and_then(|s| s.strip_suffix('_'))
            .filter(|s| !s.is_empty())
        {
            return Some((original, kind));
        }
    }
    // Frames: `{stem}_frame_{00153}` or `{stem}_frame_{12s345}`.
    let (head, tail) = base.rsplit_once("_frame_")?;
    let frame_like = !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit() || b == b's')
        && tail.bytes().next().is_some_and(|b| b.is_ascii_digit());
    (frame_like && !head.is_empty()).then_some((head, FRAME_KIND))
}

/// For listed files that do not record their original, find it by name
/// among the other listed files in the same folder. Returns
/// (version path → (original path, kind)).
///
/// When several files share the original stem (a RAW and its JPEG), the
/// RAW is taken, as the file the version was most likely made from; a
/// frame's original must be a video.
pub fn originals_by_name(files: &[PathBuf]) -> HashMap<PathBuf, (PathBuf, &'static str)> {
    let mut by_stem: HashMap<(PathBuf, String), Vec<&PathBuf>> = HashMap::new();
    for f in files {
        if let (Some(parent), Some(stem)) = (f.parent(), f.file_stem()) {
            by_stem
                .entry((parent.to_path_buf(), stem.to_string_lossy().into_owned()))
                .or_default()
                .push(f);
        }
    }
    let mut found = HashMap::new();
    for f in files {
        let (Some(parent), Some(stem)) = (f.parent(), f.file_stem()) else {
            continue;
        };
        let stem = stem.to_string_lossy();
        let Some((original_stem, kind)) = parse_version_stem(&stem) else {
            continue;
        };
        let Some(candidates) = by_stem.get(&(parent.to_path_buf(), original_stem.to_string()))
        else {
            continue;
        };
        let pick = if kind == FRAME_KIND {
            candidates
                .iter()
                .find(|p| crate::formats::is_video_file(p))
                .copied()
        } else {
            candidates
                .iter()
                .find(|p| crate::formats::is_raw_file(p.to_string_lossy().as_ref()))
                .or_else(|| {
                    candidates
                        .iter()
                        .find(|p| !crate::formats::is_video_file(p))
                })
                .copied()
        };
        if let Some(original) = pick.filter(|p| *p != f) {
            found.insert(f.clone(), (original.clone(), kind));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_names_are_read_back() {
        assert_eq!(
            parse_version_stem("DSC03453_Restored"),
            Some(("DSC03453", "Restored"))
        );
        assert_eq!(
            parse_version_stem("DSC03453_Restored_2"),
            Some(("DSC03453", "Restored"))
        );
        assert_eq!(
            parse_version_stem("IMG_1_Upscaled_12"),
            Some(("IMG_1", "Upscaled"))
        );
        assert_eq!(
            parse_version_stem("DSC03453_Restored_Upscaled"),
            Some(("DSC03453_Restored", "Upscaled"))
        );
        assert_eq!(
            parse_version_stem("2026-09-15 20:30:21 +0000_frame_00153"),
            Some(("2026-09-15 20:30:21 +0000", "Frame"))
        );
        assert_eq!(
            parse_version_stem("clip_frame_12s345_2"),
            Some(("clip", "Frame"))
        );
        assert_eq!(parse_version_stem("DSC03453"), None);
        assert_eq!(parse_version_stem("IMG_2041"), None);
        assert_eq!(parse_version_stem("_Restored"), None);
        assert_eq!(parse_version_stem("Restored"), None);
        assert_eq!(parse_version_stem("clip_frame_"), None);
    }

    #[test]
    fn originals_are_found_among_the_listed_files() {
        let d = PathBuf::from("/photos");
        let files: Vec<PathBuf> = [
            "DSC1.ARW",
            "DSC1.JPG",
            "DSC1_Restored.tiff",
            "DSC1_Restored_2.tiff",
            "DSC1_Restored_Upscaled.png",
            "IMG_5.png",
            "IMG_5_Denoised.png",
            "Lonely_Restored.png",
            "clip.MOV",
            "clip_frame_00012.png",
            "photo_frame_00001.png",
            "photo.jpg",
        ]
        .iter()
        .map(|n| d.join(n))
        .collect();
        let found = originals_by_name(&files);
        let original = |n: &str| found.get(&d.join(n)).map(|(p, k)| (p.clone(), *k));
        assert_eq!(
            original("DSC1_Restored.tiff"),
            Some((d.join("DSC1.ARW"), "Restored"))
        );
        assert_eq!(
            original("DSC1_Restored_2.tiff"),
            Some((d.join("DSC1.ARW"), "Restored"))
        );
        assert_eq!(
            original("DSC1_Restored_Upscaled.png"),
            Some((d.join("DSC1_Restored.tiff"), "Upscaled"))
        );
        assert_eq!(
            original("IMG_5_Denoised.png"),
            Some((d.join("IMG_5.png"), "Denoised"))
        );
        assert_eq!(
            original("clip_frame_00012.png"),
            Some((d.join("clip.MOV"), "Frame"))
        );
        // No original listed, or not a video for a frame: left alone.
        assert_eq!(original("Lonely_Restored.png"), None);
        assert_eq!(original("photo_frame_00001.png"), None);
        assert_eq!(original("DSC1.ARW"), None);
        assert_eq!(original("DSC1.JPG"), None);
    }

    #[test]
    fn a_second_save_gets_a_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("DSC1.ARW");
        let first = free_version_path(&source, "Restored", "tiff");
        assert_eq!(first, dir.path().join("DSC1_Restored.tiff"));
        std::fs::write(&first, b"x").unwrap();
        let second = free_version_path(&source, "Restored", "tiff");
        assert_eq!(second, dir.path().join("DSC1_Restored_2.tiff"));
        std::fs::write(&second, b"x").unwrap();
        assert_eq!(
            free_version_path(&source, "Restored", "tiff"),
            dir.path().join("DSC1_Restored_3.tiff")
        );
    }

    #[test]
    fn the_original_is_recorded_in_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("DSC1_Restored.png");
        std::fs::write(&out, b"x").unwrap();
        record_version(&out, &dir.path().join("DSC1.ARW"), "Restored");
        let meta = crate::exif_processing::load_sidecar(
            &crate::exif_processing::get_primary_sidecar_path(&out),
        );
        assert_eq!(meta.derived_from.as_deref(), Some("DSC1.ARW"));
        assert_eq!(meta.derived_kind.as_deref(), Some("Restored"));
    }
}
