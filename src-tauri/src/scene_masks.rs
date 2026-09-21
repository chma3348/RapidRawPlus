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
/// Reaching into not-sky needs less nearby sky than the ordinary band,
/// because deep inside a crown the nearest confident sky is far away.
/// How much confident sky must sit in a pixel's neighbourhood before the
/// colour matte is allowed to reach into what the model called not-sky.
/// This is what recovers sky between branches: the model paints a tree
/// crown as one solid object, but the gaps in it are sky-coloured.
const SKY_REACH_MIN_NEIGHBOURHOOD: f32 = 0.01;
/// Inside that reach, colour has to be decisive before it may overrule the
/// model. Below this the pixel keeps the model's answer.
const SKY_REACH_MIN_ALPHA: f32 = 0.6;
const SKY_REACH_FULL_ALPHA: f32 = 0.85;
/// Reaching is only allowed where the neighbourhood is finely structured
/// (branches, cables, railings): a pixel there sits among both very dark
/// and sky-bright neighbours. A smooth pale wall beside the sky has low
/// local contrast and is left alone.
const SKY_REACH_MIN_CONTRAST: f32 = 0.05;
const SKY_REACH_FULL_CONTRAST: f32 = 0.12;
/// How far a pixel's colour may sit from the local sky colour, in units of
/// the sky's own colour spread, before it stops counting as sky.
const SKY_REACH_NEAR: f32 = 1.5;
const SKY_REACH_FAR: f32 = 3.5;
/// A saliency map whose maximum stays under this has no clear subject.
const SUBJECT_MIN_PEAK: f32 = 0.5;
/// Foreground is what lies nearer the camera than the subject. The cut sits
/// this far (in normalised disparity) in front of the subject's median
/// depth, or at its 75th percentile if the subject is itself deep, so the
/// ground the subject stands on and things beside it stay out.
const FOREGROUND_SUBJECT_MARGIN: f32 = 0.03;
/// Half-width of the soft transition around that cut.
const FOREGROUND_SOFT_BAND: f32 = 0.03;
/// Fewer subject samples than this at depth resolution is no subject.
const FOREGROUND_MIN_SUBJECT_SAMPLES: usize = 16;
/// The photo and its mirror must agree on what is in front (IoU) once the
/// region is large enough for the comparison to mean something.
pub const FOREGROUND_MIN_MIRROR_AGREEMENT: f32 = 0.5;
const FOREGROUND_AGREEMENT_MIN_SHARE: f32 = 0.02;
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
/// BiRefNet does not mistake ground or water for a subject the way
/// saliency did, and close portraits legitimately fill 60–70% of the frame
/// and touch three edges. It is only declined when it takes nearly the
/// whole frame or runs along all four edges (an interior wall).
const BIREFNET_MAX_COVERAGE: f32 = 0.85;
/// Components smaller than this share of the largest are specks.
const BIREFNET_COMPONENT_MIN_RATIO: f32 = 0.05;
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

/// Box mean, re-exported for the sky-replace compositor.
pub fn box_mean_public(values: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    box_mean(values, w, h, radius)
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
    let radius = ((w.max(h) as f32 / pw.max(ph) as f32) * 2.0)
        .ceil()
        .max(1.0) as usize;
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
    coefficients
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, out)| {
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
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
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
    let separability = if var_all > 0.0 {
        (best_var / var_all) as f32
    } else {
        0.0
    };
    (
        ((best_lo + best_hi) as f32 / 2.0 + 0.5) / 255.0,
        separability,
    )
}

fn coverage(mask: &GrayImage) -> f32 {
    mask.pixels().filter(|p| p[0] > 127).count() as f32 / (mask.width() * mask.height()) as f32
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// ADE20K class index for sky in the scene-labelling model.
pub const ADE_SKY: usize = 2;
/// Long side the scene-labelling model analyses (fixed export size).
pub const SCENE_LABEL_SIZE: u32 = 768;

/// Gaussian-ish blur (three box passes) of a masked field.
fn blur3(values: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let a = box_mean(values, w, h, radius);
    let b = box_mean(&a, w, h, radius);
    box_mean(&b, w, h, radius)
}

/// Sky edges placed by colour where the sky and the land beside it differ
/// in colour, and by the guided filter where they do not.
///
/// In the model's uncertain band each pixel's sky share is its position on
/// the line between the local sky colour and the local non-sky colour
/// (both averaged from confident pixels nearby). That recovers treetops and
/// ridge detail the 768 px model smooths over. Where the two colours are
/// too close to separate (haze against haze), reliability drops to zero and
/// the guided-filter result is used instead, so low-contrast horizons are
/// never decided by noise.
pub fn sky_refine(probs: &ProbabilityMap, guide: &RgbImage) -> GrayImage {
    let guided = guided_upsample(probs, guide, 0.35);
    let (width, height) = guide.dimensions();
    let scale = (1024.0 / width.max(height) as f64).min(1.0);
    let w = (width as f64 * scale).round().max(1.0) as usize;
    let h = (height as f64 * scale).round().max(1.0) as usize;
    let small = imageops::resize(guide, w as u32, h as u32, FilterType::Triangle);
    let p = resample(
        probs.as_raw(),
        probs.width() as usize,
        probs.height() as usize,
        w,
        h,
    );
    let sky: Vec<f32> = p.iter().map(|&v| (v > 0.9) as u8 as f32).collect();
    let land: Vec<f32> = p.iter().map(|&v| (v < 0.1) as u8 as f32).collect();
    // Two sampling scales: a tight one (σ ≈ 12 px at 1024) that keeps the
    // colours local, and a wide one (σ ≈ 36 px) for pixels whose nearest
    // confident sky or land is far away — exactly the case in haze, where
    // the model's uncertain band is broad and colour help matters most.
    let channels: [Vec<f32>; 3] =
        std::array::from_fn(|c| small.pixels().map(|px| px[c] as f32 / 255.0).collect());
    let sample = |radius: usize| {
        let ws = blur3(&sky, w, h, radius);
        let wf = blur3(&land, w, h, radius);
        let sc: [Vec<f32>; 3] = std::array::from_fn(|c| {
            let num = blur3(
                &channels[c]
                    .iter()
                    .zip(&sky)
                    .map(|(a, b)| a * b)
                    .collect::<Vec<_>>(),
                w,
                h,
                radius,
            );
            num.iter().zip(&ws).map(|(n, d)| n / d.max(1e-4)).collect()
        });
        let fc: [Vec<f32>; 3] = std::array::from_fn(|c| {
            let num = blur3(
                &channels[c]
                    .iter()
                    .zip(&land)
                    .map(|(a, b)| a * b)
                    .collect::<Vec<_>>(),
                w,
                h,
                radius,
            );
            num.iter().zip(&wf).map(|(n, d)| n / d.max(1e-4)).collect()
        });
        (ws, wf, sc, fc)
    };
    // Three scales, tight to very wide. The widest reaches ~10% of the
    // frame, which is what lets a gap deep inside a tree crown still see
    // the sky's colour, while staying local enough to follow a sunset
    // gradient rather than averaging the whole sky into one colour.
    let scales = [sample(6), sample(18), sample((w.max(h) / 6).max(48))];
    let enough = |a: f32, b: f32| a.min(b) >= 0.02;
    let level = |i: usize| {
        if enough(scales[0].0[i], scales[0].1[i]) {
            0
        } else if enough(scales[1].0[i], scales[1].1[i]) {
            1
        } else {
            2
        }
    };
    let levels: Vec<usize> = (0..w * h).map(level).collect();
    let ws: Vec<f32> = (0..w * h).map(|i| scales[levels[i]].0[i]).collect();
    let wf: Vec<f32> = (0..w * h).map(|i| scales[levels[i]].1[i]).collect();
    let sc: [Vec<f32>; 3] =
        std::array::from_fn(|c| (0..w * h).map(|i| scales[levels[i]].2[c][i]).collect());
    let fc: [Vec<f32>; 3] =
        std::array::from_fn(|c| (0..w * h).map(|i| scales[levels[i]].3[c][i]).collect());
    // Local luma contrast at stats resolution, and the spread of the
    // confident sky's own colour: the two things the reach test needs.
    let luma: Vec<f32> = small
        .pixels()
        .map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0)
        .collect();
    let cr = 4;
    let luma_mean = box_mean(&luma, w, h, cr);
    let luma_sq = box_mean(&luma.iter().map(|v| v * v).collect::<Vec<_>>(), w, h, cr);
    let contrast: Vec<f32> = luma_sq
        .iter()
        .zip(&luma_mean)
        .map(|(sq, m)| (sq - m * m).max(0.0).sqrt())
        .collect();
    let sky_spread = {
        let n: f32 = sky.iter().sum();
        if n < 8.0 {
            0.05
        } else {
            let var: f32 = channels
                .iter()
                .map(|chan| {
                    let mean: f32 = chan.iter().zip(&sky).map(|(v, m)| v * m).sum::<f32>() / n;
                    chan.iter()
                        .zip(&sky)
                        .map(|(v, m)| m * (v - mean) * (v - mean))
                        .sum::<f32>()
                        / n
                })
                .sum();
            (var / 3.0).sqrt().max(0.02)
        }
    };
    let p_full = resample(
        probs.as_raw(),
        probs.width() as usize,
        probs.height() as usize,
        width as usize,
        height as usize,
    );
    let mut out = guided.clone();
    out.as_mut()
        .par_chunks_mut(width as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let sy = ((y as f32 + 0.5) * h as f32 / height as f32 - 0.5).clamp(0.0, (h - 1) as f32);
            let y0 = sy.floor() as usize;
            let y1 = (y0 + 1).min(h - 1);
            let fy = sy - y0 as f32;
            for (x, o) in row.iter_mut().enumerate() {
                let pv = p_full[y * width as usize + x];
                if pv >= 0.98 {
                    continue;
                }
                let sx =
                    ((x as f32 + 0.5) * w as f32 / width as f32 - 0.5).clamp(0.0, (w - 1) as f32);
                let x0 = sx.floor() as usize;
                let x1 = (x0 + 1).min(w - 1);
                let fx = sx - x0 as f32;
                let bil = |v: &[f32]| {
                    let t = v[y0 * w + x0] * (1.0 - fx) + v[y0 * w + x1] * fx;
                    let b = v[y1 * w + x0] * (1.0 - fx) + v[y1 * w + x1] * fx;
                    t * (1.0 - fy) + b * fy
                };
                let px = guide.get_pixel(x as u32, y as u32);
                let (mut num, mut den) = (0.0f32, 0.0f32);
                for c in 0..3 {
                    let s_c = bil(&sc[c]);
                    let f_c = bil(&fc[c]);
                    let d = s_c - f_c;
                    num += (px[c] as f32 / 255.0 - f_c) * d;
                    den += d * d;
                }
                let sky_near = bil(&ws);
                let g = *o as f32 / 255.0;
                if pv <= 0.02 {
                    // Reaching into what the model called not-sky. Inside a
                    // crown the local "non-sky" colour is contaminated by
                    // the sky showing through, so the two-colour line above
                    // is useless here. Ask instead whether this pixel *is*
                    // the sky's colour, somewhere finely structured with
                    // confident sky in range.
                    if sky_near < SKY_REACH_MIN_NEIGHBOURHOOD {
                        continue;
                    }
                    let mut dist = 0.0f32;
                    for c in 0..3 {
                        let d = px[c] as f32 / 255.0 - bil(&sc[c]);
                        dist += d * d;
                    }
                    // NB: smoothstep clamps its denominator, so it must be
                    // called with an ascending range; invert instead.
                    let like_sky =
                        1.0 - smoothstep(SKY_REACH_NEAR, SKY_REACH_FAR, dist.sqrt() / sky_spread);
                    let structured = smoothstep(
                        SKY_REACH_MIN_CONTRAST,
                        SKY_REACH_FULL_CONTRAST,
                        bil(&contrast),
                    );
                    let rel = structured
                        * smoothstep(0.004, 0.03, sky_near)
                        * smoothstep(SKY_REACH_MIN_ALPHA, SKY_REACH_FULL_ALPHA, like_sky);
                    *o = ((rel * like_sky + (1.0 - rel) * g).clamp(0.0, 1.0) * 255.0).round() as u8;
                    continue;
                }
                let alpha = (num / den.max(1e-6)).clamp(0.0, 1.0);
                let rel = smoothstep(0.04, 0.12, den.sqrt())
                    * smoothstep(0.02, 0.1, sky_near)
                    * smoothstep(0.02, 0.1, bil(&wf));
                *o = ((rel * alpha + (1.0 - rel) * g).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
    out
}

/// Long side the photo is tiled at for the detail pass.
pub const SCENE_TILE_LONG_SIDE: u32 = 1152;

/// Run the scene-labelling model on overlapping tiles at a higher
/// resolution and blend them with a Hann window.
///
/// At 768 px for the whole photo, sky between bridge cables or tree
/// branches is one or two pixels wide and comes back blurred. Tiles see
/// those gaps at their real size. Tiles also lose context (a white wall in
/// a tile can look like sky), so the caller gates this with the
/// whole-photo pass.
fn scene_sky_tiled(
    image: &DynamicImage,
    session: &Mutex<Session>,
    global: &ProbabilityMap,
) -> Result<ProbabilityMap> {
    let (w0, h0) = image.dimensions();
    let tile = SCENE_LABEL_SIZE;
    let scale = SCENE_TILE_LONG_SIDE as f32 / w0.max(h0) as f32;
    let w = ((w0 as f32 * scale).round() as u32).max(tile);
    let h = ((h0 as f32 * scale).round() as u32).max(tile);
    let big = image.resize_exact(w, h, FilterType::Triangle);
    let starts = |len: u32| -> Vec<u32> {
        if len <= tile {
            return vec![0];
        }
        let step = tile / 2;
        let mut v: Vec<u32> = (0..)
            .map(|i| i * step)
            .take_while(|&s| s + tile < len)
            .collect();
        v.push(len - tile);
        v
    };
    let (xs, ys) = (starts(w), starts(h));
    let n = (w * h) as usize;
    let mut acc = vec![0.0f32; n];
    let mut wsum = vec![0.0f32; n];
    // Tiles only earn their cost where the whole-photo pass is undecided.
    // An open horizon needs one or two; a bridge lattice needs them all.
    let uncertain: Vec<bool> = global
        .pixels()
        .map(|p| p.0[0] > 0.05 && p.0[0] < 0.95)
        .collect();
    let (gw, gh) = (global.width() as usize, global.height() as usize);
    let mut ran = 0usize;
    // Hann window, so tile seams do not show.
    let hann: Vec<f32> = (0..tile)
        .map(|i| {
            0.001 + 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (tile - 1) as f32).cos())
        })
        .collect();
    for &y0 in &ys {
        for &x0 in &xs {
            let band = {
                let to_g = |v: u32, from: u32, to: usize| {
                    ((v as f32 / from as f32) * to as f32).round() as usize
                };
                let (gx0, gy0) = (to_g(x0, w, gw), to_g(y0, h, gh));
                let (gx1, gy1) = (
                    to_g(x0 + tile, w, gw).min(gw),
                    to_g(y0 + tile, h, gh).min(gh),
                );
                let mut count = 0usize;
                for gy in gy0..gy1 {
                    for gx in gx0..gx1 {
                        count += uncertain[gy * gw + gx] as usize;
                    }
                }
                let area = ((gx1 - gx0) * (gy1 - gy0)).max(1);
                count as f32 / area as f32
            };
            if band < 0.005 {
                continue;
            }
            ran += 1;
            let crop = big.crop_imm(x0, y0, tile, tile);
            let (probs, classes, rw, rh) = ai_processing::run_scene_labels(&crop, session, tile)?;
            ensure!(classes > ADE_SKY, "scene-label model has too few classes");
            let sky = &probs[ADE_SKY * (rw * rh) as usize..];
            for ty in 0..rh.min(h - y0) {
                for tx in 0..rw.min(w - x0) {
                    let weight = hann[ty as usize] * hann[tx as usize];
                    let i = ((y0 + ty) * w + x0 + tx) as usize;
                    acc[i] += sky[(ty * rw + tx) as usize] * weight;
                    wsum[i] += weight;
                }
            }
        }
    }
    log::info!("sky: ran {ran} of {} tiles", xs.len() * ys.len());
    // Untiled areas (confident in the whole-photo pass) keep its value.
    let upscaled = resample(global.as_raw(), gw, gh, w as usize, h as usize);
    let fused: Vec<f32> = acc
        .iter()
        .zip(&wsum)
        .zip(&upscaled)
        .map(|((a, w), g)| if *w > 1e-6 { a / w } else { *g })
        .collect();
    Ok(map_from(fused, w, h))
}

/// Sky probability for the photo as the user sees it: the tiled detail
/// pass, gated by the whole-photo pass so tiles cannot invent sky where the
/// full view sees none.
pub fn scene_sky_detailed(
    image: &DynamicImage,
    session: &Mutex<Session>,
    o: Orientation,
) -> Result<ProbabilityMap> {
    let oriented = orient(image, o);
    let (probs, classes, rw, rh) =
        ai_processing::run_scene_labels(&oriented, session, SCENE_LABEL_SIZE)?;
    ensure!(classes > ADE_SKY, "scene-label model has too few classes");
    let n = (rw * rh) as usize;
    let global = map_from(probs[ADE_SKY * n..(ADE_SKY + 1) * n].to_vec(), rw, rh);
    let tiled = scene_sky_tiled(&oriented, session, &global)?;

    // Where the whole photo sees no sky within ~2% of the frame, tiles do
    // not get to claim any. Blur first so the gate is regional, not a
    // per-pixel copy of the coarse mask.
    let (gw, gh) = (global.width() as usize, global.height() as usize);
    let radius = (gw.max(gh) / 48).max(1);
    let spread = blur3(global.as_raw(), gw, gh, radius);
    let (tw, th) = (tiled.width() as usize, tiled.height() as usize);
    let gate = resample(&spread, gw, gh, tw, th);
    let fused = ProbabilityMap::from_fn(tiled.width(), tiled.height(), |x, y| {
        let allowed = smoothstep(0.02, 0.2, gate[y as usize * tw + x as usize]);
        Luma([tiled.get_pixel(x, y).0[0] * allowed])
    });
    Ok(unorient_map(&fused, o))
}

/// Sky probability (stored layout) from the scene-labelling model run on
/// the photo as the user sees it.
pub fn scene_sky_probabilities(
    image: &DynamicImage,
    session: &Mutex<Session>,
    o: Orientation,
) -> Result<ProbabilityMap> {
    let oriented = orient(image, o);
    let (probs, classes, rw, rh) =
        ai_processing::run_scene_labels(&oriented, session, SCENE_LABEL_SIZE)?;
    ensure!(classes > ADE_SKY, "scene-label model has too few classes");
    let n = (rw * rh) as usize;
    let sky = map_from(probs[ADE_SKY * n..(ADE_SKY + 1) * n].to_vec(), rw, rh);
    Ok(unorient_map(&sky, o))
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
    if union == 0 {
        1.0
    } else {
        inter as f32 / union as f32
    }
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

/// Sky mask from the scene-labelling model: tiled detail gated by the
/// whole-photo pass, then edges placed by colour where sky and land differ.
pub fn sky_mask_scene(
    image: &DynamicImage,
    session: &Mutex<Session>,
    o: Orientation,
) -> Result<Option<SceneMask>> {
    let probs = scene_sky_detailed(image, session, o)?;
    let peak = probs.pixels().map(|p| p.0[0]).fold(0.0f32, f32::max);
    if peak < SKY_MIN_PEAK {
        log::info!("sky: peak probability {peak:.2}; declining");
        return Ok(None);
    }
    if !sky_is_up(&probs, o) {
        log::info!("sky: region is not in the upper half as displayed; declining");
        return Ok(None);
    }
    let mask = sky_refine(&probs, &image.to_rgb8());
    let coverage = coverage(&mask);
    Ok((coverage >= MIN_COVERAGE).then_some(SceneMask { mask, coverage }))
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

/// Why no foreground was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForegroundDecline {
    /// There is no subject to measure the foreground against.
    NoSubject,
    /// Nothing in the photo is nearer the camera than the subject.
    NothingInFront,
    /// The depth ordering in front of the subject is not stable.
    Unstable,
}

impl ForegroundDecline {
    pub fn message(self) -> &'static str {
        match self {
            Self::NoSubject => {
                "No subject to measure the foreground against. Add a Subject mask first, then Foreground."
            }
            Self::NothingInFront => "Nothing in this photo is in front of the subject.",
            Self::Unstable => {
                "The depth in front of the subject is too ambiguous to select reliably."
            }
        }
    }
}

/// Depth-map-sized copy of a full-resolution mask, as 0..1.
fn mask_to_map(mask: &GrayImage, w: u32, h: u32) -> ProbabilityMap {
    let small = imageops::resize(mask, w, h, FilterType::Triangle);
    map_from(small.pixels().map(|p| p[0] as f32 / 255.0).collect(), w, h)
}

/// Disparity (near = 1) beyond which a pixel is in front of the subject.
pub fn subject_depth_cut(depth: &ProbabilityMap, subject: &ProbabilityMap) -> Option<f32> {
    // Interior samples only: at depth resolution the subject's outline
    // mixes in whatever is behind it.
    let mut samples: Vec<f32> = depth
        .pixels()
        .zip(subject.pixels())
        .filter(|(_, s)| s.0[0] > 0.8)
        .map(|(d, _)| d.0[0])
        .collect();
    if samples.len() < FOREGROUND_MIN_SUBJECT_SAMPLES {
        return None;
    }
    samples.sort_by(f32::total_cmp);
    let q = |f: f32| samples[((samples.len() - 1) as f32 * f).round() as usize];
    Some(q(0.75).max(q(0.5) + FOREGROUND_SUBJECT_MARGIN))
}

fn in_front(depth: &ProbabilityMap, subject: &ProbabilityMap, cut: f32) -> ProbabilityMap {
    ProbabilityMap::from_fn(depth.width(), depth.height(), |x, y| {
        let d = depth.get_pixel(x, y).0[0];
        let s = subject.get_pixel(x, y).0[0];
        Luma([smoothstep(cut - FOREGROUND_SOFT_BAND, cut + FOREGROUND_SOFT_BAND, d) * (1.0 - s)])
    })
}

/// Foreground relative to a subject: everything nearer the camera than the
/// subject, refined against the photo, never overlapping the subject.
/// `depth` is relative disparity (near = 1) in stored layout; `subject` is a
/// full-resolution mask the size of `guide`.
pub fn foreground_from_subject(
    depth: &ProbabilityMap,
    subject: &GrayImage,
    guide: &RgbImage,
) -> std::result::Result<SceneMask, ForegroundDecline> {
    if subject.dimensions() != guide.dimensions() {
        return Err(ForegroundDecline::NoSubject);
    }
    let subject_small = mask_to_map(subject, depth.width(), depth.height());
    let cut = subject_depth_cut(depth, &subject_small).ok_or(ForegroundDecline::NoSubject)?;
    let soft = in_front(depth, &subject_small, cut);
    let mut mask = guided_upsample(&soft, guide, 0.35);
    // The guided filter can bleed a little across the subject's outline;
    // the subject itself is never foreground.
    for (m, s) in mask.pixels_mut().zip(subject.pixels()) {
        m[0] = m[0].min(255 - s[0]);
    }
    let coverage = coverage(&mask);
    if coverage < MIN_COVERAGE {
        return Err(ForegroundDecline::NothingInFront);
    }
    Ok(SceneMask { mask, coverage })
}

/// Foreground mask for `image` (stored space) relative to `subject`
/// (full-resolution, stored space), seen with orientation `o`.
///
/// Depth is estimated on the photo and on its mirror. When the region in
/// front of the subject is large enough to compare, both passes must agree
/// on it; their averaged depth is what gets cut.
pub fn foreground_mask(
    image: &DynamicImage,
    depth_session: &Mutex<Session>,
    subject: &GrayImage,
    o: Orientation,
) -> Result<std::result::Result<SceneMask, ForegroundDecline>> {
    let oriented = orient(image, o);
    let a = ai_processing::run_depth_anything_model(&oriented, depth_session)?;
    let b = ai_processing::run_depth_anything_model(&oriented.fliph(), depth_session)?;
    ensure!(
        a.dimensions() == b.dimensions(),
        "mirrored depth pass changed size"
    );
    let (w, h) = a.dimensions();
    let a = map_from(a.pixels().map(|p| p[0] as f32 / 255.0).collect(), w, h);
    let b = imageops::flip_horizontal(&map_from(
        b.pixels().map(|p| p[0] as f32 / 255.0).collect(),
        w,
        h,
    ));
    let (a, b) = (unorient_map(&a, o), unorient_map(&b, o));

    let subject_small = mask_to_map(subject, a.width(), a.height());
    if let (Some(cut_a), Some(cut_b)) = (
        subject_depth_cut(&a, &subject_small),
        subject_depth_cut(&b, &subject_small),
    ) {
        let (fa, fb) = (
            in_front(&a, &subject_small, cut_a),
            in_front(&b, &subject_small, cut_b),
        );
        let n = (fa.width() * fa.height()) as f32;
        let share = |m: &ProbabilityMap| m.pixels().filter(|p| p.0[0] > 0.5).count() as f32 / n;
        if share(&fa).max(share(&fb)) >= FOREGROUND_AGREEMENT_MIN_SHARE {
            let agreement = map_agreement(&fa, &fb);
            if agreement < FOREGROUND_MIN_MIRROR_AGREEMENT {
                log::info!("foreground: passes agree only {agreement:.2}; declining");
                return Ok(Err(ForegroundDecline::Unstable));
            }
        }
    }
    let mut avg = a;
    for (x, y) in avg.pixels_mut().zip(b.pixels()) {
        x.0[0] = 0.5 * (x.0[0] + y.0[0]);
    }
    Ok(foreground_from_subject(&avg, subject, &image.to_rgb8()))
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
    salient_components_with(probs, SUBJECT_COMPONENT_MIN_RATIO)
}

pub fn salient_components_with(probs: &ProbabilityMap, min_ratio: f32) -> Vec<Component> {
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
        c.area as f32 >= min_ratio * largest as f32
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
    if union == 0 {
        1.0
    } else {
        inter as f32 / union as f32
    }
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
    let touched = borders
        .iter()
        .filter(|&&s| s >= SUBJECT_BORDER_SHARE)
        .count();
    if touched >= SUBJECT_MAX_BORDERS {
        log::info!("auto subject: touches {touched} borders; declining");
        return Ok(None);
    }
    if borders[1] >= SUBJECT_GROUND_BOTTOM_SHARE && coverage >= SUBJECT_GROUND_MIN_COVERAGE {
        log::info!(
            "auto subject: spans the whole bottom edge at {coverage:.2} coverage; declining as ground"
        );
        return Ok(None);
    }
    Ok(Some(AutoSubject {
        scene: SceneMask { mask, coverage },
        points,
        from_sam,
    }))
}

/// Automatic subject from BiRefNet: the model's own cutout, cleaned of
/// specks, with edges placed against the full-resolution photo.
pub fn auto_subject_birefnet(
    image: &DynamicImage,
    session: &Mutex<Session>,
    o: Orientation,
) -> Result<Option<AutoSubject>> {
    let oriented = orient(image, o);
    let (p, w, h) = ai_processing::run_birefnet(&oriented, session)?;
    let probs = unorient_map(&map_from(p, w, h), o);
    Ok(auto_subject_from_birefnet(&probs, image))
}

/// Shared by the app and the tests: gate and refine a BiRefNet map (stored
/// layout, any aspect: it is stretched back to the photo).
pub fn auto_subject_from_birefnet(
    probs: &ProbabilityMap,
    image: &DynamicImage,
) -> Option<AutoSubject> {
    let peak = probs.pixels().map(|p| p.0[0]).fold(0.0f32, f32::max);
    if peak < SUBJECT_MIN_PEAK {
        return None;
    }
    let mut comps = salient_components_with(probs, BIREFNET_COMPONENT_MIN_RATIO);
    if comps.is_empty() {
        return None;
    }
    // Zero everything outside the kept components (and their soft rims) so
    // specks the component filter dropped cannot reappear after refinement.
    let (mw, mh) = (probs.width() as usize, probs.height() as usize);
    let mut keep = vec![false; mw * mh];
    for c in &comps {
        let pad = 3;
        for y in c.min.1.saturating_sub(pad)..=(c.max.1 + pad).min(mh - 1) {
            for x in c.min.0.saturating_sub(pad)..=(c.max.0 + pad).min(mw - 1) {
                keep[y * mw + x] = true;
            }
        }
    }
    let cleaned = ProbabilityMap::from_fn(probs.width(), probs.height(), |x, y| {
        let v = probs.get_pixel(x, y).0[0];
        Luma([if keep[y as usize * mw + x as usize] {
            v
        } else {
            0.0
        }])
    });
    let mask = guided_upsample(&cleaned, &image.to_rgb8(), 0.25);
    let coverage = coverage(&mask);
    if !(MIN_COVERAGE..=BIREFNET_MAX_COVERAGE).contains(&coverage) || borders_touched(&mask) >= 4 {
        log::info!("auto subject: coverage {coverage:.2}, declining");
        return None;
    }
    let (iw, ih) = image.dimensions();
    let map_size = probs.dimensions();
    comps.truncate(8);
    let points = if comps.len() == 1 {
        component_box(&comps[0], map_size, (iw, ih))
    } else {
        let (sx, sy) = (iw as f64 / map_size.0 as f64, ih as f64 / map_size.1 as f64);
        comps
            .iter()
            .map(|c| SubjectPoint {
                x: c.centroid.0 * sx,
                y: c.centroid.1 * sy,
                label: 1,
            })
            .collect()
    };
    Some(AutoSubject {
        scene: SceneMask { mask, coverage },
        points,
        from_sam: false,
    })
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
    (
        px * a.cos() - py * a.sin() + cx,
        px * a.sin() + py * a.cos() + cy,
    )
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
                let o = Orientation {
                    steps,
                    flip_horizontal: flips & 1 == 1,
                    flip_vertical: flips & 2 == 2,
                };
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
        assert!(!sky_is_up(
            &top,
            Orientation {
                steps: 2,
                ..Default::default()
            }
        ));
        // A vertical flip alone also puts it at the bottom.
        assert!(!sky_is_up(
            &top,
            Orientation {
                flip_vertical: true,
                ..Default::default()
            }
        ));
        // Stored sideways (sky on the left), displayed rotated 90° clockwise:
        // the left edge becomes the top.
        let left = map(10, 20, |x, _| if x < 4 { 0.9 } else { 0.1 });
        assert!(sky_is_up(
            &left,
            Orientation {
                steps: 1,
                ..Default::default()
            }
        ));
        assert!(!sky_is_up(
            &left,
            Orientation {
                steps: 3,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn border_count_separates_objects_from_backgrounds() {
        let object = GrayImage::from_fn(100, 100, |x, y| {
            Luma([if (30..70).contains(&x) && y >= 30 {
                255
            } else {
                0
            }])
        });
        assert_eq!(borders_touched(&object), 1);
        let water = GrayImage::from_fn(100, 100, |_, y| Luma([if y < 60 { 255 } else { 0 }]));
        assert_eq!(borders_touched(&water), 3);
        let ground = GrayImage::from_fn(100, 100, |x, y| {
            Luma([if y > 60 && (x > 10 || y > 80) { 255 } else { 0 }])
        });
        let shares = border_shares(&ground);
        assert!(shares[1] > 0.95 && shares[0] == 0.0, "{shares:?}");
        assert_eq!(
            map_agreement(&map(4, 4, |_, _| 0.9), &map(4, 4, |_, _| 0.9)),
            1.0
        );
        assert_eq!(
            map_agreement(&map(4, 4, |_, _| 0.9), &map(4, 4, |_, _| 0.1)),
            0.0
        );
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
                    let (crw, crh) = if steps % 2 == 1 {
                        (ih as f64, iw as f64)
                    } else {
                        (iw as f64, ih as f64)
                    };
                    let c = (crw / 2.0, crh / 2.0);
                    let a = (rot as f64).to_radians();
                    let (px, py) = (d.0 - c.0, d.1 - c.1);
                    let (ux, uy) = (
                        px * a.cos() + py * a.sin() + c.0,
                        -px * a.sin() + py * a.cos() + c.1,
                    );
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
                    assert!(
                        (back.0 - p.0).abs() < 1e-6 && (back.1 - p.1).abs() < 1e-6,
                        "{o:?} rot {rot}: {back:?}"
                    );
                }
            }
        }
    }

    /// A white sky over dark land, with a coarse mask whose edge is 20 px
    /// too low: colour must pull it onto the real boundary.
    #[test]
    fn sky_refine_snaps_the_edge_to_the_colour_boundary() {
        let (w, h) = (256u32, 160u32);
        let guide = RgbImage::from_fn(w, h, |_, y| {
            if y < 80 {
                Rgb([245, 245, 250])
            } else {
                Rgb([40, 45, 40])
            }
        });
        let coarse = map(32, 20, |_, y| {
            let fy = (y as f32 + 0.5) * 8.0;
            1.0 - smoothstep(80.0, 120.0, fy)
        });
        let out = sky_refine(&coarse, &guide);
        let col = |y: u32| out.get_pixel(128, y)[0] as f32 / 255.0;
        let p = resample(coarse.as_raw(), 32, 20, 256, 160);
        let coarse_at = |y: usize| p[y * 256 + 128];
        assert!(col(10) > 0.94, "sky interior lost: {}", col(10));
        assert!(col(150) < 0.06, "land interior selected: {}", col(150));
        // Below the true boundary the model still claimed sky; colour must
        // push it down (the ±0.35 band bounds how far a single pass may go).
        assert!(
            col(90) < coarse_at(90) - 0.3,
            "edge not pulled up: {} vs {}",
            col(90),
            coarse_at(90)
        );
        assert!(
            col(95) < coarse_at(95) - 0.3,
            "edge not pulled up: {} vs {}",
            col(95),
            coarse_at(95)
        );
        assert!(col(70) > 0.9, "sky above the boundary lost: {}", col(70));
    }

    /// Branches against a bright sky: the model paints the whole crown as
    /// one object, so the sky showing between the branches is labelled
    /// not-sky. Colour has to recover those gaps without selecting the
    /// branches themselves.
    #[test]
    fn sky_refine_recovers_gaps_between_branches() {
        let (w, h) = (256u32, 256u32);
        let branch = |x: u32, y: u32| y >= 96 && (x / 4).is_multiple_of(3);
        let guide = RgbImage::from_fn(w, h, |x, y| {
            if branch(x, y) {
                Rgb([25, 30, 25])
            } else {
                Rgb([240, 240, 245])
            }
        });
        // The model: sky above the crown, nothing inside it.
        let coarse = map(32, 32, |_, y| if y < 12 { 0.99 } else { 0.0 });
        let out = sky_refine(&coarse, &guide);
        let at = |x: u32, y: u32| out.get_pixel(x, y)[0];
        // A gap between branches, well inside the crown.
        assert!(
            at(6, 160) > 180,
            "gap between branches not recovered: {}",
            at(6, 160)
        );
        // A branch itself stays out.
        assert!(at(1, 160) < 60, "branch selected as sky: {}", at(1, 160));
        assert!(at(128, 20) > 240, "open sky lost: {}", at(128, 20));
    }

    /// Haze: sky and land are nearly the same colour, so colour cannot
    /// decide. The result must stay close to the model's own map rather
    /// than invent an edge.
    #[test]
    fn sky_refine_defers_to_the_model_when_colours_match() {
        let (w, h) = (256u32, 160u32);
        let guide = RgbImage::from_fn(w, h, |_, y| {
            if y < 80 {
                Rgb([200, 200, 200])
            } else {
                Rgb([197, 198, 197])
            }
        });
        let coarse = map(32, 20, |_, y| {
            let fy = (y as f32 + 0.5) * 8.0;
            1.0 - smoothstep(60.0, 100.0, fy)
        });
        let out = sky_refine(&coarse, &guide);
        let p = resample(coarse.as_raw(), 32, 20, w as usize, h as usize);
        for (i, px) in out.pixels().enumerate() {
            let diff = (px[0] as f32 / 255.0 - p[i]).abs();
            assert!(diff <= 0.36, "moved {diff:.2} with no colour evidence");
        }
    }

    #[test]
    fn birefnet_subject_keeps_portraits_and_declines_whole_frames() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_fn(200, 200, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        }));
        // A close portrait: 60% of the frame, touching left, right, bottom.
        let portrait = map(100, 100, |x, y| {
            if (15..85).contains(&x) && y > 20 {
                0.99
            } else {
                0.01
            }
        });
        let got = auto_subject_from_birefnet(&portrait, &image).expect("portrait kept");
        assert!(got.scene.coverage > 0.5, "{}", got.scene.coverage);
        assert_eq!(
            got.points.len(),
            2,
            "one component should give a box prompt"
        );
        // A wall: every border, nearly the whole frame.
        let wall = map(100, 100, |x, y| {
            if (1..99).contains(&x) && (1..99).contains(&y) {
                0.99
            } else {
                0.4
            }
        });
        assert!(auto_subject_from_birefnet(&wall, &image).is_none());
        // Nothing salient.
        assert!(auto_subject_from_birefnet(&map(100, 100, |_, _| 0.05), &image).is_none());
    }

    #[test]
    fn birefnet_subject_drops_specks_and_keeps_peers() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(400, 400, Rgb([90, 90, 90])));
        let probs = map(100, 100, |x, y| {
            let a = (10..40).contains(&x) && (10..40).contains(&y);
            let b = (60..85).contains(&x) && (60..85).contains(&y);
            let speck = x == 95 && y == 5;
            if a || b || speck { 0.95 } else { 0.02 }
        });
        let got = auto_subject_from_birefnet(&probs, &image).expect("two subjects");
        assert_eq!(got.points.len(), 2, "two components give one point each");
        assert!(got.points.iter().all(|p| p.label == 1));
        // The speck must not survive into the mask.
        assert_eq!(got.scene.mask.get_pixel(382, 22)[0], 0);
    }

    #[test]
    fn otsu_splits_a_bimodal_depth_and_flags_a_flat_one() {
        let bimodal: Vec<f32> = (0..1000)
            .map(|i| if i % 3 == 0 { 0.2 } else { 0.8 })
            .collect();
        let (t, sep) = otsu(&bimodal);
        assert!(t > 0.2 && t < 0.8, "threshold {t}");
        assert!(sep > 0.95, "separability {sep}");
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let (_, sep_ramp) = otsu(&ramp);
        assert!(
            sep_ramp < 0.8,
            "a uniform ramp is not a clean split: {sep_ramp}"
        );
        let flat = vec![0.4f32; 500];
        assert_eq!(otsu(&flat).1, 0.0);
    }

    /// A coarse, blurry mask edge that lies a few pixels off the true colour
    /// edge must be pulled onto it by the guide.
    #[test]
    fn guided_upsample_snaps_to_the_guide_edge() {
        let (w, h) = (256u32, 128u32);
        let guide = RgbImage::from_fn(w, h, |x, _| {
            if x < 140 {
                Rgb([200, 200, 200])
            } else {
                Rgb([30, 30, 30])
            }
        });
        // Coarse map: 32×16, soft transition centred at x≈120 in full-res
        // terms (i.e. 20 px left of the true edge).
        let coarse = map(32, 16, |x, _| {
            let fx = (x as f32 + 0.5) * 8.0;
            1.0 - smoothstep(100.0, 140.0, fx)
        });
        let mask = guided_upsample(&coarse, &guide, 0.35);
        let row = |x: u32| mask.get_pixel(x, 64)[0];
        assert!(
            row(20) > 240 && row(240) < 15,
            "interiors preserved: {} {}",
            row(20),
            row(240)
        );
        // Inside the coarse transition band the guide must decide: bright
        // side high, dark side low, with the steep change at the true edge.
        assert!(
            row(130) > row(150) + 60,
            "edge not snapped: {} vs {}",
            row(130),
            row(150)
        );
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

    /// Three depth layers: background (far), subject layer, near ground.
    fn layered_scene() -> (ProbabilityMap, GrayImage, RgbImage) {
        let depth = map(32, 32, |_, y| match y {
            0..=9 => 0.1,
            10..=19 => 0.5,
            _ => 0.9,
        });
        // Subject: a block in the middle layer, full resolution 128x128.
        let subject = GrayImage::from_fn(128, 128, |x, y| {
            Luma([if (48..80).contains(&x) && (40..80).contains(&y) {
                255
            } else {
                0
            }])
        });
        (
            depth,
            subject,
            RgbImage::from_pixel(128, 128, Rgb([100, 100, 100])),
        )
    }

    #[test]
    fn foreground_is_what_lies_in_front_of_the_subject() {
        let (depth, subject, guide) = layered_scene();
        let fg = foreground_from_subject(&depth, &subject, &guide).expect("foreground");
        // Only the near layer (rows 20..32 of 32) counts; background and the
        // subject's own layer do not.
        assert!((fg.coverage - 12.0 / 32.0).abs() < 0.05, "{}", fg.coverage);
        assert!(fg.mask.get_pixel(10, 10)[0] < 10, "background selected");
        assert!(
            fg.mask.get_pixel(10, 60)[0] < 10,
            "subject's depth layer selected"
        );
        assert!(fg.mask.get_pixel(10, 120)[0] > 245, "near ground missed");
    }

    #[test]
    fn foreground_never_overlaps_the_subject() {
        let (depth, _, guide) = layered_scene();
        // A subject whose legs reach down into the near layer.
        let subject = GrayImage::from_fn(128, 128, |x, y| {
            Luma([if (48..80).contains(&x) && (40..110).contains(&y) {
                255
            } else {
                0
            }])
        });
        let fg = foreground_from_subject(&depth, &subject, &guide).expect("foreground");
        for (m, s) in fg.mask.pixels().zip(subject.pixels()) {
            assert!(m[0] as u16 + s[0] as u16 <= 255);
        }
        assert_eq!(fg.mask.get_pixel(64, 100)[0], 0);
    }

    #[test]
    fn foreground_declines_without_a_subject_or_anything_in_front() {
        let (depth, _, guide) = layered_scene();
        let none = GrayImage::new(128, 128);
        assert_eq!(
            foreground_from_subject(&depth, &none, &guide).err(),
            Some(ForegroundDecline::NoSubject)
        );
        // Subject on the nearest layer: nothing can be in front of it.
        let nearest = GrayImage::from_fn(128, 128, |x, y| {
            Luma([if (48..80).contains(&x) && y >= 90 {
                255
            } else {
                0
            }])
        });
        assert_eq!(
            foreground_from_subject(&depth, &nearest, &guide).err(),
            Some(ForegroundDecline::NothingInFront)
        );
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
