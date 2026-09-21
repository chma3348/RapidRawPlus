//! Auto level: estimate the fine-rotation angle that makes the dominant
//! near-horizontal (or, failing that, near-vertical) structure in a photo
//! level.
//!
//! The estimator is deliberately model-free and cheap (a few tens of ms at
//! 1024 px). It builds a structure tensor over a blurred luma image, keeps
//! only coherent (line-like) pixels, folds their orientation into a tilt
//! from the nearest axis, and reads the tilt off a weighted 0.1° histogram.
//! Horizontal structure (horizons, sills, water lines) is preferred over
//! vertical structure because a "level" tool is expected to fix horizons
//! first; verticals are only consulted when nothing horizontal is
//! confident.
//!
//! The returned angle is in the app's `rotation` convention (positive =
//! clockwise, the convention `apply_rotation` uses), so it can be written
//! straight into the adjustments.

use image::{DynamicImage, GenericImageView, GrayImage, imageops::FilterType};
use serde::Serialize;

/// Longest side the analysis runs at. Angle precision scales with line
/// length in pixels, so 1024 px gives sub-0.1° on a full-frame horizon
/// while keeping the whole thing well under 100 ms.
const ANALYSIS_SIDE: u32 = 1024;
/// Largest tilt the tool will correct. Beyond this the "nearest axis" fold
/// becomes ambiguous and a photographer almost never wants auto-correction.
pub const MAX_TILT_DEG: f32 = 15.0;
const BIN_DEG: f32 = 0.1;
const BINS: usize = (90.0 / BIN_DEG) as usize;
/// Minimum structure-tensor coherence for a pixel to count as "on a line".
const MIN_COHERENCE: f32 = 0.6;
/// Share of all coherent-edge mass that must sit within ±0.75° of the peak
/// for the estimate to be trusted. A flat distribution over 90° puts about
/// 1.7% in that window; a real horizon or a wall of windows puts 10–60%.
pub const MIN_CONFIDENCE: f32 = 0.05;
/// Horizontal structure wins ties against vertical structure by this factor.
const HORIZONTAL_PREFERENCE: f32 = 1.5;
/// Gaussian prior on the tilt used when choosing between competing peaks.
/// Accidental tilts are almost always a few degrees, while a strong line at
/// 10–15° is far more often a bridge, a staircase or a roof than a tilted
/// camera. A peak at 12° needs roughly 18× the mass of one at 0° to win.
const TILT_PRIOR_SIGMA_DEG: f32 = 5.0;
/// A second peak this close to the mirror image (−tilt) of the winner marks
/// converging lines (perspective), whose true roll is the axis of symmetry.
const MIRROR_TOLERANCE_DEG: f32 = 1.25;
/// The mirror peak must carry at least this share of the winner's mass.
const MIRROR_MIN_RATIO: f32 = 0.5;
/// Below this tilt the winner and its mirror are too close to tell apart
/// from one broad peak, so the symmetry rule stays out of it.
const MIRROR_MIN_TILT_DEG: f32 = 2.5;
/// Corrections larger than this need the other axis to agree. A rolled
/// camera tilts horizontals and verticals alike; a single family of strong
/// diagonals (roof ribs, a staircase, a bridge deck) does not, and is the
/// classic way to "level" a photo into a Dutch angle.
const CORROBORATION_ABOVE_DEG: f32 = 5.0;
const CORROBORATION_TOLERANCE_DEG: f32 = 1.0;
/// The corroborating peak must carry at least this share of the winner.
const CORROBORATION_MIN_RATIO: f32 = 0.2;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct LevelEstimate {
    /// Rotation to apply (degrees, clockwise positive).
    pub angle: f32,
    /// Share of coherent-edge mass in the winning peak (0..1).
    pub confidence: f32,
    /// Which structure the estimate came from.
    pub reference: LevelReference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LevelReference {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy)]
struct Peak {
    tilt: f32,
    mass: f32,
}

/// Estimate the tilt of `image`. Returns `None` when there is no confident
/// horizontal or vertical structure to level against.
pub fn estimate_level(image: &DynamicImage) -> Option<LevelEstimate> {
    let (w, h) = image.dimensions();
    if w < 16 || h < 16 {
        return None;
    }
    let scale = ANALYSIS_SIDE as f32 / w.max(h) as f32;
    let small = if scale < 1.0 {
        let nw = ((w as f32 * scale).round() as u32).max(16);
        let nh = ((h as f32 * scale).round() as u32).max(16);
        image.resize_exact(nw, nh, FilterType::Triangle)
    } else {
        image.clone()
    };
    let (luma, valid) = luma_and_validity(&small);
    let analysis = analyse(
        &luma,
        &valid,
        small.width() as usize,
        small.height() as usize,
    )?;
    decide(&analysis)
}

/// A candidate peak, for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct PeakReport {
    pub tilt: f32,
    pub mass_share: f32,
    pub horizontal: bool,
}

/// Same as [`estimate_level`], plus the strongest local maxima of both
/// histograms (within ±MAX_TILT_DEG) so a probe can show what competed.
pub fn estimate_level_verbose(image: &DynamicImage) -> (Option<LevelEstimate>, Vec<PeakReport>) {
    let (w, h) = image.dimensions();
    if w < 16 || h < 16 {
        return (None, Vec::new());
    }
    let scale = ANALYSIS_SIDE as f32 / w.max(h) as f32;
    let small = if scale < 1.0 {
        let nw = ((w as f32 * scale).round() as u32).max(16);
        let nh = ((h as f32 * scale).round() as u32).max(16);
        image.resize_exact(nw, nh, FilterType::Triangle)
    } else {
        image.clone()
    };
    let (luma, valid) = luma_and_validity(&small);
    let Some(analysis) = analyse(
        &luma,
        &valid,
        small.width() as usize,
        small.height() as usize,
    ) else {
        return (None, Vec::new());
    };
    let mut peaks = Vec::new();
    for (hist, horizontal) in [(&analysis.hist_h, true), (&analysis.hist_v, false)] {
        let smoothed = smooth(hist);
        let lo = ((45.0 - MAX_TILT_DEG) / BIN_DEG) as usize;
        let hi = ((45.0 + MAX_TILT_DEG) / BIN_DEG) as usize;
        for b in lo..=hi {
            let v = smoothed[b];
            if v > 0.0
                && (b == 0 || smoothed[b - 1] < v)
                && (b + 1 >= BINS || smoothed[b + 1] <= v)
                && let Some(p) = refine_peak(hist, b)
            {
                peaks.push(PeakReport {
                    tilt: p.tilt,
                    mass_share: p.mass / analysis.total_mass,
                    horizontal,
                });
            }
        }
    }
    peaks.sort_by(|a, b| b.mass_share.total_cmp(&a.mass_share));
    peaks.truncate(8);
    (decide(&analysis), peaks)
}

struct Analysis {
    hist_h: Vec<f32>,
    hist_v: Vec<f32>,
    total_mass: f32,
}

/// Convenience for callers that already hold an 8-bit grey image.
pub fn estimate_level_gray(gray: &GrayImage) -> Option<LevelEstimate> {
    estimate_level(&DynamicImage::ImageLuma8(gray.clone()))
}

/// Luma in 0..1 plus a validity mask that excludes transparent pixels
/// (perspective-warped borders, PNG alpha) so their synthetic edges cannot
/// vote.
fn luma_and_validity(image: &DynamicImage) -> (Vec<f32>, Vec<bool>) {
    let (w, h) = image.dimensions();
    let n = (w * h) as usize;
    let mut luma = vec![0.0f32; n];
    let mut valid = vec![true; n];
    let rgba = image.to_rgba32f();
    for (i, p) in rgba.pixels().enumerate() {
        let [r, g, b, a] = p.0;
        luma[i] = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0);
        valid[i] = a > 0.5;
    }
    // Dilate the invalid region by 3 px so the edge of a transparent
    // border, which the blur and tensor windows straddle, is dropped too.
    if valid.iter().any(|v| !v) {
        let src = valid.clone();
        let (wi, hi) = (w as i64, h as i64);
        for y in 0..hi {
            for x in 0..wi {
                if src[(y * wi + x) as usize] {
                    continue;
                }
                for dy in -3..=3 {
                    for dx in -3..=3 {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx >= 0 && ny >= 0 && nx < wi && ny < hi {
                            valid[(ny * wi + nx) as usize] = false;
                        }
                    }
                }
            }
        }
    }
    (luma, valid)
}

fn analyse(luma: &[f32], valid: &[bool], w: usize, h: usize) -> Option<Analysis> {
    let blurred = gaussian_blur(luma, w, h, 1.2);
    let (gx, gy) = scharr(&blurred, w, h);

    // Structure tensor, box-averaged over 5×5. Averaging before taking the
    // orientation is what makes the angle continuous (and far less biased
    // toward the pixel axes than a per-pixel atan2 would be).
    let n = w * h;
    let mut jxx = vec![0.0f32; n];
    let mut jyy = vec![0.0f32; n];
    let mut jxy = vec![0.0f32; n];
    for i in 0..n {
        jxx[i] = gx[i] * gx[i];
        jyy[i] = gy[i] * gy[i];
        jxy[i] = gx[i] * gy[i];
    }
    let jxx = box_blur(&jxx, w, h, 2);
    let jyy = box_blur(&jyy, w, h, 2);
    let jxy = box_blur(&jxy, w, h, 2);

    // Adaptive magnitude threshold: only the strongest ~15% of pixels are
    // edges worth listening to, with an absolute floor for flat images.
    let mut mags: Vec<f32> = (0..n)
        .filter(|&i| valid[i])
        .map(|i| (jxx[i] + jyy[i]).sqrt())
        .collect();
    if mags.len() < 256 {
        return None;
    }
    let cut = (mags.len() as f32 * 0.85) as usize;
    let (_, thresh, _) = mags.select_nth_unstable_by(cut, |a, b| a.total_cmp(b));
    let mag_thresh = thresh.max(0.01);

    let mut hist_h = vec![0.0f32; BINS];
    let mut hist_v = vec![0.0f32; BINS];
    let mut total_mass = 0.0f32;
    let mut contributing = 0usize;
    let margin = 2usize;
    for y in margin..h.saturating_sub(margin) {
        for x in margin..w.saturating_sub(margin) {
            let i = y * w + x;
            if !valid[i] {
                continue;
            }
            let trace = jxx[i] + jyy[i];
            let mag = trace.sqrt();
            if mag < mag_thresh || trace <= 0.0 {
                continue;
            }
            let diff = jxx[i] - jyy[i];
            let coherence = (diff * diff + 4.0 * jxy[i] * jxy[i]).sqrt() / trace;
            if coherence < MIN_COHERENCE {
                continue;
            }
            // Dominant gradient direction, in (-90, 90].
            let grad_deg = (0.5 * (2.0 * jxy[i]).atan2(diff)).to_degrees();
            // Line direction is the gradient rotated by 90°. Fold it into a
            // tilt from the nearest axis in (-45, 45], and remember which
            // axis: a line direction near 0 is horizontal, near ±90 vertical.
            let line_deg = grad_deg + 90.0;
            let folded = fold_90(line_deg);
            let r180 = line_deg.rem_euclid(180.0);
            let is_horizontal = r180 <= 45.0 || r180 > 135.0;
            let weight = mag * coherence * coherence;
            total_mass += weight;
            contributing += 1;
            let bin = (((folded + 45.0) / BIN_DEG) as usize).min(BINS - 1);
            if is_horizontal {
                hist_h[bin] += weight;
            } else {
                hist_v[bin] += weight;
            }
        }
    }
    if contributing < 200 || total_mass <= 0.0 {
        return None;
    }
    Some(Analysis {
        hist_h,
        hist_v,
        total_mass,
    })
}

fn decide(analysis: &Analysis) -> Option<LevelEstimate> {
    let total_mass = analysis.total_mass;
    let peak_h = find_peak(&analysis.hist_h);
    let peak_v = find_peak(&analysis.hist_v);

    let pick = match (peak_h, peak_v) {
        (Some(ph), Some(pv)) => {
            if ph.mass * HORIZONTAL_PREFERENCE >= pv.mass {
                Some((ph, LevelReference::Horizontal))
            } else {
                Some((pv, LevelReference::Vertical))
            }
        }
        (Some(ph), None) => Some((ph, LevelReference::Horizontal)),
        (None, Some(pv)) => Some((pv, LevelReference::Vertical)),
        (None, None) => None,
    }?;

    let (peak, reference) = pick;
    let confidence = peak.mass / total_mass;
    if confidence < MIN_CONFIDENCE {
        return None;
    }
    if peak.tilt.abs() > CORROBORATION_ABOVE_DEG {
        let other = match reference {
            LevelReference::Horizontal => &analysis.hist_v,
            LevelReference::Vertical => &analysis.hist_h,
        };
        if !corroborates(other, peak) {
            return None;
        }
    }
    // A line tilted by +t (image coords, y down) slopes down to the right,
    // i.e. it is rotated clockwise on screen; levelling it means rotating
    // the image counter-clockwise, which is -t in the clockwise-positive
    // rotation convention.
    Some(LevelEstimate {
        angle: -peak.tilt,
        confidence,
        reference,
    })
}

/// Fold an angle in degrees to the nearest multiple of 90°, returning the
/// signed offset in (-45, 45].
fn fold_90(deg: f32) -> f32 {
    let r = deg.rem_euclid(90.0);
    if r > 45.0 { r - 90.0 } else { r }
}

/// Find the strongest peak within ±MAX_TILT_DEG of the axis. Returns the
/// sub-bin refined tilt and the raw mass within ±0.75° of it.
/// Smooth with a small Gaussian (σ = 2 bins) so a peak spread over a few
/// bins by noise still wins over a single spiky bin elsewhere.
fn smooth(hist: &[f32]) -> Vec<f32> {
    let kernel: Vec<f32> = (-4..=4)
        .map(|k| (-(k * k) as f32 / (2.0 * 2.0 * 2.0)).exp())
        .collect();
    let ksum: f32 = kernel.iter().sum();
    (0..BINS)
        .map(|b| {
            let mut acc = 0.0;
            for (k, kw) in kernel.iter().enumerate() {
                let idx = b as i64 + k as i64 - 4;
                if idx >= 0 && (idx as usize) < BINS {
                    acc += hist[idx as usize] * kw;
                }
            }
            acc / ksum
        })
        .collect()
}

fn find_peak(hist: &[f32]) -> Option<Peak> {
    let smoothed = smooth(hist);
    let lo = ((45.0 - MAX_TILT_DEG) / BIN_DEG) as usize;
    let hi = ((45.0 + MAX_TILT_DEG) / BIN_DEG) as usize;
    let prior = |bin: usize| {
        let t = bin_centre(bin);
        (-(t * t) / (2.0 * TILT_PRIOR_SIGMA_DEG * TILT_PRIOR_SIGMA_DEG)).exp()
    };
    let (best_bin, best_val) = (lo..=hi)
        .map(|b| (b, smoothed[b] * prior(b)))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    if best_val <= 0.0 {
        return None;
    }
    // A winner sitting on the window edge is usually the tail of a stronger
    // structure just outside it; refuse rather than clip it to ±MAX_TILT.
    let edge = (0.5 / BIN_DEG) as usize;
    if best_bin < lo + edge || best_bin > hi - edge {
        return None;
    }

    let winner = refine_peak(hist, best_bin)?;

    // Converging lines: if a comparable peak sits at the mirror angle, the
    // camera roll is the axis of symmetry between the two, not either one.
    if winner.tilt.abs() >= MIRROR_MIN_TILT_DEG {
        let mirror_centre = ((-winner.tilt + 45.0) / BIN_DEG) as usize;
        let span = (MIRROR_TOLERANCE_DEG / BIN_DEG) as usize;
        let ma = mirror_centre.saturating_sub(span).max(lo + 1);
        let mb = (mirror_centre + span).min(hi - 1);
        // The mirror has to be a real local maximum, not merely the highest
        // bin of a flat stretch next to the winner's own tail.
        let is_local_max =
            |b: usize| smoothed[b] > smoothed[b - 1] && smoothed[b] >= smoothed[b + 1];
        if ma <= mb
            && let Some((mbin, _)) = (ma..=mb)
                .filter(|&b| is_local_max(b))
                .map(|b| (b, smoothed[b]))
                .max_by(|a, b| a.1.total_cmp(&b.1))
            && let Some(mirror) = refine_peak(hist, mbin)
            && mirror.mass >= MIRROR_MIN_RATIO * winner.mass
        {
            return Some(Peak {
                tilt: 0.5 * (winner.tilt + mirror.tilt),
                mass: winner.mass + mirror.mass,
            });
        }
    }
    Some(winner)
}

/// Does `hist` (the other axis) hold a peak within tolerance of `peak`'s
/// tilt carrying a meaningful share of its mass?
fn corroborates(hist: &[f32], peak: Peak) -> bool {
    let centre = ((peak.tilt + 45.0) / BIN_DEG) as usize;
    let span = (CORROBORATION_TOLERANCE_DEG / BIN_DEG) as usize;
    let a = centre.saturating_sub(span);
    let b = (centre + span).min(BINS - 1);
    let smoothed = smooth(hist);
    (a..=b)
        .filter(|&i| {
            i > 0
                && i + 1 < BINS
                && smoothed[i] >= smoothed[i - 1]
                && smoothed[i] >= smoothed[i + 1]
        })
        .filter_map(|i| refine_peak(hist, i))
        .any(|p| {
            (p.tilt - peak.tilt).abs() <= CORROBORATION_TOLERANCE_DEG
                && p.mass >= CORROBORATION_MIN_RATIO * peak.mass
        })
}

fn bin_centre(bin: usize) -> f32 {
    (bin as f32 + 0.5) * BIN_DEG - 45.0
}

/// Sub-bin refinement (weighted centroid of the raw histogram within ±0.5°
/// of `bin`) and the raw mass within ±0.75°.
fn refine_peak(hist: &[f32], bin: usize) -> Option<Peak> {
    let half = (0.5 / BIN_DEG) as usize;
    let a = bin.saturating_sub(half);
    let b = (bin + half).min(BINS - 1);
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    for (i, &v) in hist.iter().enumerate().take(b + 1).skip(a) {
        num += v * bin_centre(i);
        den += v;
    }
    if den <= 0.0 {
        return None;
    }
    let win = (0.75 / BIN_DEG) as usize;
    let a = bin.saturating_sub(win);
    let b = (bin + win).min(BINS - 1);
    Some(Peak {
        tilt: num / den,
        mass: hist[a..=b].iter().sum(),
    })
}

fn gaussian_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as i64;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|k| (-(k * k) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let ksum: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.into_iter().map(|k| k / ksum).collect();

    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, kw) in kernel.iter().enumerate() {
                let sx = (x as i64 + k as i64 - radius).clamp(0, w as i64 - 1) as usize;
                acc += src[y * w + sx] * kw;
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (k, kw) in kernel.iter().enumerate() {
                let sy = (y as i64 + k as i64 - radius).clamp(0, h as i64 - 1) as usize;
                acc += tmp[sy * w + x] * kw;
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// Scharr 3×3 gradients (better rotational symmetry than Sobel, which
/// matters when the whole point is reading an angle off the gradient).
fn scharr(src: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut gx = vec![0.0f32; w * h];
    let mut gy = vec![0.0f32; w * h];
    if w < 3 || h < 3 {
        return (gx, gy);
    }
    let at = |x: usize, y: usize| src[y * w + x];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let tl = at(x - 1, y - 1);
            let tc = at(x, y - 1);
            let tr = at(x + 1, y - 1);
            let ml = at(x - 1, y);
            let mr = at(x + 1, y);
            let bl = at(x - 1, y + 1);
            let bc = at(x, y + 1);
            let br = at(x + 1, y + 1);
            gx[y * w + x] = (3.0 * (tr - tl) + 10.0 * (mr - ml) + 3.0 * (br - bl)) / 32.0;
            gy[y * w + x] = (3.0 * (bl - tl) + 10.0 * (bc - tc) + 3.0 * (br - tr)) / 32.0;
        }
    }
    (gx, gy)
}

fn box_blur(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * h];
    let norm = 1.0 / (2 * radius + 1) as f32;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let mut acc: f32 =
            (0..=radius.min(w - 1)).map(|x| row[x]).sum::<f32>() + row[0] * radius as f32;
        for x in 0..w {
            tmp[y * w + x] = acc * norm;
            let add = row[(x + radius + 1).min(w - 1)];
            let sub = row[x.saturating_sub(radius)];
            acc += add - sub;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for x in 0..w {
        let mut acc: f32 =
            (0..=radius.min(h - 1)).map(|y| tmp[y * w + x]).sum::<f32>() + tmp[x] * radius as f32;
        for y in 0..h {
            out[y * w + x] = acc * norm;
            let add = tmp[(y + radius + 1).min(h - 1) * w + x];
            let sub = tmp[y.saturating_sub(radius) * w + x];
            acc += add - sub;
        }
    }
    out
}

/// Frontend entry point. Runs on the geometry-warped image with the current
/// 90° orientation and flips applied, and with fine rotation at zero, so the
/// returned angle replaces (rather than adds to) any existing rotation.
#[tauri::command]
pub async fn auto_level(
    js_adjustments: serde_json::Value,
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<Option<LevelEstimate>, String> {
    let warped = crate::get_cached_full_warped_image(&state, &js_adjustments)?;
    let result = tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        let oriented =
            crate::image_processing::apply_coarse_rotation(warped.as_ref(), orientation_steps);
        let oriented =
            crate::image_processing::apply_flip(oriented, flip_horizontal, flip_vertical);
        let result = estimate_level(oriented.as_ref());
        log::info!("auto_level: {:?} in {:.1?}", result, start.elapsed());
        result
    })
    .await
    .map_err(|e| format!("auto_level task failed: {e}"))?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_processing::apply_rotation;
    use image::{Rgba, RgbaImage};

    /// A "beach": bright sky over dark sea with a soft horizon, plus a few
    /// horizontal bands so there is more than one line to vote.
    fn horizon_scene(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let fy = y as f32 / h as f32;
                let mut v = if fy < 0.45 { 0.85 } else { 0.25 };
                if (0.60..0.62).contains(&fy) || (0.80..0.815).contains(&fy) {
                    v = 0.55;
                }
                // Gentle texture so the tensor threshold has something to cut.
                let noise = (((x * 7 + y * 13) % 17) as f32 / 17.0 - 0.5) * 0.04;
                let px = ((v + noise).clamp(0.0, 1.0) * 255.0) as u8;
                img.put_pixel(x, y, Rgba([px, px, px, 255]));
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    /// A "building": horizontal floors and vertical mullions, the kind of
    /// scene where both axes agree on the camera roll.
    fn building_scene(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let floor = (y / 60) % 2 == 0;
                let mullion = (x / 45) % 2 == 0;
                let v = match (floor, mullion) {
                    (true, true) => 0.85,
                    (true, false) => 0.55,
                    (false, true) => 0.40,
                    (false, false) => 0.20,
                };
                let px = (v * 255.0) as u8;
                img.put_pixel(x, y, Rgba([px, px, px, 255]));
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    /// A "facade": vertical stripes only, no horizontal reference at all.
    fn facade_scene(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let stripe = (x / 40) % 2 == 0;
                let v = if stripe { 0.8 } else { 0.3 };
                let px = (v * 255.0) as u8;
                img.put_pixel(x, y, Rgba([px, px, px, 255]));
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    /// Crop away the transparent corners `apply_rotation` leaves, the way a
    /// user-facing crop would, so the test measures the scene and not the
    /// frame. (The estimator also masks alpha=0, so this is belt and braces.)
    fn centre_crop(img: &DynamicImage, frac: f32) -> DynamicImage {
        let (w, h) = img.dimensions();
        let cw = (w as f32 * frac) as u32;
        let ch = (h as f32 * frac) as u32;
        img.crop_imm((w - cw) / 2, (h - ch) / 2, cw, ch)
    }

    fn assert_close(actual: f32, expected: f32, tol: f32, what: &str) {
        assert!(
            (actual - expected).abs() <= tol,
            "{what}: expected {expected:.2}, got {actual:.2} (tol {tol})"
        );
    }

    #[test]
    fn level_horizon_reports_zero() {
        let est = estimate_level(&horizon_scene(800, 600)).expect("horizon should be confident");
        assert_close(est.angle, 0.0, 0.1, "level horizon");
        assert_eq!(est.reference, LevelReference::Horizontal);
        assert!(est.confidence > 0.3, "confidence {}", est.confidence);
    }

    #[test]
    fn tilted_horizon_is_undone_by_returned_angle() {
        for tilt in [-4.5f32, -2.5, 1.0, 3.7] {
            let scene = horizon_scene(1000, 700);
            let tilted = centre_crop(&apply_rotation(&scene, tilt), 0.7);
            let est = estimate_level(&tilted).expect("tilted horizon should be confident");
            assert_close(est.angle, -tilt, 0.2, &format!("tilt {tilt}"));
            // Self-consistency against the real rotation code, regardless of
            // anyone's reasoning about sign conventions: applying the
            // returned angle must produce a level image.
            let levelled = centre_crop(&apply_rotation(&tilted, est.angle), 0.7);
            let again =
                estimate_level(&levelled).expect("levelled image should still be confident");
            assert_close(again.angle, 0.0, 0.2, &format!("re-estimate after {tilt}"));
        }
    }

    #[test]
    fn large_tilt_is_corrected_when_both_axes_agree() {
        for tilt in [-9.0f32, 7.5] {
            let scene = building_scene(1000, 700);
            let tilted = centre_crop(&apply_rotation(&scene, tilt), 0.65);
            let est = estimate_level(&tilted).expect("building should be confident");
            assert_close(est.angle, -tilt, 0.25, &format!("building tilt {tilt}"));
        }
    }

    #[test]
    fn large_tilt_from_a_single_line_family_is_refused() {
        // Only horizontals, tilted 9°: indistinguishable from a level photo
        // of a sloped roof, so the tool must decline rather than guess.
        let scene = horizon_scene(1000, 700);
        let tilted = centre_crop(&apply_rotation(&scene, 9.0), 0.65);
        assert!(
            estimate_level(&tilted).is_none(),
            "uncorroborated 9° should be refused"
        );
    }

    #[test]
    fn verticals_are_used_when_nothing_horizontal_exists() {
        let scene = facade_scene(900, 700);
        let tilted = centre_crop(&apply_rotation(&scene, 2.0), 0.7);
        let est = estimate_level(&tilted).expect("facade should be confident");
        assert_eq!(est.reference, LevelReference::Vertical);
        assert_close(est.angle, -2.0, 0.2, "facade tilt");
    }

    #[test]
    fn featureless_image_returns_none() {
        let flat =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(600, 400, Rgba([120, 120, 120, 255])));
        assert!(estimate_level(&flat).is_none());
    }

    #[test]
    fn large_tilts_are_refused() {
        // Beyond MAX_TILT the fold is ambiguous, so the tool must decline
        // rather than confidently apply a wrong small correction.
        let scene = horizon_scene(1000, 700);
        let tilted = centre_crop(&apply_rotation(&scene, 22.0), 0.6);
        match estimate_level(&tilted) {
            None => {}
            Some(est) => assert!(
                est.confidence < 0.2,
                "22° tilt should not yield a confident estimate, got {est:?}"
            ),
        }
    }

    #[test]
    fn fold_wraps_correctly() {
        assert_close(fold_90(0.0), 0.0, 1e-6, "0");
        assert_close(fold_90(3.0), 3.0, 1e-6, "3");
        assert_close(fold_90(-3.0), -3.0, 1e-6, "-3");
        assert_close(fold_90(93.0), 3.0, 1e-6, "93");
        assert_close(fold_90(87.0), -3.0, 1e-6, "87");
        assert_close(fold_90(-87.0), 3.0, 1e-6, "-87");
        assert_close(fold_90(178.0), -2.0, 1e-6, "178");
    }
}
