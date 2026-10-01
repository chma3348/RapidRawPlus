//! Colour labels as Finder tags.
//!
//! A red label in the app shows as Finder's red dot on the file, and
//! Spotlight finds it with `tag:Red`. Finder keeps tags in an extended
//! attribute (a binary property list of "Name\ncolour-number" strings),
//! so the photo's bytes and modification time are untouched. Only the
//! five colours the app uses are managed; any other tags a person added
//! in Finder stay. Volumes without extended attributes (some NAS shares)
//! are skipped quietly.

use std::path::Path;

/// The app's colour names with Finder's tag name and colour number.
const COLOURS: [(&str, &str, u8); 5] = [
    ("red", "Red", 6),
    ("yellow", "Yellow", 5),
    ("green", "Green", 2),
    ("blue", "Blue", 4),
    ("purple", "Purple", 3),
];

#[cfg(target_os = "macos")]
const ATTR: &str = "com.apple.metadata:_kMDItemUserTags";

/// Whether a Finder tag entry is one of the colours the app manages.
fn is_managed(entry: &str) -> bool {
    let name = entry.split('\n').next().unwrap_or(entry);
    COLOURS.iter().any(|(_, finder, _)| *finder == name)
}

/// The tag list with the app's colour replaced by `color` (or removed).
fn retag(mut tags: Vec<String>, color: Option<&str>) -> Vec<String> {
    tags.retain(|t| !is_managed(t));
    if let Some((_, name, n)) = color.and_then(|c| {
        COLOURS
            .iter()
            .find(|(app, _, _)| app.eq_ignore_ascii_case(c))
    }) {
        tags.push(format!("{name}\n{n}"));
    }
    tags
}

/// Show `color` (an app colour name, or `None` for no label) on `image`
/// in Finder.
#[cfg(target_os = "macos")]
pub fn set_color_label(image: &Path, color: Option<&str>) {
    let current: Vec<String> = xattr::get(image, ATTR)
        .ok()
        .flatten()
        .and_then(|bytes| plist::from_bytes(&bytes).ok())
        .unwrap_or_default();
    let tags = retag(current.clone(), color);
    if tags == current {
        return;
    }
    let result = if tags.is_empty() {
        xattr::remove(image, ATTR)
    } else {
        let mut buf = Vec::new();
        if let Err(e) = plist::to_writer_binary(&mut buf, &tags) {
            log::debug!("Finder tags for {image:?}: {e}");
            return;
        }
        xattr::set(image, ATTR, &buf)
    };
    if let Err(e) = result {
        log::debug!("Finder tags not set on {image:?}: {e}");
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_color_label(_image: &Path, _color: Option<&str>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_finder_tags_are_kept() {
        let tags = vec![
            "Work\n0".to_string(),
            "Red\n6".to_string(),
            "Orange\n7".to_string(),
        ];
        assert_eq!(
            retag(tags.clone(), Some("blue")),
            ["Work\n0", "Orange\n7", "Blue\n4"]
        );
        assert_eq!(retag(tags, None), ["Work\n0", "Orange\n7"]);
        assert_eq!(retag(vec![], Some("unknown")), Vec::<String>::new());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn labels_reach_finder() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("DSC1.ARW");
        std::fs::write(&f, b"raw").unwrap();
        let before = std::fs::metadata(&f).unwrap().modified().unwrap();
        set_color_label(&f, Some("red"));
        let read = |p: &Path| -> Vec<String> {
            xattr::get(p, ATTR)
                .unwrap()
                .map(|b| plist::from_bytes(&b).unwrap())
                .unwrap_or_default()
        };
        assert_eq!(read(&f), ["Red\n6"]);
        set_color_label(&f, None);
        assert!(read(&f).is_empty());
        assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), before);
    }
}
