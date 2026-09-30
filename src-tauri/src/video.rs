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
    /// Frames per second of the picture track, from the file's own sample
    /// table, so the player can step exactly one frame.
    pub frame_rate: Option<f64>,
    pub frame_count: Option<u64>,
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
    let mut info = parse_mdls(&String::from_utf8_lossy(&output.stdout));
    add_frame_timing(&mut info, path);
    Ok(info)
}

#[cfg(not(target_os = "macos"))]
pub fn video_info(path: &Path) -> Result<VideoInfo> {
    let mut info = VideoInfo::default();
    add_frame_timing(&mut info, path);
    Ok(info)
}

fn add_frame_timing(info: &mut VideoInfo, path: &Path) {
    match frame_timing(path) {
        Ok(timing) => {
            info.frame_rate = Some(timing.frame_rate);
            info.frame_count = Some(timing.frame_count);
        }
        Err(e) => log::debug!("no frame timing for {}: {e:#}", path.display()),
    }
}

/// The picture track's timing, as the container records it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameTiming {
    pub frame_rate: f64,
    pub frame_count: u64,
}

/// Read the frame rate from a MOV or MP4 file's sample table.
///
/// Spotlight does not record a frame rate, and the webview does not expose
/// one, so stepping a frame needs it from the file. Both containers are
/// ISO boxes: `moov` holds a `trak` per stream; the picture track is the
/// one whose `hdlr` says `vide`; its `mdhd` gives the time scale and its
/// `stts` the duration of every sample in that scale. Phones record at a
/// slightly variable rate, so the rate is taken from the most common
/// sample duration rather than an average.
pub fn frame_timing(path: &Path) -> Result<FrameTiming> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).context("opening the clip")?;
    let len = file.metadata()?.len();
    let mut at = 0u64;
    // Walk the top-level boxes by their headers; `moov` is often after
    // gigabytes of media data, which is never read.
    while at + 8 <= len {
        file.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 16];
        file.read_exact(&mut head[..8])?;
        let mut size = u32::from_be_bytes(head[..4].try_into().unwrap()) as u64;
        let mut header = 8u64;
        if size == 1 {
            file.read_exact(&mut head[8..16])?;
            size = u64::from_be_bytes(head[8..16].try_into().unwrap());
            header = 16;
        } else if size == 0 {
            size = len - at;
        }
        if size < header {
            return Err(anyhow!("malformed box at byte {at}"));
        }
        if &head[4..8] == b"moov" {
            let body = size - header;
            if body > 256 * 1024 * 1024 {
                return Err(anyhow!("movie header is implausibly large"));
            }
            let mut moov = vec![0u8; body as usize];
            file.read_exact(&mut moov)?;
            return timing_from_moov(&moov);
        }
        at += size;
    }
    Err(anyhow!("no movie header"))
}

/// Child boxes of `data` as (type, body).
fn boxes(mut data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    std::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let mut size = u32::from_be_bytes(data[..4].try_into().ok()?) as usize;
        let mut header = 8;
        if size == 1 {
            size = usize::try_from(u64::from_be_bytes(data.get(8..16)?.try_into().ok()?)).ok()?;
            header = 16;
        } else if size == 0 {
            size = data.len();
        }
        if size < header || size > data.len() {
            return None;
        }
        let kind = &data[4..8];
        let body = &data[header..size];
        data = &data[size..];
        Some((kind, body))
    })
}

fn child<'a>(data: &'a [u8], kind: &[u8]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| *k == kind).map(|(_, b)| b)
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn timing_from_moov(moov: &[u8]) -> Result<FrameTiming> {
    for (kind, trak) in boxes(moov) {
        if kind != b"trak" {
            continue;
        }
        let Some(mdia) = child(trak, b"mdia") else {
            continue;
        };
        // hdlr: version+flags, pre_defined, then the handler type.
        if child(mdia, b"hdlr").and_then(|h| h.get(8..12)) != Some(b"vide") {
            continue;
        }
        let mdhd = child(mdia, b"mdhd").ok_or_else(|| anyhow!("picture track has no mdhd"))?;
        let timescale = match mdhd.first() {
            Some(1) => be32(mdhd, 20),
            _ => be32(mdhd, 12),
        }
        .filter(|&t| t > 0)
        .ok_or_else(|| anyhow!("picture track has no time scale"))?;
        let stts = child(mdia, b"minf")
            .and_then(|m| child(m, b"stbl"))
            .and_then(|s| child(s, b"stts"))
            .ok_or_else(|| anyhow!("picture track has no sample durations"))?;
        let entries = be32(stts, 4).unwrap_or(0) as usize;
        let mut frame_count = 0u64;
        let mut by_delta: Vec<(u32, u64)> = Vec::new();
        for i in 0..entries {
            let (Some(count), Some(delta)) = (be32(stts, 8 + i * 8), be32(stts, 12 + i * 8)) else {
                break;
            };
            frame_count += count as u64;
            if delta == 0 {
                continue;
            }
            match by_delta.iter_mut().find(|(d, _)| *d == delta) {
                Some((_, n)) => *n += count as u64,
                None => by_delta.push((delta, count as u64)),
            }
        }
        let delta = by_delta
            .iter()
            .max_by_key(|(_, n)| *n)
            .map(|(d, _)| *d)
            .ok_or_else(|| anyhow!("picture track has no timed samples"))?;
        return Ok(FrameTiming {
            frame_rate: timescale as f64 / delta as f64,
            frame_count,
        });
    }
    Err(anyhow!("no picture track"))
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

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn track(handler: &[u8; 4], mdhd: Vec<u8>, stts: &[(u32, u32)]) -> Vec<u8> {
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0u8; 12]);
        let mut table = vec![0u8; 4];
        table.extend_from_slice(&(stts.len() as u32).to_be_bytes());
        for (count, delta) in stts {
            table.extend_from_slice(&count.to_be_bytes());
            table.extend_from_slice(&delta.to_be_bytes());
        }
        let stbl = boxed(b"stbl", &boxed(b"stts", &table));
        let minf = boxed(b"minf", &stbl);
        let mdia = [boxed(b"hdlr", &hdlr), boxed(b"mdhd", &mdhd), minf].concat();
        boxed(b"trak", &boxed(b"mdia", &mdia))
    }

    fn mdhd_v0(timescale: u32) -> Vec<u8> {
        let mut m = vec![0u8; 12];
        m.extend_from_slice(&timescale.to_be_bytes());
        m.extend_from_slice(&[0u8; 8]);
        m
    }

    #[test]
    fn frame_rate_comes_from_the_picture_track() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.MOV");
        // An audio track first (which must be skipped), then 29.97 fps
        // picture with one odd-length frame, after a large media box as
        // cameras write it.
        let audio = track(b"soun", mdhd_v0(48000), &[(300, 1024)]);
        let picture = track(
            b"vide",
            mdhd_v0(30000),
            &[(200, 1001), (1, 1500), (11, 1001)],
        );
        let moov = boxed(b"moov", &[audio, picture].concat());
        let file = [
            boxed(b"ftyp", b"qt  \0\0\0\0"),
            boxed(b"mdat", &vec![7u8; 100_000]),
            moov,
        ]
        .concat();
        std::fs::write(&clip, file).unwrap();
        let timing = frame_timing(&clip).unwrap();
        assert!((timing.frame_rate - 29.97).abs() < 0.001, "{timing:?}");
        assert_eq!(timing.frame_count, 212);
    }

    #[test]
    fn version_one_media_headers_are_read() {
        let mut mdhd = vec![1u8, 0, 0, 0];
        mdhd.extend_from_slice(&[0u8; 16]);
        mdhd.extend_from_slice(&600u32.to_be_bytes());
        mdhd.extend_from_slice(&[0u8; 12]);
        let moov = boxed(b"moov", &track(b"vide", mdhd, &[(120, 10)]));
        let timing = timing_from_moov(&moov[8..]).unwrap();
        assert_eq!(timing.frame_rate, 60.0);
        assert_eq!(timing.frame_count, 120);
    }

    #[test]
    fn files_without_a_movie_header_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.mp4");
        std::fs::write(&clip, boxed(b"mdat", &[0u8; 64])).unwrap();
        assert!(frame_timing(&clip).is_err());
        std::fs::write(&clip, b"not a video").unwrap();
        assert!(frame_timing(&clip).is_err());
    }
}
