use std::convert::AsRef;
use std::path::Path;

pub const RAW_EXTENSIONS: &[(&str, &str)] = &[
    // Adobe
    ("dng", "Adobe Digital Negative"),
    // Apple
    ("pro", "Apple ProRAW"),
    // Arri
    ("ari", "ARRI Raw"),
    // Canon
    ("crw", "Canon Raw"),
    ("cr2", "Canon Raw 2"),
    ("cr3", "Canon Raw 3"),
    // Casio
    ("bay", "Casio"),
    // Contax
    ("raw", "Contax"),
    // DJI
    // ("dng", "DJI (uses DNG)"), // Covered by Adobe

    // Epson
    ("erf", "Epson Raw"),
    // Fuji
    ("raf", "Fuji Raw"),
    // Hasselblad
    ("3fr", "Hasselblad"),
    ("fff", "Hasselblad"),
    // Imacon / Phase One
    ("iiq", "Imacon/Phase One"),
    // Kodak
    ("kdc", "Kodak"),
    ("k25", "Kodak"),
    ("dcs", "Kodak"),
    ("dcr", "Kodak"),
    // Leaf
    ("mos", "Leaf"),
    // Leica
    ("rwl", "Leica Raw"),
    // ("dng", "Leica (uses DNG)"), // Covered by Adobe

    // Mamiya
    ("mef", "Mamiya"),
    // Minolta
    ("mrw", "Minolta Raw"),
    // Nikon
    ("nef", "Nikon Electronic Format"),
    ("nrw", "Nikon Raw"),
    // Olympus
    ("orf", "Olympus Raw"),
    // Panasonic
    ("rw2", "Panasonic Raw 2"),
    ("raw", "Panasonic Raw"),
    // Pentax
    ("pef", "Pentax Electronic File"),
    ("ptx", "Pentax"),
    // Phase One
    // ("iiq", "Phase One (same as Imacon)"), // Covered by Imacon

    // Ricoh
    // ("dng", "Ricoh (uses DNG)"), // Covered by Adobe

    // Samsung
    ("srw", "Samsung Raw"),
    // Sigma
    ("x3f", "Sigma"),
    // Sony
    ("arw", "Sony Alpha Raw"),
    ("srf", "Sony Raw"),
    ("sr2", "Sony Raw 2"),
]; // Tell me if your's is missing.

pub const NON_RAW_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "jxl", // Standard formats
    "heic", "heif", // Apple/ISO HEIF (decoded via sips on macOS)
    "exr", "hdr", // High Dynamic Range / Wide Gamut
    "tga", "ico", "dds", // Graphics & Icons
    "qoi", "ff", // Simple/Specialist formats
    "pnm", "pbm", "pgm", "ppm", "pam", // Netpbm family
];

/// Video the webview can play natively (WebKit decodes H.264/HEVC in
/// these containers), so viewing needs no decoder of our own.
pub const VIDEO_EXTENSIONS: &[&str] = &["mov", "mp4", "m4v"];

pub fn is_video_file<P: AsRef<Path>>(path: P) -> bool {
    let Some(ext) = path.as_ref().extension().and_then(|s| s.to_str()) else {
        return false;
    };
    VIDEO_EXTENSIONS
        .iter()
        .any(|video_ext| video_ext.eq_ignore_ascii_case(ext))
}

/// Anything the library can show: a photograph or a playable video.
pub fn is_supported_media_file<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();
    is_supported_image_file(path) || is_video_file(path)
}

pub fn is_raw_file<P: AsRef<Path>>(path: P) -> bool {
    let ext = match path.as_ref().extension().and_then(|s| s.to_str()) {
        Some(e) => e,
        None => return false,
    };

    RAW_EXTENSIONS
        .iter()
        .any(|(raw_ext, _)| raw_ext.eq_ignore_ascii_case(ext))
}

pub fn is_supported_image_file<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();

    let ext = match path.extension().and_then(|s| s.to_str()) {
        Some(e) => e,
        None => return false,
    };

    if RAW_EXTENSIONS
        .iter()
        .any(|(raw_ext, _)| raw_ext.eq_ignore_ascii_case(ext))
    {
        return true;
    }

    NON_RAW_EXTENSIONS
        .iter()
        .any(|non_raw_ext| non_raw_ext.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn videos_are_recognised_and_photographs_are_not() {
        for name in ["clip.mov", "CLIP.MOV", "a.mp4", "b.m4v"] {
            assert!(is_video_file(name), "{name} should be video");
            assert!(!is_supported_image_file(name), "{name} is not a photo");
            assert!(is_supported_media_file(name));
        }
        for name in ["a.jpg", "b.ARW", "c.png", "d.heic"] {
            assert!(!is_video_file(name), "{name} is not video");
            assert!(is_supported_media_file(name), "{name} should be media");
        }
        // Containers WebKit will not play are left out on purpose.
        for name in ["clip.mkv", "clip.avi", "clip.webm", "notes.txt"] {
            assert!(!is_video_file(name), "{name} should not be offered");
            assert!(!is_supported_media_file(name), "{name} should not list");
        }
    }
}
