//! One-click scene masks: Sky, Foreground and (automatic) Subject.
//!
//! All three share the same shape: a small semantic model produces a coarse
//! probability map, and a guided filter against the full-resolution photo
//! places the edges. What differs is the semantic source:
//!
//! - **Sky**: the sky-segmentation U²-Net's probabilities, used as-is. The
//!   model is a sigmoid classifier, so its output is *not* re-normalised:
//!   a photo without sky yields probabilities near zero everywhere and the
//!   tool reports that no sky was found instead of stretching noise to 100%.
//! - **Foreground**: the near side of Depth Anything's relative depth, split
//!   with Otsu's threshold and softened over a narrow band. This follows the
//!   scene's own depth structure (water in front of a skyline, a diver in
//!   front of a reef) rather than a saliency guess.
//! - **Subject**: saliency (U²-Net) proposes where the subject is; SAM is
//!   then prompted with a box around each salient component and produces
//!   the object-quality edges. If SAM's answer disagrees badly with the
//!   proposal, the refined saliency map is used instead.
//!
//! Models with an "up" prior (sky especially) see the photo the way the user
//! does: the current 90° orientation and flips are applied before inference
//! and undone on the probability map, which stays in the stored, unoriented
//! image space like every other mask.

use anyhow::{Result, ensure};
use image::imageops::{self, FilterType};
use image::{DynamicImage, GenericImageView, GrayImage, ImageBuffer, Luma, RgbImage};
use ort::session::Session;
use rayon::prelude::*;
use std::sync::Mutex;

use crate::ai_processing::{self, ImageEmbeddings};
use crate::subject_selection::{self, SubjectPoint, box_mean};

/// Below this share of the frame a Sky or Foreground result is reported as
/// "nothing found" rather than applied.
pub const MIN_COVERAGE: f32 = 0.005;
/// A sky probability map whose maximum stays under this is not sky.
const SKY_MIN_PEAK: f32 = 0.6;
/// A saliency map whose maximum stays under this has no clear subject.
const SUBJECT_MIN_PEAK: f32 = 0.5;
/// Otsu separability (between-class over total variance) below which the
/// depth histogram has no meaningful near/far split.
pub const FOREGROUND_MIN_SEPARABILITY: f32 = 0.55;
/// The near/far split of the photo and of its mirror must agree at least
/// this much (IoU); below it the depth ordering is not trustworthy.
pub const FOREGROUND_MIN_MIRROR_AGREEMENT: f32 = 0.5;
/// Half-width of the soft transition around the depth threshold, as a
/// fraction of the depth range.
const FOREGROUND_SOFT_BAND: f32 = 0.05;
/// Salient components smaller than this share of the largest one are noise.
const SUBJECT_COMPONENT_MIN_RATIO: f32 = 0.25;
/// Salient components smaller than this share of the frame are ignored.
const SUBJECT_COMPONENT_MIN_FRAME: f32 = 0.002;
/// Padding added around a salient component's box before prompting SAM.
const SUBJECT_BOX_PAD: f64 = 0.04;
/// SAM's answer must overlap the saliency proposal at least this much.
const SUBJECT_MIN_AGREEMENT: f32 = 0.4;
/// A "subject" covering more of the frame than this is a scene, not a
/// subject (a valley floor, a chart), and is declined.
const SUBJECT_MAX_COVERAGE: f32 = 0.5;
/// A subject touching this many frame borders, each along at least
/// `SUBJECT_BORDER_SHARE` of its length, is background that saliency
/// mistook for an object (open water, a plain).
const SUBJECT_MAX_BORDERS: usize = 3;
const SUBJECT_BORDER_SHARE: f32 = 0.25;
/// A large region running along the whole bottom edge is the ground (sea
/// floor, valley, road), not a subject.
const SUBJECT_GROUND_BOTTOM_SHARE: f32 = 0.9;
const SUBJECT_GROUND_MIN_COVERAGE: f32 = 0.25;
/// The single-pass and mirrored-pass sky maps must agree at least this
/// much (IoU of their >50% regions); below it the model is guessing.
pub const SKY_MIN_MIRROR_AGREEMENT: f32 = 0.55;

/// The user's current 90° orientation and flips, applied before inference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Orientation {
    pub steps: u8,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

#[derive(Debug, Clone)]
pub struct SceneMask {
    /// Full-resolution soft mask in the stored (unoriented) image space.
    pub mask: GrayImage,
    /// Share of pixels above 50%.
    pub coverage: f32,
}

pub type ProbabilityMap = ImageBuffer<Luma<f32>, Vec<f32>>;

/// Apply the orientation the way `apply_all_transformations` does: coarse
/// rotation first, then flips.
pub fn orient(image: &DynamicImage, o: Orientation) -> DynamicImage {
    let mut out = match o.steps % 4 {
        1 => image.rotate90(),
        2 => image.rotate180(),
        3 => image.rotate270(),
        _ => image.clone(),
    };
    if o.flip_horizontal {
        out = out.fliph();
    }
    if o.flip_vertical {
        out = out.flipv();
    }
    out
}

/// Undo [`orient`] on a map produced from the oriented image.
pub fn unorient_map(map: &ProbabilityMap, o: Orientation) -> ProbabilityMap {
    let mut out = map.clone();
    if o.flip_vertical {
        out = imageops::flip_vertical(&out);
    }
    if o.flip_horizontal {
        out = imageops::flip_horizontal(&out);
    }
    match o.steps % 4 {
        1 => imageops::rotate270(&out),
        2 => imageops::rotate180(&out),
        3 => imageops::rotate90(&out),
        _ => out,
    }
}

/// Apply [`orient`] to a map (the forward direction of [`unorient_map`]).
pub fn orient_map(map: &ProbabilityMap, o: Orientation) -> ProbabilityMap {
    let mut out = match o.steps % 4 {
        1 => imageops::rotate90(map),
        2 => imageops::rotate180(map),
        3 => imageops::rotate270(map),
        _ => map.clone(),
    };
    if o.flip_horizontal {
        out = imageops::flip_horizontal(&out);
    }
    if o.flip_vertical {
        out = imageops::flip_vertical(&out);
    }
    out
}

fn map_from(probs: Vec<f32>, w: u32, h: u32) -> ProbabilityMap {
    ProbabilityMap::from_raw(w, h, probs).expect("probability map dimensions")
}

/// Run a U²-Net-family model on the oriented photo and return its
/// probability map in unoriented layout. With `mirror_average` the model
/// also sees the horizontally mirrored photo and the two maps are averaged,
/// which damps the model's left/right asymmetries at the cost of a second
/// pass.
pub fn probabilities_oriented(
    image: &DynamicImage,
    session: &Mutex<Session>,
    input_size: u32,
    o: Orientation,
    mirror_average: bool,
) -> Result<ProbabilityMap> {
    let oriented = orient(image, o);
    let (p, w, h) = ai_processing::run_u2net_probabilities(&oriented, session, input_size)?;
    let mut map = map_from(p, w, h);
    if mirror_average {
        let (q, qw, qh) =
            ai_processing::run_u2net_probabilities(&oriented.fliph(), session, input_size)?;
        ensure!((qw, qh) == (w, h), "mirrored pass changed size");
        let mirrored = imageops::flip_horizontal(&map_from(q, qw, qh));
        for (a, b) in map.pixels_mut().zip(mirrored.pixels()) {
            a.0[0] = 0.5 * (a.0[0] + b.0[0]);
        }
    }
    Ok(unorient_map(&map, o))
}

/// Relative depth (near = 1) from Depth Anything on the oriented photo, in
/// unoriented layout.
pub fn depth_oriented(
    image: &DynamicImage,
    session: &Mutex<Session>,
    o: Orientation,
) -> Result<ProbabilityMap> {
    let oriented = orient(image, o);
    let depth = ai_processing::run_depth_anything_model(&oriented, session)?;
    let (w, h) = depth.dimensions();
    let map = map_from(depth.pixels().map(|p| p[0] as f32 / 255.0).collect(), w, h);
    Ok(unorient_map(&map, o))
}

/// Bilinear resample of an f32 grid.
fn resample(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; dw * dh];
    out.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let sy = ((y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let fy = sy - y0 as f32;
        for (x, out) in row.iter_mut().enumerate() {
            let sx = ((x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let fx = sx - x0 as f32;
            let top = src[y0 * sw + x0] * (1.0 - fx) + src[y0 * sw + x1] * fx;
            let bottom = src[y1 * sw + x0] * (1.0 - fx) + src[y1 * sw + x1] * fx;
            *out = top * (1.0 - fy) + bottom * fy;
        }
    });
    out
}

/// Place the edges of a coarse probability map with a guided filter against
/// the photo (He et al.), then render `a·RGB + b` at full resolution.
///
/// Statistics are gathered at ≤1024 px. `band` bounds how far the guided
/// value may move a coarse probability: confident interiors (0 or 1) are
/// never touched, and uncertain pixels move by at most ±band. Edges can
/// therefore only be *placed* within the coarse map's own uncertainty, not
/// invented in confident regions.
pub fn guided_upsample(coarse: &ProbabilityMap, guide: &RgbImage, band: f32) -> GrayImage {
    use nalgebra::{Matrix3, Vector3};
    let (width, height) = guide.dimensions();
    let (pw, ph) = (coarse.width() as usize, coarse.height() as usize);
    let scale = (1024.0 / width.max(height) as f64).min(1.0);
    let w = (width as f64 * scale).round().max(1.0) as usize;
    let h = (height as f64 * scale).round().max(1.0) as usize;
    let small = imageops::resize(guide, w as u32, h as u32, FilterType::Triangle);
    let p = resample(coarse.as_raw(), pw, ph, w, h);
    let rgb: [Vec<f32>; 3] =
        std::array::from_fn(|c| small.pixels().map(|p| p[c] as f32 / 255.0).collect());
    // The coarse model's own footprint: one model pixel, twice over, at
    // statistics resolution. That is the distance an edge may be moved.
    let radius = ((w.max(h) as f32 / pw.max(ph) as f32) * 2.0).ceil().max(1.0) as usize;
    let mean = |v: &[f32]| box_mean(v, w, h, radius);
    let mp = mean(&p);
    let mc: [Vec<f32>; 3] = std::array::from_fn(|c| mean(&rgb[c]));
    let cross = |a: &[f32], b: &[f32], ma: &[f32], mb: &[f32]| -> Vec<f32> {
        let product: Vec<_> = a.iter().zip(b).map(|(a, b)| a * b).collect();
        mean(&product)
            .iter()
            .zip(ma.iter().zip(mb))
            .map(|(v, (a, b))| v - a * b)
            .collect()
    };
    let cp: [Vec<f32>; 3] = std::array::from_fn(|c| cross(&rgb[c], &p, &mc[c], &mp));
    let pairs = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
    let covariance: Vec<_> = pairs
        .iter()
        .map(|&(a, b)| cross(&rgb[a], &rgb[b], &mc[a], &mc[b]))
        .collect();
    let mut coefficients = vec![[0.0; 4]; w * h];
    coefficients.par_iter_mut().enumerate().for_each(|(i, out)| {
        let cov = Matrix3::new(
            covariance[0][i] + 0.0025,
            covariance[1][i],
            covariance[2][i],
            covariance[1][i],
            covariance[3][i] + 0.0025,
            covariance[4][i],
            covariance[2][i],
            covariance[4][i],
            covariance[5][i] + 0.0025,
        );
        let a = cov
            .cholesky()
            .map(|m| m.solve(&Vector3::new(cp[0][i], cp[1][i], cp[2][i])))
            .unwrap_or_else(Vector3::zeros);
        *out = [
            a[0],
            a[1],
            a[2],
            mp[i] - (0..3).map(|c| a[c] * mc[c][i]).sum::<f32>(),
        ];
    });
    let averaged: [Vec<f32>; 4] =
        std::array::from_fn(|c| mean(&coefficients.iter().map(|v| v[c]).collect::<Vec<_>>()));
    let mut result = vec![0u8; width as usize * height as usize];
    result
        .par_chunks_mut(width as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let sy = ((y as f32 + 0.5) * h as f32 / height as f32 - 0.5).clamp(0.0, (h - 1) as f32);
            let y0 = sy.floor() as usize;
            let fy = sy - y0 as f32;
            for (x, out) in row.iter_mut().enumerate() {
                let sx =
                    ((x as f32 + 0.5) * w as f32 / width as f32 - 0.5).clamp(0.0, (w - 1) as f32);
                let x0 = sx.floor() as usize;
                let fx = sx - x0 as f32;
                let ids = [
                    y0 * w + x0,
                    y0 * w + (x0 + 1).min(w - 1),
                    (y0 + 1).min(h - 1) * w + x0,
                    (y0 + 1).min(h - 1) * w + (x0 + 1).min(w - 1),
                ];
                let weights = [
                    (1.0 - fx) * (1.0 - fy),
                    fx * (1.0 - fy),
                    (1.0 - fx) * fy,
                    fx * fy,
                ];
                let sample =
                    |v: &[f32]| ids.iter().zip(weights).map(|(&i, a)| v[i] * a).sum::<f32>();
                let original = sample(&p);
                let color = guide.get_pixel(x as u32, y as u32);
                let guided = sample(&averaged[3])
                    + (0..3)
                        .map(|c| sample(&averaged[c]) * color[c] as f32 / 255.0)
                        .sum::<f32>();
                let alpha = if original <= 0.0 || original >= 1.0 {
                    original
                } else {
                    guided.clamp(original - band, original + band)
                };
                *out = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
    GrayImage::from_raw(width, height, result).expect("mask dimensions")
}

/// Otsu's threshold on values in 0..1, plus its separability
/// (between-class variance over total variance, 0..1).
pub fn otsu(values: &[f32]) -> (f32, f32) {
    let mut hist = [0usize; 256];
    for &v in values {
        hist[((v.clamp(0.0, 1.0) * 255.0) as usize).min(255)] += 1;
    }
    let total = values.len() as f64;
    if total == 0.0 {
        return (0.5, 0.0);
    }
    let sum_all: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let mean_all = sum_all / total;
    let var_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| c as f64 * (i as f64 - mean_all).powi(2))
        .sum::<f64>()
        / total;
    // Between-class variance is flat across any empty stretch of the
    // histogram, so the threshold is the middle of the maximal plateau
    // rather than its first bin.
    let (mut best_lo, mut best_hi, mut best_var) = (127usize, 127usize, 0.0f64);
    let (mut w0, mut sum0) = (0.0f64, 0.0f64);
    for (t, &count) in hist.iter().enumerate().take(255) {
        w0 += count as f64;
        sum0 += t as f64 * count as f64;
        if w0 == 0.0 {
            continue;
        }
        let w1 = total - w0;
        if w1 == 0.0 {
            break;
        }
        let m0 = sum0 / w0;
        let m1 = (sum_all - sum0) / w1;
        let var = w0 * w1 * (m0 - m1) * (m0 - m1) / (total * total);
        if var > best_var * (1.0 + 1e-9) {
            best_var = var;
            best_lo = t;
            best_hi = t;
        } else if var >= best_var * (1.0 - 1e-9) && best_var > 0.0 {
            best_hi = t;
        }
    }
    let separability = if var_all > 0.0 { (best_var / var_all) as f32 } else { 0.0 };
    (((best_lo + best_hi) as f32 / 2.0 + 0.5) / 255.0, separability)
}

fn coverage(mask: &GrayImage) -> f32 {
    mask.pixels().filter(|p| p[0] > 127).count() as f32 / (mask.width() * mask.height()) as f32
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Sky from a probability map: gate, then refine. Returns `None` when the
/// photo holds no confident sky.
pub fn sky_from_probabilities(probs: &ProbabilityMap, guide: &RgbImage) -> Option<SceneMask> {
    let peak = probs.pixels().map(|p| p.0[0]).fold(0.0f32, f32::max);
    if peak < SKY_MIN_PEAK {
        return None;
    }
    let mask = guided_upsample(probs, guide, 0.35);
    let coverage = coverage(&mask);
    (coverage >= MIN_COVERAGE).then_some(SceneMask { mask, coverage })
}

/// IoU of the >50% regions of two maps of equal size.
pub fn map_agreement(a: &ProbabilityMap, b: &ProbabilityMap) -> f32 {
    let (mut inter, mut union) = (0usize, 0usize);
    for (p, q) in a.pixels().zip(b.pixels()) {
        let (x, y) = (p.0[0] > 0.5, q.0[0] > 0.5);
        inter += (x && y) as usize;
        union += (x || y) as usize;
    }
    if union == 0 { 1.0 } else { inter as f32 / union as f32 }
}

/// Is the >50% region's centroid in the upper half of the frame as the
/// user sees it? Sky that is not up is not sky.
pub fn sky_is_up(probs: &ProbabilityMap, o: Orientation) -> bool {
    let oriented = orient_map(probs, o);
    let (mut sum_y, mut n) = (0.0f64, 0usize);
    for (_, y, p) in oriented.enumerate_pixels() {
        if p.0[0] > 0.5 {
            sum_y += y as f64;
            n += 1;
        }
    }
    n > 0 && (sum_y / n as f64) < 0.5 * oriented.height() as f64
}

/// Sky mask for `image` (stored space) as seen with orientation `o`.
///
/// Two passes (photo and its mirror) are always run: their disagreement
/// is the confidence gate, and their average is the map that gets refined.
pub fn sky_mask(
    image: &DynamicImage,
    session: &Mutex<Session>,
    o: Orientation,
) -> Result<Option<SceneMask>> {
    let oriented = orient(image, o);
    let (p, w, h) = ai_processing::run_u2net_probabilities(&oriented, session, 320)?;
    let (q, qw, qh) = ai_processing::run_u2net_probabilities(&oriented.fliph(), session, 320)?;
    ensure!((qw, qh) == (w, h), "mirrored pass changed size");
    let a = map_from(p, w, h);
    let b = imageops::flip_horizontal(&map_from(q, qw, qh));
    let agreement = map_agreement(&a, &b);
    if agreement < SKY_MIN_MIRROR_AGREEMENT {
        log::info!("sky: passes agree only {agreement:.2}; declining");
        return Ok(None);
    }
    let mut avg = a;
    for (x, y) in avg.pixels_mut().zip(b.pixels()) {
        x.0[0] = 0.5 * (x.0[0] + y.0[0]);
    }
    // Position check happens in oriented layout, before unorienting.
    if !sky_is_up(&avg, Orientation::default()) {
        log::info!("sky: region is not in the upper half; declining");
        return Ok(None);
    }
    let probs = unorient_map(&avg, o);
    Ok(sky_from_probabilities(&probs, &image.to_rgb8()))
}

/// Foreground from relative depth (near = 1): Otsu split with a soft band,
/// refined against the photo. `None` when the depth has no clear split.
pub fn foreground_from_depth(depth: &ProbabilityMap, guide: &RgbImage) -> Option<SceneMask> {
    let (threshold, separability) = otsu(depth.as_raw());
    if separability < FOREGROUND_MIN_SEPARABILITY {
        return None;
    }
    let soft = ProbabilityMap::from_fn(depth.width(), depth.height(), |x, y| {
        let d = depth.get_pixel(x, y).0[0];
        Luma([smoothstep(
            threshold - FOREGROUND_SOFT_BAND,
            threshold + FOREGROUND_SOFT_BAND,
            d,
        )])
    });
    let mask = guided_upsample(&soft, guide, 0.35);
    let coverage = coverage(&mask);
    (MIN_COVERAGE..=1.0 - MIN_COVERAGE)
        .contains(&coverage)
        .then_some(SceneMask { mask, coverage })
}

/// Hard near-side mask of a depth map at its own Otsu threshold.
fn near_side(depth: &ProbabilityMap) -> ProbabilityMap {
    let (t, _) = otsu(depth.as_raw());
    ProbabilityMap::from_fn(depth.width(), depth.height(), |x, y| {
        Luma([if depth.get_pixel(x, y).0[0] > t { 1.0 } else { 0.0 }])
    })
}

/// Foreground mask for `image` (stored space) as seen with orientation `o`.
///
/// Depth is estimated on the photo and on its mirror; the two near/far
/// splits must agree, and their averaged depth is what gets split.
pub fn foreground_mask(
    image: &DynamicImage,
    depth_session: &Mutex<Session>,
    o: Orientation,
) -> Result<Option<SceneMask>> {
    let oriented = orient(image, o);
    let a = ai_processing::run_depth_anything_model(&oriented, depth_session)?;
    let b = ai_processing::run_depth_anything_model(&oriented.fliph(), depth_session)?;
    ensure!(a.dimensions() == b.dimensions(), "mirrored depth pass changed size");
    let (w, h) = a.dimensions();
    let a = map_from(a.pixels().map(|p| p[0] as f32 / 255.0).collect(), w, h);
    let b = imageops::flip_horizontal(&map_from(b.pixels().map(|p| p[0] as f32 / 255.0).collect(), w, h));
    let agreement = map_agreement(&near_side(&a), &near_side(&b));
    if agreement < FOREGROUND_MIN_MIRROR_AGREEMENT {
        log::info!("foreground: near/far splits agree only {agreement:.2}; declining");
        return Ok(None);
    }
    let mut avg = a;
    for (x, y) in avg.pixels_mut().zip(b.pixels()) {
        x.0[0] = 0.5 * (x.0[0] + y.0[0]);
    }
    let depth = unorient_map(&avg, o);
    Ok(foreground_from_depth(&depth, &image.to_rgb8()))
}

/// A salient component's footprint, in map coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Component {
    pub area: usize,
    pub min: (usize, usize),
    pub max: (usize, usize),
    pub centroid: (f64, f64),
}

/// 4-connected components of `probs > 0.5`, largest first, keeping only
/// those that matter next to the largest one.
pub fn salient_components(probs: &ProbabilityMap) -> Vec<Component> {
    let (w, h) = (probs.width() as usize, probs.height() as usize);
    let on: Vec<bool> = probs.pixels().map(|p| p.0[0] > 0.5).collect();
    let mut seen = vec![false; w * h];
    let mut comps = Vec::new();
    for start in 0..w * h {
        if !on[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut c = Component {
            area: 0,
            min: (usize::MAX, usize::MAX),
            max: (0, 0),
            centroid: (0.0, 0.0),
        };
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            c.area += 1;
            c.min = (c.min.0.min(x), c.min.1.min(y));
            c.max = (c.max.0.max(x), c.max.1.max(y));
            c.centroid.0 += x as f64;
            c.centroid.1 += y as f64;
            let mut push = |j: usize| {
                if on[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        c.centroid.0 /= c.area as f64;
        c.centroid.1 /= c.area as f64;
        comps.push(c);
    }
    comps.sort_by_key(|c| std::cmp::Reverse(c.area));
    let Some(largest) = comps.first().map(|c| c.area) else {
        return comps;
    };
    let frame = (w * h) as f32;
    comps.retain(|c| {
        c.area as f32 >= SUBJECT_COMPONENT_MIN_RATIO * largest as f32
            && c.area as f32 >= SUBJECT_COMPONENT_MIN_FRAME * frame
    });
    comps
}

/// Box prompt (image pixels) around a component, padded by a share of the
/// frame and clipped to it.
pub fn component_box(c: &Component, map: (u32, u32), image: (u32, u32)) -> Vec<SubjectPoint> {
    let sx = image.0 as f64 / map.0 as f64;
    let sy = image.1 as f64 / map.1 as f64;
    let pad_x = SUBJECT_BOX_PAD * image.0 as f64;
    let pad_y = SUBJECT_BOX_PAD * image.1 as f64;
    let x0 = (c.min.0 as f64 * sx - pad_x).clamp(0.0, image.0 as f64 - 1.0);
    let y0 = (c.min.1 as f64 * sy - pad_y).clamp(0.0, image.1 as f64 - 1.0);
    let x1 = ((c.max.0 + 1) as f64 * sx + pad_x).clamp(0.0, image.0 as f64 - 1.0);
    let y1 = ((c.max.1 + 1) as f64 * sy + pad_y).clamp(0.0, image.1 as f64 - 1.0);
    subject_selection::box_or_point((x0, y0), (x1, y1))
}

fn iou(a: &GrayImage, b: &GrayImage) -> f32 {
    let (mut inter, mut union) = (0usize, 0usize);
    for (p, q) in a.pixels().zip(b.pixels()) {
        let (x, y) = (p[0] > 127, q[0] > 127);
        inter += (x && y) as usize;
        union += (x || y) as usize;
    }
    if union == 0 { 1.0 } else { inter as f32 / union as f32 }
}

/// Result of an automatic subject selection.
#[derive(Debug, Clone)]
pub struct AutoSubject {
    pub scene: SceneMask,
    /// Prompts (image space) that reproduce this selection, so that later
    /// clicks refine rather than restart it: a single box for one salient
    /// component, one positive point per component otherwise.
    pub points: Vec<SubjectPoint>,
    /// True when SAM's answer was used; false when refined saliency stood in.
    pub from_sam: bool,
}

/// Automatic subject: saliency proposes, SAM delineates.
pub fn auto_subject(
    image: &DynamicImage,
    saliency_session: &Mutex<Session>,
    decoder: &Mutex<Session>,
    embeddings: &ImageEmbeddings,
    o: Orientation,
) -> Result<Option<AutoSubject>> {
    let probs = probabilities_oriented(image, saliency_session, 320, o, false)?;
    let (iw, ih) = image.dimensions();
    ensure!(
        embeddings.original_size == (iw, ih),
        "subject embeddings do not match the photo"
    );
    auto_subject_from_saliency(&probs, image, decoder, embeddings)
}

pub fn auto_subject_from_saliency(
    probs: &ProbabilityMap,
    image: &DynamicImage,
    decoder: &Mutex<Session>,
    embeddings: &ImageEmbeddings,
) -> Result<Option<AutoSubject>> {
    let peak = probs.pixels().map(|p| p.0[0]).fold(0.0f32, f32::max);
    if peak < SUBJECT_MIN_PEAK {
        return Ok(None);
    }
    let comps = salient_components(probs);
    if comps.is_empty() {
        return Ok(None);
    }
    let (iw, ih) = image.dimensions();
    let map_size = probs.dimensions();
    let guide = image.to_rgb8();
    let proposal = guided_upsample(probs, &guide, 0.35);

    let mut union: Option<GrayImage> = None;
    let mut points = Vec::new();
    for c in &comps {
        let prompt = component_box(c, map_size, (iw, ih));
        let mask = subject_selection::select(decoder, embeddings, &prompt, None, true)?;
        union = Some(match union {
            None => mask,
            Some(mut u) => {
                for (a, b) in u.pixels_mut().zip(mask.pixels()) {
                    a[0] = a[0].max(b[0]);
                }
                u
            }
        });
        if comps.len() == 1 {
            points = prompt;
        } else {
            let sx = iw as f64 / map_size.0 as f64;
            let sy = ih as f64 / map_size.1 as f64;
            points.push(SubjectPoint {
                x: c.centroid.0 * sx,
                y: c.centroid.1 * sy,
                label: 1,
            });
        }
    }
    let sam = union.expect("at least one component");
    let agreement = iou(&sam, &proposal);
    let (mask, from_sam) = if agreement >= SUBJECT_MIN_AGREEMENT {
        (sam, true)
    } else {
        log::warn!(
            "auto subject: SAM agreed with saliency only {agreement:.2}; using refined saliency"
        );
        (proposal, false)
    };
    let coverage = coverage(&mask);
    if !(MIN_COVERAGE..=SUBJECT_MAX_COVERAGE).contains(&coverage) {
        log::info!("auto subject: coverage {coverage:.2} outside the subject range; declining");
        return Ok(None);
    }
    let borders = border_shares(&mask);
    let touched = borders.iter().filter(|&&s| s >= SUBJECT_BORDER_SHARE).count();
    if touched >= SUBJECT_MAX_BORDERS {
        log::info!("auto subject: touches {touched} borders; declining");
        return Ok(None);
    }
    if borders[1] >= SUBJECT_GROUND_BOTTOM_SHARE && coverage >= SUBJECT_GROUND_MIN_COVERAGE {
        log::info!("auto subject: spans the whole bottom edge at {coverage:.2} coverage; declining as ground");
        return Ok(None);
    }
    Ok(Some(AutoSubject {
        scene: SceneMask { mask, coverage },
        points,
        from_sam,
    }))
}

/// Number of frame borders along which the mask runs for at least
/// `SUBJECT_BORDER_SHARE` of the border's length.
pub fn borders_touched(mask: &GrayImage) -> usize {
    border_shares(mask)
        .iter()
        .filter(|&&s| s >= SUBJECT_BORDER_SHARE)
        .count()
}

/// Share of each frame border (top, bottom, left, right) covered by the mask.
pub fn border_shares(mask: &GrayImage) -> [f32; 4] {
    let (w, h) = mask.dimensions();
    // Mattes often fade to zero on the very last pixel row; sample a
    // little inside the frame instead.
    let inset_x = (w / 100).max(2).min(w.saturating_sub(1));
    let inset_y = (h / 100).max(2).min(h.saturating_sub(1));
    let on = |x: u32, y: u32| mask.get_pixel(x, y)[0] > 127;
    let top = (0..w).filter(|&x| on(x, inset_y)).count() as f32 / w as f32;
    let bottom = (0..w).filter(|&x| on(x, h - 1 - inset_y)).count() as f32 / w as f32;
    let left = (0..h).filter(|&y| on(inset_x, y)).count() as f32 / h as f32;
    let right = (0..h).filter(|&y| on(w - 1 - inset_x, y)).count() as f32 / h as f32;
    [top, bottom, left, right]
}

/// Map an image-space point to the display space the frontend prompts in
/// (coarse rotation, then flips, then fine rotation about the centre). This
/// is the inverse of the transform `generate_ai_subject_mask` applies to
/// incoming clicks.
pub fn to_display_space(
    p: (f64, f64),
    image: (u32, u32),
    o: Orientation,
    rotation_deg: f32,
) -> (f64, f64) {
    let (w, h) = (image.0 as f64, image.1 as f64);
    let (mut x, mut y) = match o.steps % 4 {
        1 => (h - p.1, p.0),
        2 => (w - p.0, h - p.1),
        3 => (p.1, w - p.0),
        _ => p,
    };
    let (cw, ch) = if o.steps % 2 == 1 { (h, w) } else { (w, h) };
    if o.flip_horizontal {
        x = cw - x;
    }
    if o.flip_vertical {
        y = ch - y;
    }
    let (cx, cy) = (cw / 2.0, ch / 2.0);
    let a = (rotation_deg as f64).to_radians();
    let (px, py) = (x - cx, y - cy);
    (px * a.cos() - py * a.sin() + cx, px * a.sin() + py * a.cos() + cy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn map(w: u32, h: u32, f: impl Fn(u32, u32) -> f32) -> ProbabilityMap {
        ProbabilityMap::from_fn(w, h, |x, y| Luma([f(x, y)]))
    }

    #[test]
    fn orientation_round_trips_for_all_eight_cases() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(7, 5, |x, y| {
            Rgb([(x * 30) as u8, (y * 50) as u8, (x * y) as u8])
        }));
        let m = map(7, 5, |x, y| (x * 5 + y) as f32);
        for steps in 0..4u8 {
            for flips in 0..4u8 {
                let o = Orientation {
                    steps,
                    flip_horizontal: flips & 1 == 1,
                    flip_vertical: flips & 2 == 2,
                };
                // A map made from the oriented image, unoriented, must line
                // up with the stored image pixel for pixel.
                let oriented = orient(&img, o);
                let from_oriented = map(oriented.width(), oriented.height(), |x, y| {
                    let px = oriented.get_pixel(x, y);
                    // encode the stored coordinates through the colour
                    (px[0] as u32 / 30 * 5 + px[1] as u32 / 50) as f32
                });
                let back = unorient_map(&from_oriented, o);
                assert_eq!(back.dimensions(), m.dimensions(), "{o:?}");
                assert!(
                    back.pixels().zip(m.pixels()).all(|(a, b)| a == b),
                    "unorient(orient(x)) != x for {o:?}"
                );
            }
        }
    }

    #[test]
    fn orient_map_is_the_inverse_of_unorient_map() {
        let m = map(7, 5, |x, y| (x * 5 + y) as f32);
        for steps in 0..4u8 {
            for flips in 0..4u8 {
                let o = Orientation { steps, flip_horizontal: flips & 1 == 1, flip_vertical: flips & 2 == 2 };
                let back = unorient_map(&orient_map(&m, o), o);
                assert!(back.pixels().zip(m.pixels()).all(|(a, b)| a == b), "{o:?}");
            }
        }
    }

    #[test]
    fn sky_must_be_in_the_upper_half_as_displayed() {
        // Sky at the top of the stored image...
        let top = map(20, 10, |_, y| if y < 4 { 0.9 } else { 0.1 });
        assert!(sky_is_up(&top, Orientation::default()));
        // ...is at the bottom once the user rotates the photo 180°.
        assert!(!sky_is_up(&top, Orientation { steps: 2, ..Default::default() }));
        // A vertical flip alone also puts it at the bottom.
        assert!(!sky_is_up(&top, Orientation { flip_vertical: true, ..Default::default() }));
        // Stored sideways (sky on the left), displayed rotated 90° clockwise:
        // the left edge becomes the top.
        let left = map(10, 20, |x, _| if x < 4 { 0.9 } else { 0.1 });
        assert!(sky_is_up(&left, Orientation { steps: 1, ..Default::default() }));
        assert!(!sky_is_up(&left, Orientation { steps: 3, ..Default::default() }));
    }

    #[test]
    fn border_count_separates_objects_from_backgrounds() {
        let object = GrayImage::from_fn(100, 100, |x, y| Luma([if (30..70).contains(&x) && y >= 30 { 255 } else { 0 }]));
        assert_eq!(borders_touched(&object), 1);
        let water = GrayImage::from_fn(100, 100, |_, y| Luma([if y < 60 { 255 } else { 0 }]));
        assert_eq!(borders_touched(&water), 3);
        let ground = GrayImage::from_fn(100, 100, |x, y| Luma([if y > 60 && (x > 10 || y > 80) { 255 } else { 0 }]));
        let shares = border_shares(&ground);
        assert!(shares[1] > 0.95 && shares[0] == 0.0, "{shares:?}");
        assert_eq!(map_agreement(&map(4, 4, |_, _| 0.9), &map(4, 4, |_, _| 0.9)), 1.0);
        assert_eq!(map_agreement(&map(4, 4, |_, _| 0.9), &map(4, 4, |_, _| 0.1)), 0.0);
    }

    #[test]
    fn display_space_transform_inverts_the_click_transform() {
        // Mirror of the un-transform in generate_ai_subject_mask.
        let (iw, ih) = (400u32, 300u32);
        for steps in 0..4u8 {
            for flips in 0..4u8 {
                for rot in [0.0f32, 7.5, -12.0] {
                    let o = Orientation {
                        steps,
                        flip_horizontal: flips & 1 == 1,
                        flip_vertical: flips & 2 == 2,
                    };
                    let p = (123.0, 77.0);
                    let d = to_display_space(p, (iw, ih), o, rot);
                    // apply the click transform from ai_commands
                    let (crw, crh) = if steps % 2 == 1 { (ih as f64, iw as f64) } else { (iw as f64, ih as f64) };
                    let c = (crw / 2.0, crh / 2.0);
                    let a = (rot as f64).to_radians();
                    let (px, py) = (d.0 - c.0, d.1 - c.1);
                    let (ux, uy) = (px * a.cos() + py * a.sin() + c.0, -px * a.sin() + py * a.cos() + c.1);
                    let (fx, fy) = (
                        if o.flip_horizontal { crw - ux } else { ux },
                        if o.flip_vertical { crh - uy } else { uy },
                    );
                    let back = match steps {
                        1 => (fy, ih as f64 - fx),
                        2 => (iw as f64 - fx, ih as f64 - fy),
                        3 => (iw as f64 - fy, fx),
                        _ => (fx, fy),
                    };
                    assert!((back.0 - p.0).abs() < 1e-6 && (back.1 - p.1).abs() < 1e-6, "{o:?} rot {rot}: {back:?}");
                }
            }
        }
    }

    #[test]
    fn otsu_splits_a_bimodal_depth_and_flags_a_flat_one() {
        let bimodal: Vec<f32> = (0..1000).map(|i| if i % 3 == 0 { 0.2 } else { 0.8 }).collect();
        let (t, sep) = otsu(&bimodal);
        assert!(t > 0.2 && t < 0.8, "threshold {t}");
        assert!(sep > 0.95, "separability {sep}");
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let (_, sep_ramp) = otsu(&ramp);
        assert!(sep_ramp < 0.8, "a uniform ramp is not a clean split: {sep_ramp}");
        let flat = vec![0.4f32; 500];
        assert_eq!(otsu(&flat).1, 0.0);
    }

    /// A coarse, blurry mask edge that lies a few pixels off the true colour
    /// edge must be pulled onto it by the guide.
    #[test]
    fn guided_upsample_snaps_to_the_guide_edge() {
        let (w, h) = (256u32, 128u32);
        let guide = RgbImage::from_fn(w, h, |x, _| if x < 140 { Rgb([200, 200, 200]) } else { Rgb([30, 30, 30]) });
        // Coarse map: 32×16, soft transition centred at x≈120 in full-res
        // terms (i.e. 20 px left of the true edge).
        let coarse = map(32, 16, |x, _| {
            let fx = (x as f32 + 0.5) * 8.0;
            1.0 - smoothstep(100.0, 140.0, fx)
        });
        let mask = guided_upsample(&coarse, &guide, 0.35);
        let row = |x: u32| mask.get_pixel(x, 64)[0];
        assert!(row(20) > 240 && row(240) < 15, "interiors preserved: {} {}", row(20), row(240));
        // Inside the coarse transition band the guide must decide: bright
        // side high, dark side low, with the steep change at the true edge.
        assert!(row(130) > row(150) + 60, "edge not snapped: {} vs {}", row(130), row(150));
    }

    #[test]
    fn sky_gate_rejects_confident_nothing() {
        let guide = RgbImage::from_pixel(64, 64, Rgb([100, 100, 100]));
        let none = map(32, 32, |_, _| 0.05);
        assert!(sky_from_probabilities(&none, &guide).is_none());
        let half = map(32, 32, |_, y| if y < 16 { 0.98 } else { 0.02 });
        let sky = sky_from_probabilities(&half, &guide).expect("sky");
        assert!((sky.coverage - 0.5).abs() < 0.05, "{}", sky.coverage);
    }

    #[test]
    fn foreground_requires_a_depth_split() {
        let guide = RgbImage::from_pixel(64, 64, Rgb([100, 100, 100]));
        let split = map(32, 32, |_, y| if y < 20 { 0.15 } else { 0.85 });
        let fg = foreground_from_depth(&split, &guide).expect("foreground");
        assert!((fg.coverage - 12.0 / 32.0).abs() < 0.05, "{}", fg.coverage);
        // A flat depth (a wall) has no near side at all.
        let flat = map(32, 32, |_, _| 0.4);
        assert!(foreground_from_depth(&flat, &guide).is_none());
    }

    #[test]
    fn components_drop_noise_and_keep_peers() {
        let m = map(100, 100, |x, y| {
            let big = (10..50).contains(&x) && (10..50).contains(&y);
            let peer = (60..90).contains(&x) && (60..90).contains(&y);
            let speck = x == 95 && y == 5;
            if big || peer || speck { 0.9 } else { 0.1 }
        });
        let comps = salient_components(&m);
        assert_eq!(comps.len(), 2, "{comps:?}");
        assert_eq!(comps[0].area, 1600);
        assert_eq!(comps[1].area, 900);
        let b = component_box(&comps[0], (100, 100), (1000, 1000));
        assert_eq!(b.len(), 2);
        assert!(b[0].x < 100.0 && b[1].x > 500.0 && b[0].label == 2 && b[1].label == 3);
    }
}
