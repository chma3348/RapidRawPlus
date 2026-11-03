//! Interactive SAM selection. Keep model logits in the padded model grid;
//! saved image masks are display artifacts, never decoder feedback.
use std::sync::Mutex;

use anyhow::{Result, ensure};
use image::{GrayImage, RgbImage};
use ndarray::Array;
use ort::{session::Session, value::Tensor};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ai_processing::ImageEmbeddings;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubjectPoint {
    pub x: f64,
    pub y: f64,
    pub label: u8,
}

pub fn box_or_point(start: (f64, f64), end: (f64, f64)) -> Vec<SubjectPoint> {
    if (start.0 - end.0).abs() < 1e-6 && (start.1 - end.1).abs() < 1e-6 {
        vec![SubjectPoint {
            x: start.0,
            y: start.1,
            label: 1,
        }]
    } else {
        vec![
            SubjectPoint {
                x: start.0.min(end.0),
                y: start.1.min(end.1),
                label: 2,
            },
            SubjectPoint {
                x: start.0.max(end.0),
                y: start.1.max(end.1),
                label: 3,
            },
        ]
    }
}

/// Select the most confident prompt-consistent hypothesis. For ambiguous first
/// clicks, prefer the larger of similarly confident masks (whole object).
fn choose_candidate(
    masks: &[f32],
    scores: &[f32],
    side: usize,
    points: &[SubjectPoint],
    scale: f64,
    whole: bool,
    valid: usize,
) -> usize {
    let area = side * side;
    // The containment rule below is for the one genuinely ambiguous case: a
    // single positive click. With a negative point or several positives the
    // prompt itself disambiguates, and the original near-tie preference for
    // the larger mask is the right amount of bias (dropping it entirely made
    // an exclude click collapse a diver to a hood).
    let hierarchy = whole && points.len() == 1;
    let mut ranked = Vec::new();
    for (i, mask) in masks.chunks_exact(area).enumerate() {
        // SAM's first token is the dedicated multi-prompt mask, not one of
        // the three ambiguous-click hypotheses. Do not rank it on a first click.
        if scores.len() == 4 && whole && points.len() == 1 && i == 0 {
            continue;
        }
        let mut score = scores
            .get(i)
            .copied()
            .filter(|s| s.is_finite())
            .unwrap_or(0.0);
        // Stability (share of the mask's support that is confidently in)
        // used to be a score penalty. That is systematically anti-whole-
        // object: a full body has proportionally far more soft edge than a
        // hood, so it lost 0.07-0.10 where the fragment lost 0.01, and that
        // alone pushed a coral head (raw IoU gap 0.175) and a diver (0.182)
        // out of the band. It is kept only as a FLOOR on the larger
        // candidate, which is what actually screens out a threshold-
        // sensitive spill (whose stability is ~0; real objects measured
        // 0.29-0.53).
        let stable = mask.iter().filter(|&&v| v > 1.0).count();
        let possible = mask.iter().filter(|&&v| v > -1.0).count().max(1);
        let stability = stable as f32 / possible as f32;
        for p in points.iter().filter(|p| p.label <= 1) {
            let x = ((p.x * scale * side as f64 / 1024.0) as usize).min(side - 1);
            let y = ((p.y * scale * side as f64 / 1024.0) as usize).min(side - 1);
            if (mask[y * side + x] > 0.0) != (p.label == 1) {
                score -= 1.0;
            }
        }
        ranked.push((
            i,
            score,
            mask.iter().filter(|&&v| v > 0.0).count(),
            stability,
        ));
    }
    let best_score = ranked.iter().map(|r| r.1).fold(f32::NEG_INFINITY, f32::max);
    // A larger mask may only win over a better-scoring one if the model is
    // committed to it, i.e. it is not a barely-positive spill. Stability is
    // the discriminator; the logit AT the click is deliberately not a floor
    // -- a click on a soft spot of a real object (a wetsuit leg measured
    // 0.52) must still count, and prompt consistency is enforced above.
    let committed = |r: &(usize, f32, usize, f32)| r.3 >= 0.2;
    if whole && !hierarchy {
        let top = ranked
            .iter()
            .filter(|r| committed(r))
            .chain(ranked.iter())
            .max_by(|a, b| {
                // Committed candidates rank first; among equals, raw score.
                (committed(a), a.1)
                    .partial_cmp(&(committed(b), b.1))
                    .unwrap()
            })
            .map(|r| r.0)
            .unwrap_or(0);
        ranked
            .iter()
            .filter(|r| r.1 >= best_score - 0.04 && (r.0 == top || committed(r)))
            .max_by_key(|r| r.2)
            .map(|r| r.0)
            .unwrap_or(0)
    } else if hierarchy {
        // SAM's three single-click hypotheses form a hierarchy: sub-part,
        // part, whole. Its predicted IoU rewards the tight, confident part,
        // so the highest score is routinely a hood or a vest while the whole
        // person sits one candidate over with a slightly lower score. Measured
        // on four different clicks on one diver, the whole-body hypothesis was
        // present every time (20-22% of frame, IoU 0.67-0.76) and a 0.04 tie
        // band picked a fragment in three of the four.
        //
        // So decide by CONTAINMENT, not by a score tie: take the largest
        // prompt-consistent candidate that is a strict superset of the top
        // scorer (>= 85% of it inside) and not merely a marginally bigger
        // version of it. Containment is what distinguishes "the whole of this
        // part" from a large unrelated blob that happens to score well.
        let top = ranked
            .iter()
            .filter(|r| committed(r))
            .chain(ranked.iter())
            .max_by(|a, b| {
                // Committed candidates rank first; among equals, raw score.
                (committed(a), a.1)
                    .partial_cmp(&(committed(b), b.1))
                    .unwrap()
            })
            .map(|r| r.0)
            .unwrap_or(0);
        let top_mask = &masks[top * area..(top + 1) * area];
        let top_area = ranked.iter().find(|r| r.0 == top).map(|r| r.2).unwrap_or(0);
        // `valid` is the grid area covering the actual image (the rest of
        // the 256-square is padding); a "whole object" covering nearly all
        // of it is the background, not a subject.
        ranked
            .iter()
            .filter(|r| r.1 >= best_score - 0.25 && r.1 > best_score - 1.0)
            .filter(|r| r.2 >= top_area * 3 / 2 && r.2 < valid * 9 / 10)
            .filter(|r| committed(r))
            .filter(|r| {
                let m = &masks[r.0 * area..(r.0 + 1) * area];
                let inside = m
                    .iter()
                    .zip(top_mask)
                    .filter(|(a, b)| **a > 0.0 && **b > 0.0)
                    .count();
                inside as f32 / top_area.max(1) as f32 >= 0.85
            })
            .max_by_key(|r| r.2)
            .map(|r| r.0)
            .unwrap_or(top)
    } else {
        ranked
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|r| r.0)
            .unwrap_or(0)
    }
}

/// Remove stray islands, but keep every explicitly included component. Skip
/// boxes: a bounding box may legitimately contain disconnected subject parts.
fn clean_islands(logits: &mut [f32], side: usize, points: &[SubjectPoint], scale: f64) {
    if points.iter().any(|p| p.label >= 2) {
        return;
    }
    let mut seen = vec![false; logits.len()];
    let mut seeds = vec![false; logits.len()];
    for p in points.iter().filter(|p| p.label == 1) {
        let x = ((p.x * scale * side as f64 / 1024.0) as usize).min(side - 1);
        let y = ((p.y * scale * side as f64 / 1024.0) as usize).min(side - 1);
        // A click can fall between coarse pixels. Use a small neighborhood.
        for sy in y.saturating_sub(1)..=(y + 1).min(side - 1) {
            for sx in x.saturating_sub(1)..=(x + 1).min(side - 1) {
                seeds[sy * side + sx] = true;
            }
        }
    }
    let mut components = Vec::new();
    for index in 0..logits.len() {
        if seen[index] || logits[index] <= 0.0 {
            continue;
        }
        let mut pixels = vec![index];
        seen[index] = true;
        let mut seeded = false;
        let mut cursor = 0;
        while cursor < pixels.len() {
            let p = pixels[cursor];
            cursor += 1;
            seeded |= seeds[p];
            let (x, y) = (p % side, p / side);
            for ny in y.saturating_sub(1)..=(y + 1).min(side - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(side - 1) {
                    let q = ny * side + nx;
                    if !seen[q] && logits[q] > 0.0 {
                        seen[q] = true;
                        pixels.push(q);
                    }
                }
            }
        }
        components.push((seeded, pixels));
    }
    // Do not destroy a result when no positive seed hits it. Remove only small
    // unseeded islands, preserving occluded limbs and separate hair strands.
    let largest = components.iter().map(|c| c.1.len()).max().unwrap_or(0);
    if components.iter().any(|c| c.0) {
        for (seeded, pixels) in components {
            if !seeded && pixels.len() < (largest / 50).max(4) {
                for p in pixels {
                    logits[p] = -12.0;
                }
            }
        }
    }
}

/// Pixel-center interpolation; color guidance is applied over overlapping
/// image neighborhoods below, never from isolated coarse-grid RGB samples.
fn render_coarse_matte(logits: &[f32], side: usize, guide: &RgbImage) -> GrayImage {
    let (w, h) = guide.dimensions();
    let scale = side as f32 / w.max(h) as f32;
    let valid_w = ((w as f32 * scale).round() as usize).clamp(1, side);
    let valid_h = ((h as f32 * scale).round() as usize).clamp(1, side);
    let mut pixels = vec![0u8; (w as usize) * (h as usize)];
    pixels
        .par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let sy = ((y as f32 + 0.5) * scale - 0.5).clamp(0.0, (valid_h - 1) as f32);
            let y0 = sy.floor() as usize;
            let y1 = (y0 + 1).min(valid_h - 1);
            let fy = sy - y0 as f32;
            for (x, out) in row.iter_mut().enumerate() {
                let sx = ((x as f32 + 0.5) * scale - 0.5).clamp(0.0, (valid_w - 1) as f32);
                let x0 = sx.floor() as usize;
                let x1 = (x0 + 1).min(valid_w - 1);
                let fx = sx - x0 as f32;
                let ids = [
                    y0 * side + x0,
                    y0 * side + x1,
                    y1 * side + x0,
                    y1 * side + x1,
                ];
                let weights = [
                    (1.0 - fx) * (1.0 - fy),
                    fx * (1.0 - fy),
                    (1.0 - fx) * fy,
                    fx * fy,
                ];
                let plain: f32 = ids.iter().zip(weights).map(|(&i, a)| logits[i] * a).sum();
                // SAM logits are not an alpha matte. Suppress their low-confidence
                // tails so strong color corrections do not tint distant background,
                // while retaining continuous coverage around the actual boundary.
                let probability = 1.0 / (1.0 + (-plain.clamp(-20.0, 20.0)).exp());
                *out = (((probability - 0.02) / 0.96).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
    GrayImage::from_raw(w, h, pixels).expect("matte dimensions")
}

/// Separable box mean with clipped windows (no dark padding at photo edges).
pub(crate) fn box_mean(values: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let mut horizontal = vec![0.0; values.len()];
    for y in 0..h {
        let mut sum = 0.0f64;
        let mut end = 0;
        let mut start = 0;
        for x in 0..w {
            let right = (x + radius + 1).min(w);
            while end < right {
                sum += values[y * w + end] as f64;
                end += 1;
            }
            let left = x.saturating_sub(radius);
            while start < left {
                sum -= values[y * w + start] as f64;
                start += 1;
            }
            horizontal[y * w + x] = (sum / (end - start) as f64) as f32;
        }
    }
    let mut result = vec![0.0; values.len()];
    for x in 0..w {
        let mut sum = 0.0f64;
        let mut end = 0;
        let mut start = 0;
        for y in 0..h {
            let bottom = (y + radius + 1).min(h);
            while end < bottom {
                sum += horizontal[end * w + x] as f64;
                end += 1;
            }
            let top = y.saturating_sub(radius);
            while start < top {
                sum -= horizontal[start * w + x] as f64;
                start += 1;
            }
            result[y * w + x] = (sum / (end - start) as f64) as f32;
        }
    }
    result
}

/// Fit alpha = a·RGB + b in overlapping image neighborhoods, then interpolate
/// the averaged coefficients to the original photo. Statistics are bounded to
/// 1024px; full-resolution color places edges without copying coarse cell texture.
fn render_matte(logits: &[f32], side: usize, guide: &RgbImage) -> GrayImage {
    use image::imageops::{FilterType, resize};
    use nalgebra::{Matrix3, Vector3};
    let (width, height) = guide.dimensions();
    let scale = (1024.0 / width.max(height) as f64).min(1.0);
    let w = (width as f64 * scale).round().max(1.0) as usize;
    let h = (height as f64 * scale).round().max(1.0) as usize;
    let small = resize(guide, w as u32, h as u32, FilterType::Triangle);
    let base = render_coarse_matte(logits, side, &small);
    let p: Vec<f32> = base.pixels().map(|p| p[0] as f32 / 255.0).collect();
    let rgb: [Vec<f32>; 3] =
        std::array::from_fn(|c| small.pixels().map(|p| p[c] as f32 / 255.0).collect());
    let radius = ((w.max(h) as f32 / side as f32) * 2.0).ceil().max(1.0) as usize;
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
            // Regularization prevents texture/noise from masquerading as a contour.
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
    let mut result = vec![0; width as usize * height as usize];
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
                // Only adjust the boundary band; never invent selection in distant
                // background or punch texture into confidently selected interiors.
                let alpha = if original <= 0.0 || original >= 1.0 {
                    original
                } else {
                    guided.clamp(original - 0.25, original + 0.25)
                };
                *out = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
    GrayImage::from_raw(width, height, result).expect("matte dimensions")
}

/// SAM predicts membership rather than physical transparency. Keep uncertainty
/// around the contour, but make unanimous 3×3 interior/background neighborhoods
/// solid so low probability tails cannot color distant, unrelated objects.
fn stabilize_interiors(logits: &mut [f32], side: usize) {
    let source = logits.to_vec();
    for y in 0..side {
        for x in 0..side {
            let mut inside = true;
            let mut outside = true;
            for ny in y.saturating_sub(1)..=(y + 1).min(side - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(side - 1) {
                    inside &= source[ny * side + nx] > 0.0;
                    outside &= source[ny * side + nx] < 0.0;
                }
            }
            let v = &mut logits[y * side + x];
            if inside {
                *v = v.max(4.0);
            }
            if outside {
                *v = v.min(-4.0);
            }
        }
    }
}

fn mask_iou(a: &[f32], b: &[f32]) -> f32 {
    let intersection = a
        .iter()
        .zip(b)
        .filter(|(x, y)| **x > 0.0 && **y > 0.0)
        .count();
    let union = a
        .iter()
        .zip(b)
        .filter(|(x, y)| **x > 0.0 || **y > 0.0)
        .count()
        .max(1);
    intersection as f32 / union as f32
}

/// Bounding box, in SAM's 1024 prompt space, of the model's low-confidence
/// support connected to the positive clicks. On the first pass every
/// single-click hypothesis contributes (the whole-object shape is often only
/// in one of them, as a faint ghost); once a box is in play only the chosen
/// mask does, so the region cannot wander. Padded 5%. None when there is
/// nothing beyond what a point prompt already covers.
fn ghost_box(
    hypotheses: &[f32],
    chosen: &[f32],
    side: usize,
    points: &[SubjectPoint],
    scale: f64,
    boxed: bool,
    single: bool,
) -> Option<[f32; 4]> {
    let area = side * side;
    // logit > -1.386  <=>  probability > 0.2
    let mut support = vec![false; area];
    // Only a lone click is ambiguous enough to pool every hypothesis; once
    // the user has added a negative or a second positive, the other
    // hypotheses describe objects the prompt has already ruled out.
    if boxed || !single {
        for (k, &v) in chosen.iter().enumerate() {
            support[k] = v > -1.386;
        }
    } else {
        for (i, m) in hypotheses.chunks_exact(area).enumerate() {
            if i == 0 && hypotheses.len() / area == 4 {
                continue;
            }
            for (k, &v) in m.iter().enumerate() {
                support[k] |= v > -1.386;
            }
        }
    }
    let mut seen = vec![false; area];
    let mut stack: Vec<usize> = points
        .iter()
        .filter(|p| p.label == 1)
        .map(|p| {
            let x = ((p.x * scale * side as f64 / 1024.0) as usize).min(side - 1);
            let y = ((p.y * scale * side as f64 / 1024.0) as usize).min(side - 1);
            y * side + x
        })
        .collect();
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (side, side, 0, 0, 0usize);
    while let Some(k) = stack.pop() {
        if seen[k] || !support[k] {
            continue;
        }
        seen[k] = true;
        n += 1;
        let (x, y) = (k % side, k / side);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
        if x > 0 {
            stack.push(k - 1);
        }
        if x + 1 < side {
            stack.push(k + 1);
        }
        if y > 0 {
            stack.push(k - side);
        }
        if y + 1 < side {
            stack.push(k + side);
        }
    }
    if n < 4 {
        return None;
    }
    let cell = 1024.0 / side as f32;
    let (bx0, by0) = (x0 as f32 * cell, y0 as f32 * cell);
    let (bx1, by1) = ((x1 + 1) as f32 * cell, (y1 + 1) as f32 * cell);
    let (pw, ph) = ((bx1 - bx0) * 0.05, (by1 - by0) * 0.05);
    Some([
        (bx0 - pw).max(0.0),
        (by0 - ph).max(0.0),
        (bx1 + pw).min(1024.0),
        (by1 + ph).min(1024.0),
    ])
}

/// Growth may add to the selection but must not swap it for something else:
/// it keeps what was already selected, it never contradicts a prompt it
/// previously satisfied, and the model must not become markedly less sure.
fn growth_accepted(
    before: &[f32],
    after: &[f32],
    old_score: f32,
    new_score: f32,
    side: usize,
    points: &[SubjectPoint],
    scale: f64,
) -> bool {
    let errors = |mask: &[f32]| {
        points
            .iter()
            .filter(|p| p.label <= 1)
            .filter(|p| {
                let x = ((p.x * scale * side as f64 / 1024.0) as usize).min(side - 1);
                let y = ((p.y * scale * side as f64 / 1024.0) as usize).min(side - 1);
                (mask[y * side + x] > 0.0) != (p.label == 1)
            })
            .count()
    };
    if errors(after) > errors(before) {
        return false;
    }
    let kept = before
        .iter()
        .zip(after)
        .filter(|(b, a)| **b > 0.0 && **a > 0.0)
        .count();
    let had = before.iter().filter(|&&v| v > 0.0).count().max(1);
    new_score.is_finite() && new_score >= old_score - 0.05 && kept as f32 / had as f32 >= 0.9
}

pub fn select(
    decoder: &Mutex<Session>,
    embeddings: &ImageEmbeddings,
    points: &[SubjectPoint],
    _selection_id: Option<&str>,
    whole: bool,
) -> Result<GrayImage> {
    select_grown(decoder, embeddings, points, whole, true)
}

/// `grow` enables the click path's automatic growth (below); painted
/// selections leave it off, because the paint already gives the extent.
fn select_grown(
    decoder: &Mutex<Session>,
    embeddings: &ImageEmbeddings,
    points: &[SubjectPoint],
    whole: bool,
    grow: bool,
) -> Result<GrayImage> {
    ensure!(
        !points.is_empty() && points.len() <= 256,
        "Use between 1 and 256 selection points."
    );
    let (w, h) = embeddings.original_size;
    ensure!(
        w > 0 && h > 0 && embeddings.guide.dimensions() == (w, h),
        "Invalid subject image dimensions."
    );
    ensure!(
        points.iter().enumerate().all(|(i, p)| match p.label {
            2 => points
                .get(i + 1)
                .is_some_and(|q| q.label == 3 && q.x >= p.x && q.y >= p.y),
            3 => i > 0 && points[i - 1].label == 2,
            _ => true,
        }),
        "A selection box requires two ordered corners."
    );
    ensure!(
        points.iter().all(|p| p.x.is_finite()
            && p.y.is_finite()
            && p.x >= 0.0
            && p.y >= 0.0
            && p.x <= w as f64
            && p.y <= h as f64
            && p.label <= 3),
        "Selection points are outside the photo."
    );
    ensure!(
        points.iter().any(|p| p.label == 1 || p.label == 2),
        "Include part of the subject first."
    );
    // Start from the full prompt set every time. A historical prior can lock
    // in mistakes and makes identical prompts depend on undo/request timing.
    // The optional second pass below only refines this request's own prediction.
    let mut prior = None;
    let scale = 1024.0 / w.max(h) as f64;
    let mut coords: Vec<f32> = points
        .iter()
        .flat_map(|p| [(p.x * scale) as f32, (p.y * scale) as f32])
        .collect();
    let mut labels: Vec<f32> = points.iter().map(|p| p.label as f32).collect();
    // Official point-only ONNX prompt convention. Boxes already provide both corners.
    if !points.iter().any(|p| p.label >= 2) {
        coords.extend([0.0, 0.0]);
        labels.push(-1.0);
    }
    let mut logits = Vec::new();
    let mut side = 256;
    let mut quality = f32::NEG_INFINITY;
    // Growth. A lone click on a person routinely yields a fragment -- SAM
    // ViT-B proposes a vest, not a diver -- yet the rest of the body is
    // usually present in the same logits at low confidence. Each pass takes
    // that low-confidence region connected to the click, turns its padded
    // bounding box into a box prompt, feeds the previous logits back, and
    // decodes again. The box tells SAM the object's extent; the prior tells
    // it which object. Measured: a torso click that produced 0.3% of the
    // frame converges to the full diver (21.5%, IoU 0.98) in three passes,
    // and a case where only the vest was ever proposed doubles its coverage
    // without touching the background, because growth is confined to the
    // model's own connected low-confidence support. It is the loop the
    // reference implementation runs, minus the peak point (the box alone
    // converged on every test case).
    let user_box = points.iter().any(|p| p.label >= 2);
    let mut grown_box: Option<[f32; 4]> = None;
    for pass in 0..4 {
        let mut session = decoder.lock().unwrap();
        let has_prior = prior.is_some();
        let input_mask = prior.take().unwrap_or_else(|| vec![0.0; 256 * 256]);
        let mut pass_coords = coords.clone();
        let mut pass_labels = labels.clone();
        if let Some(b) = grown_box {
            // The point-only convention pads with a (0,0,-1) entry; replace
            // it with the grown box so the prompt stays well-formed.
            if pass_labels.last() == Some(&-1.0) {
                pass_labels.pop();
                pass_coords.truncate(pass_coords.len() - 2);
            }
            pass_coords.extend(b);
            pass_labels.extend([2.0, 3.0]);
        }
        let outputs = session.run(ort::inputs![
            Tensor::from_array(embeddings.embeddings.clone())?,
            Tensor::from_array(Array::from_shape_vec(
                (1, pass_labels.len(), 2),
                pass_coords.clone()
            )?)?,
            Tensor::from_array(Array::from_shape_vec(
                (1, pass_labels.len()),
                pass_labels.clone()
            )?)?,
            Tensor::from_array(Array::from_shape_vec((1, 1, 256, 256), input_mask)?)?,
            Tensor::from_array(Array::from_vec(vec![if has_prior { 1.0f32 } else { 0.0 }]))?,
            Tensor::from_array(Array::from_vec(vec![h as f32, w as f32]))?,
        ])?;
        ensure!(
            outputs.len() >= 3,
            "This subject model must provide masks, quality scores, and low-resolution logits."
        );
        let low = outputs[2].try_extract_array::<f32>()?;
        let shape = low.shape();
        ensure!(
            shape.len() == 4 && shape[0] == 1 && shape[2] == 256 && shape[3] == 256,
            "Unsupported subject model mask grid; expected 256 × 256 logits."
        );
        side = shape[2];
        let data: Vec<f32> = low.iter().copied().collect();
        ensure!(
            !data.is_empty() && data.iter().all(|v| v.is_finite()),
            "Subject model returned invalid mask values."
        );
        let scores: Vec<f32> = outputs[1]
            .try_extract_array::<f32>()?
            .iter()
            .copied()
            .collect();
        ensure!(
            scores.len() == shape[1],
            "Subject model quality scores do not match its masks."
        );
        // Once a box is in the prompt (the user's or a grown one), SAM's
        // first token is the prompt-consistent answer; the hierarchy rule
        // only applies to the ambiguous single-click pass.
        let ambiguous = whole && !has_prior && grown_box.is_none() && points.len() == 1;
        // Grid cells that cover the photo itself; the rest of the 256-square
        // is SAM's letterbox padding.
        let valid = (((w as f64 * scale) as usize * side / 1024).max(1))
            * (((h as f64 * scale) as usize * side / 1024).max(1));
        let best = choose_candidate(&data, &scores, side, points, scale, ambiguous, valid);
        let candidate = &data[best * side * side..(best + 1) * side * side];
        if pass > 0 {
            // Automatic growth is bounded: one pass may not more than 2.5x
            // the selection, and it may never take most of the frame. A
            // click on one of two touching divers once grew to 94% of the
            // image; a user who wants that much draws a box, which bypasses
            // growth entirely.
            let grew_to = candidate.iter().filter(|&&v| v > 0.0).count();
            let grew_from = logits.iter().filter(|&&v| v > 0.0).count().max(1);
            let within_bounds = grew_to <= grew_from * 5 / 2 && grew_to <= valid * 3 / 5;
            let accepted = if grown_box.is_some() {
                within_bounds
                    && growth_accepted(
                        &logits,
                        candidate,
                        quality,
                        scores[best],
                        side,
                        points,
                        scale,
                    )
            } else {
                accept_refinement(
                    &logits,
                    candidate,
                    quality,
                    scores[best],
                    side,
                    points,
                    scale,
                )
            };
            if !accepted {
                break;
            }
            let converged = mask_iou(&logits, candidate) > 0.99;
            logits = candidate.to_vec();
            quality = scores[best];
            if converged {
                break;
            }
        } else {
            logits = candidate.to_vec();
            quality = scores[best];
        }
        prior = Some(logits.clone());
        if grow && !user_box {
            // Grow from the model's own connected low-confidence support
            // around the clicks, never from the rendered mask.
            grown_box = ghost_box(
                &data,
                &logits,
                side,
                points,
                scale,
                grown_box.is_some(),
                points.len() == 1,
            );
        }
    }
    clean_islands(&mut logits, side, points, scale);
    stabilize_interiors(&mut logits, side);
    let mut matte = render_matte(&logits, side, &embeddings.guide);
    snap_to_edges(&mut matte, &embeddings.guide, w.max(h) as f32 / side as f32);
    Ok(matte)
}

/// `snap_to_edges` for a Subject matte made before it existed (saved in a
/// sidecar), with SAM's usual 256-cell grid. `guide` is the photo as the
/// Subject tool sees it, the same size as the matte.
pub fn snap_saved_matte(matte: &mut GrayImage, guide: &RgbImage) {
    snap_to_edges(
        matte,
        guide,
        matte.width().max(matte.height()) as f32 / 256.0,
    );
}

/// SAM places its boundary on a grid of `cell`-pixel cells (15–25 px on a
/// full-size photo), and `render_matte` aligns it to the photo only on a copy
/// of at most 1024 px. So an in-focus edge is left as a soft, stepped band
/// tens of pixels wide that doesn't sit on the outline, and a mask and its
/// inverse both half-apply across it. Re-fit that band to the photo at full
/// resolution with a colour guided filter: within each small window the
/// matte becomes a linear function of the pixel colours, which puts a sharp
/// edge exactly where the colours change, keeps hair and defocus partial,
/// and leaves flat areas flat. Only tiles along the band are computed.
fn snap_to_edges(matte: &mut GrayImage, guide: &RgbImage, cell: f32) {
    let (w, h) = (matte.width() as usize, matte.height() as usize);
    // The band: within a cell of anything SAM was unsure about.
    let radius = cell.round().max(2.0) as usize;
    let tile = 256usize;
    let (tw, th) = (w.div_ceil(tile), h.div_ceil(tile));
    let unsure = |x: usize, y: usize| {
        let a = matte.get_pixel(x as u32, y as u32)[0];
        a > 5 && a < 250
    };
    let tiles: Vec<(usize, usize)> = (0..tw * th)
        .into_par_iter()
        .map(|i| (i % tw, i / tw))
        .filter(|&(tx, ty)| {
            let (x0, y0) = (tx * tile, ty * tile);
            (y0..(y0 + tile).min(h)).any(|y| (x0..(x0 + tile).min(w)).any(|x| unsure(x, y)))
        })
        .collect();
    let fitted: Vec<((usize, usize), Vec<u8>)> = tiles
        .into_par_iter()
        .map(|(tx, ty)| {
            // The tile, with room around it for the windows and the band.
            let pad = radius * 3;
            let (x0, y0) = (
                (tx * tile).saturating_sub(pad),
                (ty * tile).saturating_sub(pad),
            );
            let (x1, y1) = (
                ((tx + 1) * tile + pad).min(w),
                ((ty + 1) * tile + pad).min(h),
            );
            let (rw, rh) = (x1 - x0, y1 - y0);
            let mut p = Vec::with_capacity(rw * rh);
            let mut rgb: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::with_capacity(rw * rh));
            for y in y0..y1 {
                for x in x0..x1 {
                    p.push(matte.get_pixel(x as u32, y as u32)[0] as f32 / 255.0);
                    let c = guide.get_pixel(x as u32, y as u32);
                    for k in 0..3 {
                        rgb[k].push(c[k] as f32 / 255.0);
                    }
                }
            }
            // The fit leaves each region of one colour at the share of it
            // SAM selected (an edge drawn 8 px off covers part of the
            // background). Pushing gently away from one half and refitting
            // settles a region mostly in the selection fully in, and one
            // mostly out fully out; a true half-cover (fine hair) stays.
            let mut q = guided_filter(&p, &rgb, rw, rh, radius, GUIDED_EPS);
            for _ in 0..SETTLE_ROUNDS {
                q.iter_mut()
                    .for_each(|v| *v = (0.5 + (*v - 0.5) * SETTLE_GAIN).clamp(0.0, 1.0));
                q = guided_filter(&q, &rgb, rw, rh, radius / 2, GUIDED_EPS);
            }
            // Near SAM's uncertainty, the fit; well inside either side, the
            // model (a flat area must not pick up the fit's small ripple).
            let near: Vec<f32> = p
                .iter()
                .map(|a| (*a > 0.02 && *a < 0.98) as u8 as f32)
                .collect();
            let near = box_mean(&near, rw, rh, radius);
            let (cx0, cy0) = (tx * tile, ty * tile);
            let (cx1, cy1) = ((cx0 + tile).min(w), (cy0 + tile).min(h));
            let mut out = Vec::with_capacity((cx1 - cx0) * (cy1 - cy0));
            for y in cy0..cy1 {
                for x in cx0..cx1 {
                    let i = (y - y0) * rw + (x - x0);
                    let weight = (near[i] * 4.0).min(1.0);
                    let alpha = p[i] + (q[i].clamp(0.0, 1.0) - p[i]) * weight;
                    out.push((alpha * 255.0).round() as u8);
                }
            }
            ((tx, ty), out)
        })
        .collect();
    for ((tx, ty), out) in fitted {
        let (cx0, cy0) = (tx * tile, ty * tile);
        let cw = (cx0 + tile).min(w) - cx0;
        for (i, v) in out.into_iter().enumerate() {
            matte.put_pixel(
                (cx0 + i % cw) as u32,
                (cy0 + i / cw) as u32,
                image::Luma([v]),
            );
        }
    }
}

/// How closely the full-resolution fit follows colour (0–1 RGB variance):
/// small enough that a real edge is sharp, large enough that noise and fine
/// texture inside the subject are not mistaken for edges.
const GUIDED_EPS: f32 = 2e-4;
/// Rounds and strength of the push that settles part-selected regions.
const SETTLE_ROUNDS: usize = 3;
const SETTLE_GAIN: f32 = 1.6;

/// He et al.'s guided filter with a colour guide: `p` re-expressed, window
/// by window, as a linear function of the guide's colours.
fn guided_filter(
    p: &[f32],
    rgb: &[Vec<f32>; 3],
    w: usize,
    h: usize,
    radius: usize,
    eps: f32,
) -> Vec<f32> {
    use nalgebra::{Matrix3, Vector3};
    let radius = radius.max(1);
    let mean = |v: &[f32]| box_mean(v, w, h, radius);
    let product = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| a * b).collect::<Vec<_>>();
    let mp = mean(p);
    let mc: [Vec<f32>; 3] = std::array::from_fn(|c| mean(&rgb[c]));
    let cov_p: [Vec<f32>; 3] = std::array::from_fn(|c| {
        mean(&product(&rgb[c], p))
            .iter()
            .zip(mc[c].iter().zip(&mp))
            .map(|(v, (a, b))| v - a * b)
            .collect()
    });
    let pairs = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
    let var: Vec<Vec<f32>> = pairs
        .iter()
        .map(|&(a, b)| {
            mean(&product(&rgb[a], &rgb[b]))
                .iter()
                .zip(mc[a].iter().zip(&mc[b]))
                .map(|(v, (x, y))| v - x * y)
                .collect()
        })
        .collect();
    let mut coefficients: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0; w * h]);
    for i in 0..w * h {
        let sigma = Matrix3::new(
            var[0][i] + eps,
            var[1][i],
            var[2][i],
            var[1][i],
            var[3][i] + eps,
            var[4][i],
            var[2][i],
            var[4][i],
            var[5][i] + eps,
        );
        let a = sigma
            .cholesky()
            .map(|m| m.solve(&Vector3::new(cov_p[0][i], cov_p[1][i], cov_p[2][i])))
            .unwrap_or_else(Vector3::zeros);
        coefficients[0][i] = a[0];
        coefficients[1][i] = a[1];
        coefficients[2][i] = a[2];
        coefficients[3][i] = mp[i] - (0..3).map(|c| a[c] * mc[c][i]).sum::<f32>();
    }
    let averaged: [Vec<f32>; 4] = std::array::from_fn(|k| mean(&coefficients[k]));
    (0..w * h)
        .map(|i| averaged[3][i] + (0..3).map(|c| averaged[c][i] * rgb[c][i]).sum::<f32>())
        .collect()
}

/// Self-feedback must not silently replace a good object with a different one.
fn accept_refinement(
    before: &[f32],
    after: &[f32],
    old_score: f32,
    new_score: f32,
    side: usize,
    points: &[SubjectPoint],
    scale: f64,
) -> bool {
    let errors = |mask: &[f32]| {
        points
            .iter()
            .filter(|p| p.label <= 1)
            .filter(|p| {
                let x = ((p.x * scale * side as f64 / 1024.0) as usize).min(side - 1);
                let y = ((p.y * scale * side as f64 / 1024.0) as usize).min(side - 1);
                (mask[y * side + x] > 0.0) != (p.label == 1)
            })
            .count()
    };
    let (old_errors, new_errors) = (errors(before), errors(after));
    if new_errors != old_errors {
        return new_errors < old_errors;
    }
    let intersection = before
        .iter()
        .zip(after)
        .filter(|(a, b)| **a > 0.0 && **b > 0.0)
        .count();
    let union = before
        .iter()
        .zip(after)
        .filter(|(a, b)| **a > 0.0 || **b > 0.0)
        .count()
        .max(1);
    new_score.is_finite()
        && new_score >= old_score - 0.02
        && intersection as f32 / union as f32 >= 0.85
}

/// Sample actual painted pixels (never an unpainted centroid), including
/// explicit negative strokes. Neutral unpainted space permits object completion.
pub fn paint_points(paint: &GrayImage, excluded: &GrayImage) -> Vec<SubjectPoint> {
    let mut points = Vec::new();
    for (image, label) in [(paint, 1), (excluded, 0)] {
        let stride = (image.width().max(image.height()) / 512).max(1) as usize;
        let mut candidates: Vec<_> = image
            .enumerate_pixels()
            .filter(|(x, y, p)| {
                p[0] > 127
                    && (*x as usize).is_multiple_of(stride)
                    && (*y as usize).is_multiple_of(stride)
            })
            .map(|(x, y, _)| (x as f64, y as f64))
            .collect();
        if candidates.is_empty()
            && let Some((x, y, _)) = image.enumerate_pixels().find(|(_, _, p)| p[0] > 127)
        {
            candidates.push((x as f64, y as f64));
        }
        if candidates.is_empty() {
            continue;
        }
        let center = candidates
            .iter()
            .fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
        let center = (
            center.0 / candidates.len() as f64,
            center.1 / candidates.len() as f64,
        );
        let distance = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2);
        let first = *candidates
            .iter()
            .min_by(|a, b| distance(**a, center).total_cmp(&distance(**b, center)))
            .unwrap();
        let mut chosen = vec![first];
        while chosen.len() < 16 {
            let next = candidates
                .iter()
                .map(|&p| {
                    (
                        p,
                        chosen
                            .iter()
                            .map(|&c| distance(p, c))
                            .fold(f64::INFINITY, f64::min),
                    )
                })
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            if next.1 < 16.0 {
                break;
            }
            chosen.push(next.0);
        }
        points.extend(
            chosen
                .into_iter()
                .map(|(x, y)| SubjectPoint { x, y, label }),
        );
    }
    points
}

/// Paint-to-select (Remove's "Detect objects", as Lightroom's): the strokes
/// say which object AND roughly how far it reaches. Painted pixels prompt the
/// model without the click path's automatic growth, and the answer is held to
/// the paint plus a margin. Before, a stroke beside a ladder took 18% of the
/// frame and one down a tree took the forest below it (23%); on Test Photos
/// they now take 7% and 13%, while a stroke along a river or a road still
/// gets the whole of it. Erased strokes are always left out.
pub fn select_painted(
    decoder: &Mutex<Session>,
    embeddings: &ImageEmbeddings,
    paint: &GrayImage,
    excluded: &GrayImage,
    brush_radius: f64,
) -> Result<GrayImage> {
    let bounds = paint
        .enumerate_pixels()
        .filter(|(_, _, p)| p[0] > 127)
        .fold(None, |b: Option<(u32, u32, u32, u32)>, (x, y, _)| {
            Some(b.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
            }))
        });
    let Some((x0, y0, x1, y1)) = bounds else {
        anyhow::bail!("Paint over the subject first, then release.");
    };
    let margin = paint_margin(brush_radius, (x1 - x0).max(y1 - y0) as f64);
    let points = paint_points(paint, excluded);
    let mut mask = select_grown(decoder, embeddings, &points, true, false)?;
    confine_to_paint(&mut mask, paint, excluded, margin);
    Ok(mask)
}

/// How far a painted selection may reach past the paint: at least two brush
/// radii, and a fifth of the painted extent, so a quick stroke down the middle
/// of a person still reaches their sides.
fn paint_margin(brush_radius: f64, painted_extent: f64) -> f64 {
    (brush_radius * 2.0).max(painted_extent * 0.2)
}

/// Fades the selection out between `margin` and 1.25 × `margin` from the
/// nearest painted pixel, and clears it under erased strokes. Distances are
/// measured on a grid of at most 1024 px for speed.
fn confine_to_paint(mask: &mut GrayImage, paint: &GrayImage, excluded: &GrayImage, margin: f64) {
    use imageproc::distance_transform::{Norm, distance_transform};
    let (w, h) = paint.dimensions();
    let step = w.max(h).div_ceil(1024).max(1);
    let (gw, gh) = (w.div_ceil(step), h.div_ceil(step));
    let mut grid = GrayImage::new(gw, gh);
    for (x, y, p) in paint.enumerate_pixels() {
        if p[0] > 127 {
            grid.put_pixel(x / step, y / step, image::Luma([255]));
        }
    }
    let distance = distance_transform(&grid, Norm::L2);
    let inner = (margin / step as f64).min(200.0);
    let band = (inner * 0.25).max(2.0);
    let at = |gx: u32, gy: u32| distance.get_pixel(gx.min(gw - 1), gy.min(gh - 1))[0] as f64;
    let mask_w = mask.width() as usize;
    mask.par_chunks_mut(mask_w)
        .enumerate()
        .for_each(|(y, row)| {
            let y = y as u32;
            for (x, p) in row.iter_mut().enumerate() {
                let x = x as u32;
                if *p == 0 {
                    continue;
                }
                if excluded.get_pixel(x, y)[0] > 127 {
                    *p = 0;
                    continue;
                }
                // Bilinear distance, so the fade has no grid steps.
                let fx = (x as f64 + 0.5) / step as f64 - 0.5;
                let fy = (y as f64 + 0.5) / step as f64 - 0.5;
                let (gx, gy) = (fx.max(0.0).floor() as u32, fy.max(0.0).floor() as u32);
                let (tx, ty) = (
                    (fx - gx as f64).clamp(0.0, 1.0),
                    (fy - gy as f64).clamp(0.0, 1.0),
                );
                let d = (at(gx, gy) * (1.0 - tx) + at(gx + 1, gy) * tx) * (1.0 - ty)
                    + (at(gx, gy + 1) * (1.0 - tx) + at(gx + 1, gy + 1) * tx) * ty;
                let keep = ((inner + band - d) / band).clamp(0.0, 1.0);
                *p = (*p as f64 * keep).round() as u8;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    /// A sharp-edged subject whose model matte is SAM-like: a soft band
    /// 30 px wide, stepped on a 16 px grid and shifted 8 px off the outline.
    /// After snapping, the matte must sit on the outline with a narrow edge,
    /// the interior and the far background untouched.
    #[test]
    fn snap_to_edges_puts_a_blocky_offset_edge_on_the_outline() {
        let (w, h) = (640u32, 480u32);
        let (cx, cy, r) = (320.0f32, 240.0f32, 150.0f32);
        let edge = |x: u32, y: u32| (x as f32 - cx).hypot(y as f32 - cy) - r;
        let guide = RgbImage::from_fn(w, h, |x, y| {
            if edge(x, y) < 0.0 {
                image::Rgb([190, 140, 110])
            } else {
                image::Rgb([220, 170, 40])
            }
        });
        let mut matte = GrayImage::from_fn(w, h, |x, y| {
            // Stepped and offset, as SAM's coarse grid leaves it.
            let (bx, by) = ((x / 16) * 16 + 8, (y / 16) * 16 + 8);
            let d = edge(bx, by) - 8.0;
            Luma([(255.0 / (1.0 + (d / 6.0).exp())).round() as u8])
        });
        snap_to_edges(&mut matte, &guide, 16.0);
        let at = |x: u32, y: u32| matte.get_pixel(x, y)[0];
        for angle in (0..360).step_by(15) {
            let (s, c) = (angle as f32).to_radians().sin_cos();
            let point = |d: f32| ((cx + c * (r + d)) as u32, (cy + s * (r + d)) as u32);
            let (ix, iy) = point(-3.0);
            let (ox, oy) = point(3.0);
            assert!(
                at(ix, iy) > 220,
                "inside the edge at {angle}°: {}",
                at(ix, iy)
            );
            assert!(
                at(ox, oy) < 35,
                "outside the edge at {angle}°: {}",
                at(ox, oy)
            );
        }
        assert_eq!(at(cx as u32, cy as u32), 255, "interior");
        assert_eq!(at(5, 5), 0, "far background");
    }
    #[test]
    fn self_feedback_cannot_switch_to_a_larger_unrelated_object() {
        let p = box_or_point((0.0, 0.0), (0.0, 0.0));
        let before = [4.0, -4.0, -4.0, -4.0];
        assert!(!accept_refinement(
            &before, &[4.0; 4], 0.9, 0.95, 2, &p, 512.0
        ));
        assert!(accept_refinement(&before, &before, 0.9, 0.91, 2, &p, 512.0));
    }
    #[test]
    fn whole_object_prefers_large_plausible_candidate_but_respects_exclusion() {
        let mut masks = vec![-2.0; 3 * 16];
        masks[0] = 2.0;
        masks[16..24].fill(2.0);
        masks[32..48].fill(2.0);
        let mut points = box_or_point((0.0, 0.0), (0.0, 0.0));
        assert_eq!(
            choose_candidate(&masks, &[0.94, 0.90, 0.60], 4, &points, 256.0, true, 16),
            1
        );
        points.push(SubjectPoint {
            x: 3.0,
            y: 1.0,
            label: 0,
        });
        assert_eq!(
            choose_candidate(&masks, &[0.94, 0.90, 0.60], 4, &points, 256.0, true, 16),
            0
        );
    }
    #[test]
    fn painted_selection_stays_near_the_paint_and_off_erased_strokes() {
        let mut paint = GrayImage::new(400, 100);
        for y in 40..60 {
            for x in 40..60 {
                paint.put_pixel(x, y, Luma([255]));
            }
        }
        let mut excluded = GrayImage::new(400, 100);
        excluded.put_pixel(70, 50, Luma([255]));
        let mut mask = GrayImage::from_pixel(400, 100, Luma([255]));
        confine_to_paint(&mut mask, &paint, &excluded, paint_margin(10.0, 20.0));
        assert_eq!(mask.get_pixel(50, 50)[0], 255);
        assert_eq!(mask.get_pixel(75, 50)[0], 255);
        assert_eq!(mask.get_pixel(70, 50)[0], 0);
        assert_eq!(mask.get_pixel(300, 50)[0], 0);
    }
    #[test]
    fn portrait_matte_crops_padding_instead_of_stretching_it() {
        let mut logits = vec![-12.0; 16];
        for y in 0..4 {
            logits[y * 4..y * 4 + 2].fill(12.0);
        }
        let matte = render_matte(&logits, 4, &RgbImage::new(20, 40));
        assert!(matte.pixels().all(|p| p[0] == 255));
    }
    #[test]
    fn matte_has_clean_interiors_and_continuous_edges() {
        let guide = RgbImage::new(40, 40);
        assert!(
            render_matte(&[-4.0; 16], 4, &guide)
                .pixels()
                .all(|p| p[0] == 0)
        );
        assert!(
            render_matte(&[4.0; 16], 4, &guide)
                .pixels()
                .all(|p| p[0] == 255)
        );
        assert!(
            render_matte(&[0.0; 16], 4, &guide)
                .pixels()
                .all(|p| p[0] == 128)
        );
        let logits = [-4.0, -4.0, 4.0, 4.0].repeat(4);
        let matte = render_matte(&logits, 4, &guide);
        assert!(matte.pixels().any(|p| p[0] > 0 && p[0] < 255));
    }
    #[test]
    fn confidence_cleanup_preserves_boundary_logits() {
        let mut logits = [-0.5, -0.5, 0.5, 0.5].repeat(4);
        stabilize_interiors(&mut logits, 4);
        for row in logits.chunks_exact(4) {
            assert_eq!(row, &[-4.0, -0.5, 0.5, 4.0]);
        }
    }
    #[test]
    fn single_click_does_not_choose_multiclick_token_or_unstable_spill() {
        let mut masks = vec![-4.0; 4 * 16];
        masks[..16].fill(4.0); // Tempting but inapplicable multi-click token.
        masks[16..24].fill(4.0);
        masks[32..48].fill(0.1); // Large, threshold-sensitive background spill.
        masks[48] = 4.0;
        let points = box_or_point((0.0, 0.0), (0.0, 0.0));
        assert_eq!(
            choose_candidate(&masks, &[0.99, 0.9, 0.94, 0.7], 4, &points, 256.0, true, 16),
            1
        );
    }
    #[test]
    fn guided_edges_follow_color_without_marking_distant_background() {
        let guide = RgbImage::from_fn(128, 128, |x, _| {
            image::Rgb(if x < 64 {
                [220, 30, 30]
            } else {
                [30, 180, 210]
            })
        });
        let logits: Vec<_> = (0..16 * 16)
            .map(|i| if i % 16 < 8 { 4.0 } else { -4.0 })
            .collect();
        let plain = render_coarse_matte(&logits, 16, &guide);
        let guided = render_matte(&logits, 16, &guide);
        let error = |m: &GrayImage| {
            m.enumerate_pixels()
                .map(|(x, _, p)| {
                    if x < 64 {
                        (255 - p[0]) as u64
                    } else {
                        p[0] as u64
                    }
                })
                .sum::<u64>()
        };
        assert!(error(&guided) < error(&plain));
        assert_eq!(guided.get_pixel(10, 64)[0], 255);
        assert_eq!(guided.get_pixel(110, 64)[0], 0);
        // Identical horizontal structure must not acquire a grid pattern.
        for y in 1..128 {
            for x in 0..128 {
                assert_eq!(guided.get_pixel(x, y), guided.get_pixel(x, 0));
            }
        }
    }
    #[test]
    fn guided_filter_handles_one_pixel_wide_images() {
        for (w, h) in [(1, 30), (30, 1), (1, 1)] {
            let result = render_matte(&[4.0; 16], 4, &RgbImage::new(w, h));
            assert_eq!(result.dimensions(), (w, h));
            assert!(result.pixels().all(|p| p[0] == 255));
        }
    }
    #[test]
    fn paint_samples_stay_on_strokes_and_keep_negative_prompts() {
        let mut paint = GrayImage::new(100, 100);
        let mut excluded = paint.clone();
        paint.put_pixel(10, 10, Luma([255]));
        paint.put_pixel(90, 90, Luma([255]));
        excluded.put_pixel(50, 50, Luma([255]));
        let points = paint_points(&paint, &excluded);
        assert!(points.iter().any(|p| p.label == 0));
        for p in points {
            assert_eq!(
                if p.label == 1 { &paint } else { &excluded }.get_pixel(p.x as u32, p.y as u32)[0],
                255
            );
        }
    }
}
