//! Viewing video files alongside photographs.
//!
//! Playback itself is the webview's job: the formats listed here are the
//! ones WebKit decodes natively (H.264/HEVC in MOV or MP4), so the player
//! is a `<video>` element pointed at the file and costs nothing.
//!
//! What the app needs from Rust is the two things a grid needs: a poster
//! frame and the basic facts. Both come from macOS itself — `qlmanage` for
//! the frame, Spotlight's metadata for duration and size — for the same
//! reason HEIC decoding already shells out to `sips`: the system has the
//! codecs, and bundling a video decoder to draw one thumbnail would be a
//! poor trade. On other platforms both return nothing and the UI falls
//! back to a placeholder.

use anyhow::{Context, Result, anyhow};
use image::DynamicImage;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub duration_seconds: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub codecs: Vec<String>,
}

/// Parse the `mdls` output we ask for. Kept separate from the command so
/// the parsing is testable without a Mac or a file.
pub fn parse_mdls(output: &str) -> VideoInfo {
    let mut info = VideoInfo::default();
    let mut in_codecs = false;
    for line in output.lines() {
        let line = line.trim();
        if in_codecs {
            if line.starts_with(')') {
                in_codecs = false;
                continue;
            }
            let codec = line.trim_end_matches(',').trim_matches('"').trim();
            if !codec.is_empty() {
                info.codecs.push(codec.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "kMDItemDurationSeconds" => info.duration_seconds = value.parse().ok(),
            "kMDItemPixelWidth" => info.width = value.parse().ok(),
            "kMDItemPixelHeight" => info.height = value.parse().ok(),
            "kMDItemCodecs" if value == "(" => in_codecs = true,
            _ => {}
        }
    }
    info
}

#[cfg(target_os = "macos")]
pub fn video_info(path: &Path) -> Result<VideoInfo> {
    let output = std::process::Command::new("mdls")
        .args([
            "-name",
            "kMDItemDurationSeconds",
            "-name",
            "kMDItemPixelWidth",
            "-name",
            "kMDItemPixelHeight",
            "-name",
            "kMDItemCodecs",
        ])
        .arg(path)
        .output()
        .context("could not run mdls")?;
    Ok(parse_mdls(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(not(target_os = "macos"))]
pub fn video_info(_path: &Path) -> Result<VideoInfo> {
    Ok(VideoInfo::default())
}

/// A still from the video to stand in for it in the grid.
#[cfg(target_os = "macos")]
pub fn poster_frame(path: &Path) -> Result<DynamicImage> {
    let dir = std::env::temp_dir().join(format!(
        "rapidraw_poster_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).context("poster frame temp dir")?;
    let cleanup = scopeguard(dir.clone());
    let status = std::process::Command::new("qlmanage")
        .arg("-t")
        .args(["-s", "1024"])
        .arg("-o")
        .arg(&dir)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("could not run qlmanage")?;
    if !status.success() {
        return Err(anyhow!("qlmanage could not render a frame from this file"));
    }
    // qlmanage names the output after the source, with .png appended.
    let produced = std::fs::read_dir(&dir)
        .context("poster frame dir")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .ok_or_else(|| anyhow!("qlmanage produced no frame"))?;
    let image = image::open(&produced).context("decoding the poster frame")?;
    drop(cleanup);
    Ok(image)
}

#[cfg(not(target_os = "macos"))]
pub fn poster_frame(_path: &Path) -> Result<DynamicImage> {
    Err(anyhow!(
        "Video thumbnails need macOS; this platform has no frame extractor"
    ))
}

/// Removes the temp directory when dropped, including on the error paths.
#[cfg(target_os = "macos")]
fn scopeguard(dir: std::path::PathBuf) -> impl Drop {
    struct Guard(std::path::PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    Guard(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output from `mdls` on a dive clip.
    const SAMPLE: &str = r#"kMDItemCodecs          = (
    "MPEG-4 AAC",
    "H.264",
    "Timed Metadata"
)
kMDItemDurationSeconds = 7.066666666666666
kMDItemPixelHeight     = 2160
kMDItemPixelWidth      = 3840"#;

    #[test]
    fn reads_duration_size_and_codecs() {
        let info = parse_mdls(SAMPLE);
        assert_eq!(info.width, Some(3840));
        assert_eq!(info.height, Some(2160));
        assert_eq!(info.duration_seconds, Some(7.066666666666666));
        assert_eq!(info.codecs, ["MPEG-4 AAC", "H.264", "Timed Metadata"]);
    }

    #[test]
    fn missing_fields_are_absent_rather_than_wrong() {
        // Spotlight returns (null) for files it has not indexed.
        let info = parse_mdls(
            "kMDItemDurationSeconds = (null)\nkMDItemPixelWidth      = (null)\nkMDItemPixelHeight     = (null)",
        );
        assert_eq!(info, VideoInfo::default());
    }

    #[test]
    fn empty_output_is_harmless() {
        assert_eq!(parse_mdls(""), VideoInfo::default());
    }
}
