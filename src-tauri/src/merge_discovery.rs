//! Auto-discovery of merge candidates.
//!
//! Scans a folder's photos and proposes two kinds of grouping:
//!   * bracket sets  -> "Merge to HDR"
//!   * sweep sequences -> "Stitch Panorama"
//!
//! Stage one is metadata only: capture times cut the sequence into bursts,
//! then each burst is classified by how its exposure settings vary. A real
//! AEB bracket varies exposure while aperture/focal/ISO stay put; a pano
//! sweep keeps exposure consistent across several frames. Nothing is ever
//! merged automatically — candidates only pre-fill the existing dialogs.

use crate::exif_processing::{read_exif, read_rrexif_sidecar};
use chrono::NaiveDateTime;
use rayon::prelude::*;
use serde::Serialize;
use std::io::Read;
use std::path::Path;

/// Frames further apart than this start a new burst.
const BURST_GAP_SECS: i64 = 4;
/// A bracket must span at least this many stops (guards against the small
/// shutter drift of ordinary continuous shooting in aperture priority).
const MIN_HDR_EV_SPREAD: f64 = 0.9;
/// A pano sweep must be more consistent than this, or it is a bracket.
const MAX_PANO_EV_SPREAD: f64 = 0.7;
const MIN_HDR_FRAMES: usize = 2;
const MAX_HDR_FRAMES: usize = 9;
const MIN_PANO_FRAMES: usize = 3;
const MAX_PANO_FRAMES: usize = 30;
/// EXIF lives near the front of JPEG and TIFF-based RAW files; reading a
/// prefix keeps a folder scan cheap instead of pulling whole RAW files.
const EXIF_PREFIX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct FrameMeta {
    pub path: String,
    pub timestamp: Option<i64>,
    pub exposure_secs: Option<f64>,
    pub aperture: Option<f64>,
    pub focal: Option<f64>,
    pub iso: Option<f64>,
    pub exposure_bias: Option<f64>,
}

impl FrameMeta {
    /// Exposure in stops (brighter = larger). Bracket detection compares
    /// these, so it works whether the camera varied shutter or logged only
    /// an exposure-compensation value.
    fn ev(&self) -> Option<f64> {
        match (self.exposure_secs, self.exposure_bias) {
            (Some(t), _) if t > 0.0 => Some(t.log2()),
            (_, Some(b)) => Some(b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeCandidate {
    /// "hdr" | "panorama"
    pub kind: String,
    pub paths: Vec<String>,
    pub frame_count: usize,
    /// "high" | "medium"
    pub confidence: String,
    pub evidence: String,
    pub time_span_secs: i64,
}

fn rational_f64(field: &exif::Field) -> Option<f64> {
    match field.value {
        exif::Value::Rational(ref r) => {
            let v = r.first()?;
            if v.denom == 0 {
                None
            } else {
                Some(v.num as f64 / v.denom as f64)
            }
        }
        exif::Value::SRational(ref r) => {
            let v = r.first()?;
            if v.denom == 0 {
                None
            } else {
                Some(v.num as f64 / v.denom as f64)
            }
        }
        _ => field.display_value().to_string().trim().parse::<f64>().ok(),
    }
}

fn parse_exif_datetime(s: &str) -> Option<i64> {
    let cleaned = s.trim().trim_matches('"');
    for format in ["%Y:%m:%d %H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(cleaned, format) {
            return Some(dt.and_utc().timestamp());
        }
    }
    None
}

/// Reads just enough of a file to recover its capture settings. Falls back
/// to the cached sidecar values for anything the prefix did not carry.
pub fn read_frame_meta(path: &str) -> FrameMeta {
    let mut meta = FrameMeta {
        path: path.to_string(),
        ..Default::default()
    };

    let mut prefix = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(EXIF_PREFIX_BYTES).read_to_end(&mut prefix);
    }

    if let Some(exif) = read_exif(&prefix) {
        use exif::{In, Tag};
        if let Some(f) = exif
            .get_field(Tag::DateTimeOriginal, In::PRIMARY)
            .or_else(|| exif.get_field(Tag::DateTimeDigitized, In::PRIMARY))
            .or_else(|| exif.get_field(Tag::DateTime, In::PRIMARY))
        {
            meta.timestamp = parse_exif_datetime(&f.display_value().to_string());
        }
        if let Some(f) = exif.get_field(Tag::ExposureTime, In::PRIMARY) {
            meta.exposure_secs = rational_f64(f);
        }
        if let Some(f) = exif.get_field(Tag::FNumber, In::PRIMARY) {
            meta.aperture = rational_f64(f);
        }
        if let Some(f) = exif.get_field(Tag::FocalLength, In::PRIMARY) {
            meta.focal = rational_f64(f);
        }
        if let Some(f) = exif
            .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
            .or_else(|| exif.get_field(Tag::ISOSpeed, In::PRIMARY))
        {
            meta.iso = f.display_value().to_string().trim().parse::<f64>().ok();
        }
        if let Some(f) = exif.get_field(Tag::ExposureBiasValue, In::PRIMARY) {
            meta.exposure_bias = rational_f64(f);
        }
    }

    // Cached sidecar covers files whose EXIF sat past the prefix.
    if meta.exposure_secs.is_none() || meta.aperture.is_none() {
        if let Some(map) = read_rrexif_sidecar(Path::new(path)) {
            if meta.exposure_secs.is_none()
                && let Some(raw) = map.get("ExposureTime")
            {
                let cleaned = raw.replace(" s", "");
                meta.exposure_secs = if let Some((n, d)) = cleaned.split_once('/') {
                    match (n.trim().parse::<f64>(), d.trim().parse::<f64>()) {
                        (Ok(n), Ok(d)) if d != 0.0 => Some(n / d),
                        _ => None,
                    }
                } else {
                    cleaned.trim().parse::<f64>().ok()
                };
            }
            if meta.aperture.is_none()
                && let Some(raw) = map.get("FNumber")
            {
                meta.aperture = raw.trim_start_matches("f/").trim().parse::<f64>().ok();
            }
            if meta.focal.is_none()
                && let Some(raw) = map.get("FocalLength")
            {
                meta.focal = raw
                    .split(|c: char| !c.is_ascii_digit() && c != '.')
                    .find(|t| !t.is_empty())
                    .and_then(|t| t.parse::<f64>().ok());
            }
            if meta.iso.is_none()
                && let Some(raw) = map.get("ISOSpeed")
            {
                meta.iso = raw.trim().parse::<f64>().ok();
            }
        }
    }

    if meta.timestamp.is_none()
        && let Ok(fs_meta) = std::fs::metadata(path)
        && let Ok(modified) = fs_meta.modified()
        && let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        // File mtime is a fallback ordering signal: for files straight off
        // a card it tracks capture time closely enough to cluster bursts.
        meta.timestamp = Some(dur.as_secs() as i64);
    }

    meta
}

fn spread(values: &[f64]) -> f64 {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in values {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    if lo.is_finite() && hi.is_finite() {
        hi - lo
    } else {
        0.0
    }
}

fn roughly_constant(values: &[Option<f64>], tolerance: f64) -> bool {
    let present: Vec<f64> = values.iter().filter_map(|v| *v).collect();
    if present.len() < 2 {
        return true;
    }
    spread(&present) <= tolerance
}

/// Splits time-ordered frames into bursts on capture-time gaps.
pub fn cluster_bursts(frames: &[FrameMeta]) -> Vec<Vec<FrameMeta>> {
    let mut ordered: Vec<FrameMeta> = frames.to_vec();
    ordered.sort_by_key(|f| f.timestamp.unwrap_or(i64::MAX));

    let mut bursts: Vec<Vec<FrameMeta>> = Vec::new();
    for frame in ordered {
        let start_new = match (bursts.last().and_then(|b| b.last()), frame.timestamp) {
            (Some(prev), Some(ts)) => match prev.timestamp {
                Some(prev_ts) => ts - prev_ts > BURST_GAP_SECS,
                None => true,
            },
            (None, _) => true,
            (Some(_), None) => true,
        };
        if start_new {
            bursts.push(vec![frame]);
        } else if let Some(last) = bursts.last_mut() {
            last.push(frame);
        }
    }
    bursts
}

/// Classifies one burst. Bracket detection is strict (constant aperture,
/// focal and ISO with a real EV spread); pano detection requires the
/// opposite — several consistently exposed frames at one focal length.
pub fn classify_burst(burst: &[FrameMeta]) -> Option<MergeCandidate> {
    if burst.len() < MIN_HDR_FRAMES {
        return None;
    }
    let evs: Vec<f64> = burst.iter().filter_map(|f| f.ev()).collect();
    if evs.len() < burst.len() {
        return None;
    }
    let ev_spread = spread(&evs);
    let apertures: Vec<Option<f64>> = burst.iter().map(|f| f.aperture).collect();
    let focals: Vec<Option<f64>> = burst.iter().map(|f| f.focal).collect();
    let isos: Vec<Option<f64>> = burst.iter().map(|f| f.iso).collect();
    let time_span = match (
        burst.first().and_then(|f| f.timestamp),
        burst.last().and_then(|f| f.timestamp),
    ) {
        (Some(a), Some(b)) => b - a,
        _ => 0,
    };
    let paths: Vec<String> = burst.iter().map(|f| f.path.clone()).collect();

    let settings_locked = roughly_constant(&apertures, 0.15)
        && roughly_constant(&focals, 0.5)
        && roughly_constant(&isos, 1.0);

    if settings_locked && ev_spread >= MIN_HDR_EV_SPREAD && burst.len() <= MAX_HDR_FRAMES {
        // Distinct, evenly separated EV steps are the AEB signature.
        let mut sorted = evs.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let steps: Vec<f64> = sorted.windows(2).map(|w| w[1] - w[0]).collect();
        let even = steps.iter().all(|s| *s > 0.2) && spread(&steps) < 0.5;
        return Some(MergeCandidate {
            kind: "hdr".to_string(),
            frame_count: burst.len(),
            paths,
            confidence: if even { "high" } else { "medium" }.to_string(),
            evidence: format!(
                "{} frames over {}s, {:.1} stop spread, aperture/ISO locked",
                burst.len(),
                time_span,
                ev_spread
            ),
            time_span_secs: time_span,
        });
    }

    if burst.len() >= MIN_PANO_FRAMES
        && burst.len() <= MAX_PANO_FRAMES
        && ev_spread <= MAX_PANO_EV_SPREAD
        && roughly_constant(&focals, 0.5)
    {
        return Some(MergeCandidate {
            kind: "panorama".to_string(),
            frame_count: burst.len(),
            paths,
            // EXIF alone cannot prove the camera panned; content overlap
            // verification is the planned second stage.
            confidence: "medium".to_string(),
            evidence: format!(
                "{} consistently exposed frames over {}s at one focal length",
                burst.len(),
                time_span
            ),
            time_span_secs: time_span,
        });
    }

    None
}

#[tauri::command]
pub async fn discover_merge_candidates(paths: Vec<String>) -> Result<Vec<MergeCandidate>, String> {
    tokio::task::spawn_blocking(move || {
        let frames: Vec<FrameMeta> = paths.par_iter().map(|p| read_frame_meta(p)).collect();
        let bursts = cluster_bursts(&frames);
        let candidates: Vec<MergeCandidate> =
            bursts.iter().filter_map(|b| classify_burst(b)).collect();
        log::info!(
            "[discover] scanned {} photo(s) -> {} burst(s) -> {} candidate(s) ({} hdr, {} panorama)",
            frames.len(),
            bursts.len(),
            candidates.len(),
            candidates.iter().filter(|c| c.kind == "hdr").count(),
            candidates.iter().filter(|c| c.kind == "panorama").count(),
        );
        candidates
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(path: &str, ts: i64, exposure: f64, aperture: f64, focal: f64, iso: f64) -> FrameMeta {
        FrameMeta {
            path: path.to_string(),
            timestamp: Some(ts),
            exposure_secs: Some(exposure),
            aperture: Some(aperture),
            focal: Some(focal),
            iso: Some(iso),
            exposure_bias: None,
        }
    }

    /// A real AEB bracket: shutter varies by 2 stops per step, everything
    /// else locked.
    #[test]
    fn detects_exposure_bracket() {
        let burst = vec![
            frame("a.arw", 100, 1.0 / 500.0, 8.0, 24.0, 100.0),
            frame("b.arw", 101, 1.0 / 125.0, 8.0, 24.0, 100.0),
            frame("c.arw", 102, 1.0 / 30.0, 8.0, 24.0, 100.0),
        ];
        let c = classify_burst(&burst).expect("bracket not detected");
        assert_eq!(c.kind, "hdr");
        assert_eq!(c.frame_count, 3);
        assert_eq!(c.confidence, "high", "even 2-stop steps should read as high confidence");
    }

    /// Continuous shooting in aperture priority: shutter drifts slightly.
    /// Must NOT be offered as a bracket.
    #[test]
    fn rejects_ordinary_burst_as_bracket() {
        let burst = vec![
            frame("a.arw", 100, 1.0 / 500.0, 4.0, 50.0, 200.0),
            frame("b.arw", 101, 1.0 / 520.0, 4.0, 50.0, 200.0),
            frame("c.arw", 102, 1.0 / 480.0, 4.0, 50.0, 200.0),
        ];
        let c = classify_burst(&burst);
        assert!(
            c.as_ref().map(|c| c.kind.as_str()) != Some("hdr"),
            "shutter noise must not read as a bracket: {c:?}"
        );
    }

    /// A pano sweep: several consistently exposed frames, same focal.
    #[test]
    fn detects_pano_sweep() {
        let burst: Vec<FrameMeta> = (0..6)
            .map(|i| frame(&format!("p{i}.arw"), 200 + i as i64, 1.0 / 250.0, 8.0, 35.0, 100.0))
            .collect();
        let c = classify_burst(&burst).expect("sweep not detected");
        assert_eq!(c.kind, "panorama");
        assert_eq!(c.frame_count, 6);
    }

    /// Two frames minutes apart are unrelated — no candidate at all.
    #[test]
    fn ignores_unrelated_frames() {
        let frames = vec![
            frame("a.arw", 100, 1.0 / 250.0, 8.0, 35.0, 100.0),
            frame("b.arw", 900, 1.0 / 250.0, 8.0, 35.0, 100.0),
        ];
        let bursts = cluster_bursts(&frames);
        assert_eq!(bursts.len(), 2, "gap must split the burst");
        assert!(bursts.iter().all(|b| classify_burst(b).is_none()));
    }

    /// A bracket and a sweep in the same folder are reported separately.
    #[test]
    fn separates_bracket_and_sweep_in_one_folder() {
        let mut frames = vec![
            frame("h0.arw", 100, 1.0 / 500.0, 8.0, 24.0, 100.0),
            frame("h1.arw", 101, 1.0 / 125.0, 8.0, 24.0, 100.0),
            frame("h2.arw", 102, 1.0 / 30.0, 8.0, 24.0, 100.0),
        ];
        frames.extend((0..5).map(|i| {
            frame(&format!("p{i}.arw"), 300 + i as i64, 1.0 / 250.0, 8.0, 35.0, 100.0)
        }));
        let candidates: Vec<MergeCandidate> = cluster_bursts(&frames)
            .iter()
            .filter_map(|b| classify_burst(b))
            .collect();
        assert_eq!(candidates.len(), 2, "expected one bracket + one sweep: {candidates:?}");
        assert!(candidates.iter().any(|c| c.kind == "hdr"));
        assert!(candidates.iter().any(|c| c.kind == "panorama"));
    }
}
