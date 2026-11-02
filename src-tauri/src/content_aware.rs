//! Content-aware fill: rebuilds a selection from real texture copied out of
//! the same photograph, with no model and no invention.
//!
//! This is the method behind Lightroom's Remove with "Use generative AI"
//! off (and Photoshop's Content-Aware Fill): multi-scale PatchMatch
//! completion after Wexler, Shechtman & Irani ("Space-Time Completion of
//! Video", 2007) with the randomized nearest-neighbour search of Barnes et
//! al. ("PatchMatch", 2009).
//!
//! * Every pixel near the hole holds a pointer to a 7x7 source patch that
//!   lies entirely in usable photograph: outside every selection and, for a
//!   clipped (blown) selection, outside the clipped pixels too, so a blown
//!   sky is rebuilt from the real sky and cloud around it rather than from
//!   white.
//! * Pointers are improved by propagation (a neighbour's good match, shifted
//!   by one, is probably good here too) and random search around the best
//!   match at shrinking radii.
//! * The hole is then re-estimated by voting: each hole pixel becomes the
//!   weighted mean of what the overlapping patches say it should be. Patches
//!   that match well, and patches near the known boundary, count more.
//! * This runs coarse to fine. The coarsest level sees the whole hole in a
//!   few dozen pixels and settles its layout; each finer level starts from
//!   the coarser pointers, so the final level copies full-resolution pixels
//!   (real grain and detail, never an upscaled blur).
//!
//! Four things keep it from the classic failures, each measured on the
//! fill test set (examples/fill_testset.rs):
//! * the pyramid's depth follows the hole's thickness, not its span, so the
//!   coarsest level is never so small that a patch covers half the scene;
//! * the coarsest level starts from an exemplar fill in Criminisi order
//!   (edges first), not a smooth blur the search then matches to anything;
//! * copying from a different height costs extra, so the layers of a
//!   photograph (sky, horizon, ground) stay in order;
//! * copying a different kind of surface costs extra: each source's texture
//!   energy is compared with what the ring around the hole has there, so a
//!   soft out-of-focus wall is not filled with sharp hair from the subject
//!   beside it.
//!
//! Then the seam: a membrane correction (as healing does) gives the whole
//! fill the tone of the photograph around it, and a Poisson solve on the
//! copied gradients joins the copies to each other and to the photograph
//! without a step, keeping their detail.
//!
//! (Measured and not adopted: LaMa's fill as a structure guide for the
//! search. It carried fence wires across a hole, but its smooth, biased
//! colour steered every textured fill toward the flattest ground in the
//! frame — cracked mud filled with plain sand, a wall with a flat blob —
//! in colour, gradient and confidence-weighted forms alike.)
//!
//! Distances and votes are taken in the 8-bit display-encoded values every
//! other fill path hands its models, so the result composites through the
//! same patch format and gamma flag as the LaMa and engine fills.
//!
//! Deterministic: every random choice comes from a generator seeded by the
//! caller's seed and the work item, never from thread timing.

use image::{GrayImage, Rgba, RgbaImage};
use rayon::prelude::*;

/// Half the patch side: 7x7 patches.
const R: i32 = 3;
/// The coarsest level sees the hole about this many pixels deep (from its
/// deepest point to the nearest edge)...
const COARSE_DEPTH: f32 = 6.0;
/// ...unless that would leave the level smaller than this on a side, too
/// small for a patch to mean anything.
const COARSE_MIN_SIDE: usize = 64;
/// Rows each parallel search band owns. Fixed so results never depend on
/// the number of threads.
const BAND_ROWS: usize = 16;
/// A pixel whose darkest channel is at least this is clipped white (the
/// Clipped selection's default 98% threshold, in 8-bit).
const CLIPPED_AT: u8 = 250;
/// Wexler's boundary confidence base: a patch `d` pixels from known
/// photograph votes with weight `GAMMA^-d`.
const GAMMA: f32 = 1.3;

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Seeds every random choice. The same seed always gives the same fill;
    /// a new seed gives a different, equally plausible one.
    pub seed: u64,
    /// Never copy from clipped-white pixels (for clipped/blown selections).
    pub exclude_clipped: bool,
    /// Quarter turns the photo is shown at (the editor's `orientationSteps`).
    /// The image is filled in its stored orientation; on an odd count the
    /// displayed vertical (which the height prior follows) is the stored x.
    pub orientation_steps: u8,
}

/// Small, fast, seedable generator (SplitMix64).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `-r..=r`.
    fn around(&mut self, r: i32) -> i32 {
        (self.next() % (2 * r as u64 + 1)) as i32 - r
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mix(a: u64, b: u64) -> u64 {
    Rng::new(a.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ b).next()
}

/// One pyramid level of a context crop.
struct Level {
    w: usize,
    h: usize,
    /// RGB, display-encoded 0..255.
    px: Vec<[f32; 3]>,
    /// Pixels to synthesize.
    hole: Vec<bool>,
    /// Pixels a source patch may contain.
    usable: Vec<bool>,
    /// Weight of the height prior (see `height_prior`).
    lambda_y: f32,
    /// The displayed vertical is the stored x axis (photo turned 90°).
    turned: bool,
    /// Local texture energy (see `texture_energy`): measured outside the
    /// hole, interpolated smoothly across it.
    energy: Option<Vec<f32>>,
    /// Weight of the texture-energy term.
    texture_weight: f32,
}

impl Level {
    fn downsample(&self) -> Level {
        let w = self.w.div_ceil(2);
        let h = self.h.div_ceil(2);
        let mut px = vec![[0.0f32; 3]; w * h];
        let mut hole = vec![false; w * h];
        let mut usable = vec![true; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                let mut all = [0.0f32; 3];
                let (mut n, mut n_all) = (0.0f32, 0.0f32);
                let i = y * w + x;
                for (cx, cy) in [
                    (2 * x, 2 * y),
                    (2 * x + 1, 2 * y),
                    (2 * x, 2 * y + 1),
                    (2 * x + 1, 2 * y + 1),
                ] {
                    if cx >= self.w || cy >= self.h {
                        continue;
                    }
                    let c = cy * self.w + cx;
                    let p = self.px[c];
                    for k in 0..3 {
                        all[k] += p[k];
                    }
                    n_all += 1.0;
                    if self.hole[c] {
                        hole[i] = true;
                    } else {
                        for k in 0..3 {
                            sum[k] += p[k];
                        }
                        n += 1.0;
                    }
                    if !self.usable[c] {
                        usable[i] = false;
                    }
                }
                px[i] = if n > 0.0 {
                    [sum[0] / n, sum[1] / n, sum[2] / n]
                } else {
                    [all[0] / n_all, all[1] / n_all, all[2] / n_all]
                };
            }
        }
        Level {
            w,
            h,
            px,
            hole,
            usable,
            lambda_y: self.lambda_y,
            turned: self.turned,
            energy: None,
            texture_weight: self.texture_weight,
        }
    }

    /// Centres whose whole 7x7 patch is inside the level and usable.
    fn valid_centres(&self) -> Vec<bool> {
        let (w, h) = (self.w, self.h);
        // Summed-area table of unusable pixels.
        let mut sat = vec![0u32; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0u32;
            for x in 0..w {
                row += u32::from(!self.usable[y * w + x]);
                sat[(y + 1) * (w + 1) + x + 1] = sat[y * (w + 1) + x + 1] + row;
            }
        }
        let r = R as usize;
        let mut valid = vec![false; w * h];
        if w <= 2 * r || h <= 2 * r {
            return valid;
        }
        for y in r..h - r {
            for x in r..w - r {
                let (x0, y0, x1, y1) = (x - r, y - r, x + r + 1, y + r + 1);
                let bad = sat[y1 * (w + 1) + x1] + sat[y0 * (w + 1) + x0]
                    - sat[y0 * (w + 1) + x1]
                    - sat[y1 * (w + 1) + x0];
                valid[y * w + x] = bad == 0;
            }
        }
        valid
    }

    /// Pixels whose patch overlaps the hole: the hole dilated by `R`.
    fn targets(&self) -> Vec<bool> {
        let (w, h) = (self.w, self.h);
        let r = R as usize;
        // Separable dilation: rows, then columns.
        let mut rows = vec![false; w * h];
        for y in 0..h {
            let mut last: Option<usize> = None;
            for x in 0..w + r {
                if x < w && self.hole[y * w + x] {
                    last = Some(x);
                }
                if x >= r && last.is_some_and(|l| x - l <= 2 * r) {
                    rows[y * w + (x - r)] = true;
                }
            }
        }
        let mut out = vec![false; w * h];
        for x in 0..w {
            let mut last: Option<usize> = None;
            for y in 0..h + r {
                if y < h && rows[y * w + x] {
                    last = Some(y);
                }
                if y >= r && last.is_some_and(|l| y - l <= 2 * r) {
                    out[(y - r) * w + x] = true;
                }
            }
        }
        out
    }
}

/// Chamfer distance from every pixel to the nearest non-hole pixel.
fn distance_to_known(hole: &[bool], w: usize, h: usize) -> Vec<f32> {
    const FAR: f32 = 1.0e6;
    let mut d: Vec<f32> = hole.iter().map(|&b| if b { FAR } else { 0.0 }).collect();
    let s2 = std::f32::consts::SQRT_2;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if d[i] == 0.0 {
                continue;
            }
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + 1.0);
            }
            if y > 0 {
                v = v.min(d[i - w] + 1.0);
                if x > 0 {
                    v = v.min(d[i - w - 1] + s2);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + s2);
                }
            }
            d[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            if d[i] == 0.0 {
                continue;
            }
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + 1.0);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + 1.0);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + s2);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + s2);
                }
            }
            d[i] = v;
        }
    }
    d
}

/// Mean squared difference (per pixel, summed over RGB) between the patch
/// at target `t` and the patch at source `s`, giving up once it exceeds
/// `cap`. Target pixels outside the level are skipped; source patches are
/// always fully inside (only valid centres are ever sources).
#[inline]
fn raw_patch_distance(lv: &Level, t: (i32, i32), s: (i32, i32), cap: f32) -> f32 {
    let (w, h) = (lv.w as i32, lv.h as i32);
    let mut sum = 0.0f32;
    let mut n = 0u32;
    let cap_sum = cap * 49.0;
    for dy in -R..=R {
        let ty = t.1 + dy;
        if ty < 0 || ty >= h {
            continue;
        }
        let trow = (ty * w) as usize;
        let srow = ((s.1 + dy) * w) as usize;
        for dx in -R..=R {
            let tx = t.0 + dx;
            if tx < 0 || tx >= w {
                continue;
            }
            let a = lv.px[trow + tx as usize];
            let b = lv.px[srow + (s.0 + dx) as usize];
            let (d0, d1, d2) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
            sum += d0 * d0 + d1 * d1 + d2 * d2;
            n += 1;
        }
        if sum > cap_sum {
            return f32::MAX;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}

/// What copying from a different height costs, on top of the colour
/// difference. Photographs are layered top to bottom (sky over horizon over
/// ground) and ground texture shrinks with distance, so a source level with
/// the hole is usually the right one; without this, a hole touching the
/// horizon grew hillside up into the sky. In the distance's units (mean
/// squared 8-bit RGB difference) for a source one level-height away.
const LAMBDA_Y: f32 = 2400.0;

/// What copying a different kind of surface costs (see `energy_distance`),
/// per squared unit of log energy ratio beyond the dead zone; in the
/// distance's units. Measured on the fill test set: it stops a soft,
/// out-of-focus wall being filled with sharp hair and a face from the
/// subject beside it, and leaves fills of uniform texture as they were.
const TEXTURE_WEIGHT: f32 = 1000.0;

/// Local texture energy of a level: the RMS of its fine detail (luma minus
/// a small blur) over a window three patches wide. Inside the hole, and wherever
/// the window reaches into it, the energy is not measured but interpolated
/// smoothly from the measured ring around it, which is what the fill should
/// match there.
fn texture_energy(lv: &Level) -> Vec<f32> {
    let (w, h) = (lv.w, lv.h);
    let luma: Vec<[f32; 3]> = lv
        .px
        .iter()
        .map(|p| {
            let l = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            [l, 0.0, 0.0]
        })
        .collect();
    let low = box_blur2(&luma, w, h, 1);
    let hp2: Vec<[f32; 3]> = luma
        .iter()
        .zip(&low)
        .map(|(a, b)| {
            let d = a[0] - b[0];
            [d * d, 0.0, 0.0]
        })
        .collect();
    let e2 = box_blur2(&hp2, w, h, 3 * R as usize);
    // Measured only where the window is clear of the hole.
    let outside: Vec<bool> = lv.hole.iter().map(|b| !b).collect();
    let d_out = distance_to_known(&outside, w, h);
    let known: Vec<bool> = (0..w * h)
        .map(|i| !lv.hole[i] && d_out[i] > (6 * R + 2) as f32)
        .collect();
    // Plus a floor for sensor noise, so flat sky compares by ratio sanely.
    let measured: Vec<f32> = e2.iter().map(|v| v[0].max(0.0).sqrt() + 1.5).collect();
    smooth_fill(&measured, &known, w, h)
}

#[inline]
fn height_prior(lv: &Level, t: (i32, i32), s: (i32, i32)) -> f32 {
    // Along the displayed vertical: the stored x axis on a turned photo.
    let dy = if lv.turned {
        (s.0 - t.0) as f32 / lv.w.max(1) as f32
    } else {
        (s.1 - t.1) as f32 / lv.h.max(1) as f32
    };
    lv.lambda_y * dy * dy
}

/// How far the source's texture energy is from what the hole should have
/// at the target: a smooth patch copied into grainy ground (or the reverse)
/// shows as a flat blob even when its colour is right.
#[inline]
fn energy_distance(lv: &Level, t: (i32, i32), s: (i32, i32)) -> f32 {
    let Some(e) = &lv.energy else {
        return 0.0;
    };
    let w = lv.w as i32;
    // A ratio within the dead zone is free: only a clearly different kind
    // of surface (sharp hair against soft bokeh, smooth sand against
    // cracked mud) costs, so the term never fragments a coherent copy.
    let ratio = (e[(t.1 * w + t.0) as usize] / e[(s.1 * w + s.0) as usize])
        .ln()
        .abs();
    let excess = (ratio - ENERGY_DEAD_ZONE).max(0.0);
    lv.texture_weight * excess * excess
}

/// Texture energies within this log-ratio of each other (x1.6) count as
/// the same kind of surface.
const ENERGY_DEAD_ZONE: f32 = 0.47;

#[inline]
fn patch_distance(lv: &Level, t: (i32, i32), s: (i32, i32), cap: f32) -> f32 {
    let prior = height_prior(lv, t, s) + energy_distance(lv, t, s);
    if prior >= cap {
        return f32::MAX;
    }
    let d = raw_patch_distance(lv, t, s, cap - prior);
    if d == f32::MAX { d } else { d + prior }
}

#[inline]
fn masked_distance(lv: &Level, known: &[bool], t: (i32, i32), s: (i32, i32), cap: f32) -> f32 {
    let prior = height_prior(lv, t, s);
    if prior >= cap {
        return f32::MAX;
    }
    let d = raw_masked_distance(lv, known, t, s, cap - prior);
    if d == f32::MAX { d } else { d + prior }
}

/// The nearest-neighbour field: for every target pixel, its source centre.
struct Field {
    src: Vec<(i32, i32)>,
    dist: Vec<f32>,
}

struct LevelCtx<'a> {
    lv: &'a Level,
    valid: &'a [bool],
    valid_list: &'a [u32],
    targets: &'a [bool],
}

impl LevelCtx<'_> {
    #[inline]
    fn is_valid(&self, s: (i32, i32)) -> bool {
        s.0 >= 0
            && s.1 >= 0
            && (s.0 as usize) < self.lv.w
            && (s.1 as usize) < self.lv.h
            && self.valid[s.1 as usize * self.lv.w + s.0 as usize]
    }

    fn random_valid(&self, rng: &mut Rng) -> (i32, i32) {
        let i = self.valid_list[rng.below(self.valid_list.len())] as usize;
        ((i % self.lv.w) as i32, (i / self.lv.w) as i32)
    }

    /// One PatchMatch pass (propagation + random search) over every target,
    /// in parallel bands of rows. Within a band the scan is sequential so
    /// good matches propagate; across bands, neighbours are read from the
    /// field as it was when the pass began, which keeps the result
    /// independent of thread scheduling.
    fn search(&self, field: &mut Field, seed: u64, reverse: bool) {
        let w = self.lv.w;
        let h = self.lv.h;
        let before = field.src.clone();
        let max_r = (w.max(h) as i32).max(1);
        let band_len = BAND_ROWS * w;
        field
            .src
            .par_chunks_mut(band_len)
            .zip(field.dist.par_chunks_mut(band_len))
            .enumerate()
            .for_each(|(band, (src, dist))| {
                let mut rng = Rng::new(mix(seed, band as u64));
                let off = band * band_len;
                let rows = src.len() / w;
                let step: i32 = if reverse { -1 } else { 1 };
                for ry in 0..rows {
                    let y = if reverse { rows - 1 - ry } else { ry };
                    let gy = (off / w + y) as i32;
                    for rx in 0..w {
                        let x = if reverse { w - 1 - rx } else { rx };
                        let li = y * w + x;
                        let gi = off + li;
                        if !self.targets[gi] {
                            continue;
                        }
                        let t = (x as i32, gy);
                        let mut best = src[li];
                        let mut best_d = dist[li];
                        // Propagation from the already-visited neighbours.
                        let nx = x as i32 - step;
                        if nx >= 0 && (nx as usize) < w {
                            let ng = gi as i64 - step as i64;
                            if self.targets[ng as usize] {
                                let s = src[(li as i64 - step as i64) as usize];
                                let cand = (s.0 + step, s.1);
                                if cand != best && self.is_valid(cand) {
                                    let d = patch_distance(self.lv, t, cand, best_d);
                                    if d < best_d {
                                        best = cand;
                                        best_d = d;
                                    }
                                }
                            }
                        }
                        let ny = gy - step;
                        if ny >= 0 && (ny as usize) < h {
                            let ng = ny as usize * w + x;
                            if self.targets[ng] {
                                let s = if ng >= off && ng < off + src.len() {
                                    src[ng - off]
                                } else {
                                    before[ng]
                                };
                                let cand = (s.0, s.1 + step);
                                if cand != best && self.is_valid(cand) {
                                    let d = patch_distance(self.lv, t, cand, best_d);
                                    if d < best_d {
                                        best = cand;
                                        best_d = d;
                                    }
                                }
                            }
                        }
                        // Random search at exponentially shrinking radii.
                        let mut r = max_r;
                        while r >= 1 {
                            let cand = (best.0 + rng.around(r), best.1 + rng.around(r));
                            if self.is_valid(cand) {
                                let d = patch_distance(self.lv, t, cand, best_d);
                                if d < best_d {
                                    best = cand;
                                    best_d = d;
                                }
                            }
                            r /= 2;
                        }
                        src[li] = best;
                        dist[li] = best_d;
                    }
                }
            });
    }

    /// Re-estimates every hole pixel as the weighted mean of what the
    /// overlapping patches say it should be.
    ///
    /// With `ring`, known pixels near the hole are estimated too (what the
    /// fill would have put there), which is what the seam correction
    /// compares against the photograph.
    fn vote(&self, field: &Field, conf: &[f32], ring: bool) -> Vec<[f32; 3]> {
        let (w, h) = (self.lv.w, self.lv.h);
        // Similarity scale: a good match is one at or under the 75th
        // percentile of this pass's distances.
        let mut sample: Vec<f32> = field
            .dist
            .iter()
            .zip(self.targets)
            .filter(|(_, t)| **t)
            .map(|(d, _)| *d)
            .step_by(7)
            .collect();
        let sigma2 = if sample.is_empty() {
            1.0
        } else {
            let k = (sample.len() * 3 / 4).min(sample.len() - 1);
            let (_, v, _) = sample.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
            v.max(1.0)
        };
        let weight: Vec<f32> = field
            .dist
            .iter()
            .zip(conf)
            .zip(self.targets)
            .map(|((d, c), t)| {
                if *t {
                    (-d / (2.0 * sigma2)).exp() * c + 1e-8
                } else {
                    0.0
                }
            })
            .collect();
        let mut out = self.lv.px.clone();
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let i = y * w + x;
                if !(self.lv.hole[i] || (ring && self.targets[i])) {
                    continue;
                }
                let mut acc = [0.0f32; 3];
                let mut wsum = 0.0f32;
                for dy in -R..=R {
                    let py = y as i32 + dy;
                    if py < 0 || py >= h as i32 {
                        continue;
                    }
                    for dx in -R..=R {
                        let px = x as i32 + dx;
                        if px < 0 || px >= w as i32 {
                            continue;
                        }
                        let p = py as usize * w + px as usize;
                        let wt = weight[p];
                        if wt == 0.0 {
                            continue;
                        }
                        let s = field.src[p];
                        let q = (s.1 - dy) as usize * w + (s.0 - dx) as usize;
                        let v = self.lv.px[q];
                        acc[0] += wt * v[0];
                        acc[1] += wt * v[1];
                        acc[2] += wt * v[2];
                        wsum += wt;
                    }
                }
                if wsum > 0.0 {
                    *o = [acc[0] / wsum, acc[1] / wsum, acc[2] / wsum];
                }
            }
        });
        out
    }
}

impl LevelCtx<'_> {
    /// Votes for the fill's gradients rather than its colours: at every
    /// target pixel, the weighted mean of the forward differences the
    /// overlapping patches copy there. Where two copied regions meet, their
    /// colours may disagree but their gradients barely do, so a fill
    /// rebuilt from these (see `poisson_refine`) has no seam between them.
    fn vote_gradients(&self, field: &Field, conf: &[f32]) -> Gradients {
        let (w, h) = (self.lv.w, self.lv.h);
        let mut sample: Vec<f32> = field
            .dist
            .iter()
            .zip(self.targets)
            .filter(|(_, t)| **t)
            .map(|(d, _)| *d)
            .step_by(7)
            .collect();
        let sigma2 = if sample.is_empty() {
            1.0
        } else {
            let k = (sample.len() * 3 / 4).min(sample.len() - 1);
            let (_, v, _) = sample.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
            v.max(1.0)
        };
        let weight: Vec<f32> = field
            .dist
            .iter()
            .zip(conf)
            .zip(self.targets)
            .map(|((d, c), t)| {
                if *t {
                    (-d / (2.0 * sigma2)).exp() * c + 1e-8
                } else {
                    0.0
                }
            })
            .collect();
        let px = &self.lv.px;
        let mut gx = vec![[0.0f32; 3]; w * h];
        let mut gy = vec![[0.0f32; 3]; w * h];
        gx.par_chunks_mut(w)
            .zip(gy.par_chunks_mut(w))
            .enumerate()
            .for_each(|(y, (rx, ry))| {
                for x in 0..w {
                    let i = y * w + x;
                    if !self.targets[i] {
                        continue;
                    }
                    let (mut ax, mut ay) = ([0.0f32; 3], [0.0f32; 3]);
                    let (mut wx, mut wy) = (0.0f32, 0.0f32);
                    for dy in -R..=R {
                        let py = y as i32 + dy;
                        if py < 0 || py >= h as i32 {
                            continue;
                        }
                        for dx in -R..=R {
                            let pxx = x as i32 + dx;
                            if pxx < 0 || pxx >= w as i32 {
                                continue;
                            }
                            let p = py as usize * w + pxx as usize;
                            let wt = weight[p];
                            if wt == 0.0 {
                                continue;
                            }
                            let s = field.src[p];
                            let q = (s.1 - dy) as usize * w + (s.0 - dx) as usize;
                            // Only differences that stay inside the patch:
                            // its pixels are all real photograph.
                            if -dx < R {
                                let a = px[q];
                                let b = px[q + 1];
                                for c in 0..3 {
                                    ax[c] += wt * (b[c] - a[c]);
                                }
                                wx += wt;
                            }
                            if -dy < R {
                                let a = px[q];
                                let b = px[q + w];
                                for c in 0..3 {
                                    ay[c] += wt * (b[c] - a[c]);
                                }
                                wy += wt;
                            }
                        }
                    }
                    if wx > 0.0 {
                        rx[x] = [ax[0] / wx, ax[1] / wx, ax[2] / wx];
                    }
                    if wy > 0.0 {
                        ry[x] = [ay[0] / wy, ay[1] / wy, ay[2] / wy];
                    }
                }
            });
        (gx, gy)
    }
}

/// Like `patch_distance`, counting only the target pixels marked `known`.
fn raw_masked_distance(lv: &Level, known: &[bool], t: (i32, i32), s: (i32, i32), cap: f32) -> f32 {
    let (w, h) = (lv.w as i32, lv.h as i32);
    let mut sum = 0.0f32;
    let mut n = 0u32;
    for dy in -R..=R {
        let ty = t.1 + dy;
        if ty < 0 || ty >= h {
            continue;
        }
        for dx in -R..=R {
            let tx = t.0 + dx;
            if tx < 0 || tx >= w {
                continue;
            }
            let ti = (ty * w + tx) as usize;
            if !known[ti] {
                continue;
            }
            let a = lv.px[ti];
            let b = lv.px[((s.1 + dy) * w + s.0 + dx) as usize];
            let (d0, d1, d2) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
            sum += d0 * d0 + d1 * d1 + d2 * d2;
            n += 1;
        }
    }
    if n == 0 {
        return f32::MAX;
    }
    let d = sum / n as f32;
    if d > cap { f32::MAX } else { d }
}

/// Fills the coarsest level's hole from the outside in, each pixel copied
/// from the source patch that best matches what is already known around
/// it, with pointers carried on from filled neighbours so copied structure
/// stays coherent. A starting guess: the EM passes that follow refine it.
///
/// The order is Criminisi et al.'s ("Region Filling and Object Removal by
/// Exemplar-Based Image Inpainting", 2004): confidence (how much of the
/// patch is known) times how squarely an edge in the photograph runs into
/// the hole. Edges such as a horizon are carried across first, so the sky
/// beside a hole closes over it before the ground below can grow upward
/// into it (measured: filling by distance alone grew a block of hillside
/// into the middle of a sky).
fn greedy_init(lv: &mut Level, valid: &[bool], valid_list: &[u32], seed: u64) {
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;

    #[derive(PartialEq)]
    struct Item(f32, u32, usize);
    impl Eq for Item {}
    impl PartialOrd for Item {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Item {
        fn cmp(&self, o: &Self) -> Ordering {
            self.0
                .total_cmp(&o.0)
                // Ties: lower index first, for determinism.
                .then(o.2.cmp(&self.2))
        }
    }

    let (w, h) = (lv.w, lv.h);
    let mut known: Vec<bool> = lv.hole.iter().map(|b| !b).collect();
    let mut conf: Vec<f32> = known.iter().map(|k| if *k { 1.0 } else { 0.0 }).collect();
    let mut version = vec![0u32; w * h];
    let mut src: Vec<(i32, i32)> = vec![(-1, -1); w * h];
    let mut rng = Rng::new(seed);
    let is_valid = |c: (i32, i32)| {
        c.0 >= 0
            && c.1 >= 0
            && (c.0 as usize) < w
            && (c.1 as usize) < h
            && valid[c.1 as usize * w + c.0 as usize]
    };
    let luma = |p: [f32; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];

    // Priority of a front pixel, from the current known/confidence state.
    let priority = |i: usize, known: &[bool], conf: &[f32], px: &[[f32; 3]]| -> Option<f32> {
        let (x, y) = ((i % w) as i32, (i / w) as i32);
        let at = |x: i32, y: i32| -> Option<usize> {
            (x >= 0 && y >= 0 && x < w as i32 && y < h as i32).then(|| y as usize * w + x as usize)
        };
        // On the front: unfilled, with a filled 8-neighbour.
        let mut front = false;
        let (mut nx, mut ny) = (0.0f32, 0.0f32);
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                if let Some(j) = at(x + dx, y + dy)
                    && known[j]
                {
                    front = true;
                    // The normal points from the filled side into the hole.
                    nx -= dx as f32;
                    ny -= dy as f32;
                }
            }
        }
        if !front {
            return None;
        }
        let nl = (nx * nx + ny * ny).sqrt().max(1e-6);
        let (nx, ny) = (nx / nl, ny / nl);
        let mut c = 0.0f32;
        let mut iso = 0.0f32;
        for dy in -R..=R {
            for dx in -R..=R {
                let Some(j) = at(x + dx, y + dy) else {
                    continue;
                };
                c += conf[j];
                // The strongest known edge in the patch, as an isophote.
                let (qx, qy) = (x + dx, y + dy);
                if let (Some(l), Some(r), Some(u), Some(d)) = (
                    at(qx - 1, qy),
                    at(qx + 1, qy),
                    at(qx, qy - 1),
                    at(qx, qy + 1),
                ) && known[l]
                    && known[r]
                    && known[u]
                    && known[d]
                {
                    let gx = (luma(px[r]) - luma(px[l])) * 0.5;
                    let gy = (luma(px[d]) - luma(px[u])) * 0.5;
                    // Isophote (-gy, gx) against the normal.
                    let along = (-gy * nx + gx * ny).abs() / 255.0;
                    iso = iso.max(along);
                }
            }
        }
        Some(c / 49.0 * (iso + 0.02))
    };

    let mut heap = BinaryHeap::new();
    for i in 0..w * h {
        if !known[i]
            && let Some(p) = priority(i, &known, &conf, &lv.px)
        {
            heap.push(Item(p, 0, i));
        }
    }
    let max_r = (w.max(h) as i32).max(1);
    while let Some(Item(_, ver, i)) = heap.pop() {
        if known[i] || ver != version[i] {
            continue;
        }
        let t = ((i % w) as i32, (i / w) as i32);
        let mut best = (-1, -1);
        let mut best_d = f32::MAX;
        let mut cands: Vec<(i32, i32)> = Vec::with_capacity(48);
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let (nx, ny) = (t.0 + dx, t.1 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let s = src[ny as usize * w + nx as usize];
                if s.0 >= 0 {
                    cands.push((s.0 - dx, s.1 - dy));
                }
            }
        }
        for _ in 0..32 {
            let v = valid_list[rng.below(valid_list.len())] as usize;
            cands.push(((v % w) as i32, (v / w) as i32));
        }
        for c in cands {
            if is_valid(c) {
                let dd = masked_distance(lv, &known, t, c, best_d);
                if dd < best_d {
                    best = c;
                    best_d = dd;
                }
            }
        }
        let mut r = max_r;
        while r >= 1 && best.0 >= 0 {
            let c = (best.0 + rng.around(r), best.1 + rng.around(r));
            if is_valid(c) {
                let dd = masked_distance(lv, &known, t, c, best_d);
                if dd < best_d {
                    best = c;
                    best_d = dd;
                }
            }
            r /= 2;
        }
        if best.0 < 0 {
            let v = valid_list[rng.below(valid_list.len())] as usize;
            best = ((v % w) as i32, (v / w) as i32);
        }
        // Confidence of what was just filled: the patch's at this moment.
        let mut c = 0.0f32;
        for dy in -R..=R {
            for dx in -R..=R {
                let (qx, qy) = (t.0 + dx, t.1 + dy);
                if qx >= 0 && qy >= 0 && qx < w as i32 && qy < h as i32 {
                    c += conf[qy as usize * w + qx as usize];
                }
            }
        }
        src[i] = best;
        lv.px[i] = lv.px[best.1 as usize * w + best.0 as usize];
        known[i] = true;
        conf[i] = c / 49.0;
        // Neighbours whose patch or front changed get a fresh priority.
        for dy in -(R + 1)..=(R + 1) {
            for dx in -(R + 1)..=(R + 1) {
                let (qx, qy) = (t.0 + dx, t.1 + dy);
                if qx < 0 || qy < 0 || qx >= w as i32 || qy >= h as i32 {
                    continue;
                }
                let j = qy as usize * w + qx as usize;
                if known[j] {
                    continue;
                }
                if let Some(p) = priority(j, &known, &conf, &lv.px) {
                    version[j] += 1;
                    heap.push(Item(p, version[j], j));
                }
            }
        }
    }
}

/// A fill's x and y forward differences, per pixel.
type Gradients = (Vec<[f32; 3]>, Vec<[f32; 3]>);

/// What `synthesize` returns: the level-0 pixels with the hole filled, and
/// the fill's own estimate of the known pixels just around it.
struct Synthesized {
    px: Vec<[f32; 3]>,
    ring: Vec<[f32; 3]>,
    /// What the copied patches say the fill's x and y gradients are
    /// (forward differences), around and inside the hole.
    grad: Option<Gradients>,
    /// The hole's half-thickness in pixels (deepest point to the edge).
    depth: f32,
}

/// Fills the `known == false` pixels of `v` smoothly from the known ones
/// (push-pull: masked averages down a pyramid, then interpolated back up
/// and relaxed toward a membrane). Unlike a pinned solve, unknown values
/// never leak into the coarse averages, so a large gap is not pulled toward
/// whatever placeholder it held.
fn smooth_fill(v: &[f32], known: &[bool], w: usize, h: usize) -> Vec<f32> {
    let n_known = known.iter().filter(|k| **k).count();
    if n_known == w * h {
        return v.to_vec();
    }
    if n_known == 0 {
        return vec![0.0; w * h];
    }
    if w <= 2 || h <= 2 {
        let mean = v
            .iter()
            .zip(known)
            .filter(|(_, k)| **k)
            .map(|(x, _)| *x)
            .sum::<f32>()
            / n_known as f32;
        return v
            .iter()
            .zip(known)
            .map(|(x, k)| if *k { *x } else { mean })
            .collect();
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut cv = vec![0.0f32; cw * ch];
    let mut ck = vec![false; cw * ch];
    for y in 0..ch {
        for x in 0..cw {
            let (mut sum, mut n) = (0.0f32, 0u32);
            for (sx, sy) in [
                (2 * x, 2 * y),
                (2 * x + 1, 2 * y),
                (2 * x, 2 * y + 1),
                (2 * x + 1, 2 * y + 1),
            ] {
                if sx < w && sy < h && known[sy * w + sx] {
                    sum += v[sy * w + sx];
                    n += 1;
                }
            }
            if n > 0 {
                cv[y * cw + x] = sum / n as f32;
                ck[y * cw + x] = true;
            }
        }
    }
    let coarse = smooth_fill(&cv, &ck, cw, ch);
    let mut out = v.to_vec();
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * 0.5 - 0.5).max(0.0);
        let y0 = (fy as usize).min(ch - 1);
        let y1 = (y0 + 1).min(ch - 1);
        let ty = fy - y0 as f32;
        for x in 0..w {
            if known[y * w + x] {
                continue;
            }
            let fx = ((x as f32 + 0.5) * 0.5 - 0.5).max(0.0);
            let x0 = (fx as usize).min(cw - 1);
            let x1 = (x0 + 1).min(cw - 1);
            let tx = fx - x0 as f32;
            let a = coarse[y0 * cw + x0] * (1.0 - tx) + coarse[y0 * cw + x1] * tx;
            let b = coarse[y1 * cw + x0] * (1.0 - tx) + coarse[y1 * cw + x1] * tx;
            out[y * w + x] = a * (1.0 - ty) + b * ty;
        }
    }
    // A few relaxation sweeps (in place) smooth the interpolation's kinks.
    for _ in 0..4 {
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if known[i] {
                    continue;
                }
                let l = out[if x > 0 { i - 1 } else { i }];
                let r = out[if x + 1 < w { i + 1 } else { i }];
                let u = out[if y > 0 { i - w } else { i }];
                let d = out[if y + 1 < h { i + w } else { i }];
                out[i] = 0.25 * (l + r + u + d);
            }
        }
    }
    out
}

/// Box blur of radius `r` (clamped at the borders), run twice: close to a
/// Gaussian of sigma ~0.8r.
fn box_blur2(px: &[[f32; 3]], w: usize, h: usize, r: usize) -> Vec<[f32; 3]> {
    if r == 0 {
        return px.to_vec();
    }
    let pass = |src: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0f32; 3]; w * h];
        let (n_lines, len) = if horizontal { (h, w) } else { (w, h) };
        let at = |line: usize, k: usize| {
            if horizontal {
                line * w + k
            } else {
                k * w + line
            }
        };
        let norm = 1.0 / (2 * r + 1) as f32;
        for line in 0..n_lines {
            let mut acc = [0.0f32; 3];
            for k in 0..=2 * r {
                let p = src[at(line, k.saturating_sub(r).min(len - 1))];
                for c in 0..3 {
                    acc[c] += p[c];
                }
            }
            for k in 0..len {
                out[at(line, k)] = [acc[0] * norm, acc[1] * norm, acc[2] * norm];
                let add = src[at(line, (k + r + 1).min(len - 1))];
                let sub = src[at(line, k.saturating_sub(r))];
                for c in 0..3 {
                    acc[c] += add[c] - sub[c];
                }
            }
        }
        out
    };
    let mut v = px.to_vec();
    for _ in 0..2 {
        v = pass(&v, true);
        v = pass(&v, false);
    }
    v
}

/// Synthesizes the hole of `base` (full resolution), or `None` when there
/// is nothing usable to copy from.
fn synthesize(base: Level, seed: u64) -> Option<Synthesized> {
    // The hole's thickness decides the pyramid depth: the coarsest level
    // should see across it in a few patch widths. The span would go far
    // deeper on a long thin hole, to a level so small that a 7x7 patch
    // covers half the scene and the layout it settles is nonsense (measured:
    // a sky band filled with sand, a stone step with a face).
    let depth = distance_to_known(&base.hole, base.w, base.h)
        .into_iter()
        .fold(0.0f32, f32::max);
    if depth == 0.0 {
        let px = base.px.clone();
        return Some(Synthesized {
            ring: base.px,
            px,
            grad: None,
            depth,
        });
    }
    let mut levels = vec![base];
    let mut r = depth;
    while r > COARSE_DEPTH {
        let last = levels.last().unwrap();
        if last.w.min(last.h) / 2 < COARSE_MIN_SIDE {
            break;
        }
        let next = last.downsample();
        levels.push(next);
        r /= 2.0;
    }
    // The coarsest level that still has somewhere to copy from.
    while levels.len() > 1 && !levels.last().unwrap().valid_centres().iter().any(|v| *v) {
        levels.pop();
    }
    let n_levels = levels.len();
    let mut field: Option<Field> = None;
    let mut ring = Vec::new();
    let mut grad = None;
    for li in (0..n_levels).rev() {
        let valid = levels[li].valid_centres();
        let valid_list: Vec<u32> = valid
            .iter()
            .enumerate()
            .filter(|(_, v)| **v)
            .map(|(i, _)| i as u32)
            .collect();
        if valid_list.is_empty() {
            return None;
        }
        if levels[li].texture_weight > 0.0 {
            let e = texture_energy(&levels[li]);
            levels[li].energy = Some(e);
        }
        if li == n_levels - 1 {
            greedy_init(&mut levels[li], &valid, &valid_list, mix(seed, 0xC0A25E));
        }
        let targets = levels[li].targets();
        let lv = &levels[li];
        let w = lv.w;
        let conf: Vec<f32> = distance_to_known(&lv.hole, w, lv.h)
            .into_iter()
            .map(|d| GAMMA.powf(-d.min(40.0)))
            .collect();
        let ctx = LevelCtx {
            lv,
            valid: &valid,
            valid_list: &valid_list,
            targets: &targets,
        };
        let level_seed = mix(seed, li as u64);
        // Initial field: random at the coarsest level, else the coarser
        // level's pointers scaled up.
        let mut rng = Rng::new(mix(level_seed, 0xF1E1D));
        let mut src = vec![(0i32, 0i32); w * lv.h];
        for (i, s) in src.iter_mut().enumerate() {
            if !targets[i] {
                continue;
            }
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let up = field.as_ref().and_then(|f| {
                let cw = levels[li + 1].w;
                let ch = levels[li + 1].h;
                let (cx, cy) = ((x / 2).min(cw as i32 - 1), (y / 2).min(ch as i32 - 1));
                let c = f.src[cy as usize * cw + cx as usize];
                let cand = (2 * c.0 + (x & 1), 2 * c.1 + (y & 1));
                if ctx.is_valid(cand) {
                    Some(cand)
                } else {
                    // Nudge toward the level's interior before giving up.
                    let nudged = (
                        cand.0.clamp(R, w as i32 - 1 - R),
                        cand.1.clamp(R, lv.h as i32 - 1 - R),
                    );
                    ctx.is_valid(nudged).then_some(nudged)
                }
            });
            *s = up.unwrap_or_else(|| ctx.random_valid(&mut rng));
        }
        let mut f = Field {
            dist: vec![f32::MAX; w * lv.h],
            src,
        };
        // Distances for the initial pointers.
        f.dist
            .par_iter_mut()
            .zip(f.src.par_iter())
            .enumerate()
            .for_each(|(i, (d, s))| {
                if targets[i] {
                    *d = patch_distance(lv, ((i % w) as i32, (i / w) as i32), *s, f32::MAX);
                }
            });
        let mut px = if li == n_levels - 1 {
            None
        } else {
            // Start this level from the upsampled pointers' vote.
            Some(ctx.vote(&f, &conf, false))
        };
        // EM: coarse levels settle the layout; fine levels mostly refine.
        // (Measured: 5/10/24 iterations bought ~3% on in-mask error for
        // two to three times the time.)
        let iterations = if li == 0 {
            3
        } else if li == n_levels - 1 {
            12
        } else {
            5
        };
        for it in 0..iterations {
            if let Some(p) = px.take() {
                levels[li].px = p;
            }
            let lv = &levels[li];
            let ctx = LevelCtx {
                lv,
                valid: &valid,
                valid_list: &valid_list,
                targets: &targets,
            };
            // Distances against the current estimate, then search.
            f.dist
                .par_iter_mut()
                .zip(f.src.par_iter())
                .enumerate()
                .for_each(|(i, (d, s))| {
                    if targets[i] {
                        *d = patch_distance(lv, ((i % w) as i32, (i / w) as i32), *s, f32::MAX);
                    }
                });
            ctx.search(&mut f, mix(level_seed, it as u64 + 1), it % 2 == 1);
            let last = li == 0 && it + 1 == iterations;
            px = Some(ctx.vote(&f, &conf, last));
        }
        if let Some(p) = px.take() {
            if li == 0 {
                // The last vote also estimated the known ring: keep that
                // apart, and the photograph where it is known.
                let lv = &mut levels[0];
                for (i, v) in p.iter().enumerate() {
                    if lv.hole[i] {
                        lv.px[i] = *v;
                    }
                }
                ring = p;
                let ctx = LevelCtx {
                    lv: &levels[0],
                    valid: &valid,
                    valid_list: &valid_list,
                    targets: &targets,
                };
                grad = Some(ctx.vote_gradients(&f, &conf));
            } else {
                levels[li].px = p;
            }
        }
        field = Some(f);
        // The finer levels need only their own pyramid entry from here.
        levels.truncate(li + 1);
    }
    Some(Synthesized {
        px: levels.swap_remove(0).px,
        ring,
        grad,
        depth,
    })
}

/// Removes the tone step a copy would leave at the selection's edge. The
/// fill's own estimate of the pixels just outside the hole shows how far
/// its tone is from the photograph there; that difference is spread across
/// the whole hole as a smooth membrane, as healing does. The copied detail
/// keeps its contrast (the correction is smooth), and the fill keeps the
/// surroundings' tone all the way in.
///
/// (Measured against the earlier version, which faded this correction out
/// over three quarters of the hole's depth so the middle kept the tone of
/// wherever it was copied from: that left a soft lens-shaped patch of
/// another tone in smooth skies, which a viewer reads as an outline.)
///
/// (Tried and dropped: lifting a clipped selection's fill to the tone of the
/// glow around it. On lacy clipped cloud the fill sits ~15 levels under that
/// glow, which shows as bright outlines; the lift narrowed it by 6 and moved
/// the fill's mean further from the unclipped sky nearby, 0.9 -> 2.3 ΔE.)
/// So with `fade` (clipped selections) the correction fades out over that
/// many pixels from the edge instead.
fn seam_corrected(
    image: &RgbaImage,
    inside: &[bool],
    w: usize,
    (cx0, cy0, cw, ch): (usize, usize, usize, usize),
    syn: &Synthesized,
    fade: Option<f32>,
) -> Vec<[f32; 3]> {
    let depth_in = fade.map(|_| distance_to_known(inside, cw, ch));
    // The ring: known pixels touching the hole.
    let outside: Vec<bool> = inside.iter().map(|b| !b).collect();
    let d_out = distance_to_known(&outside, cw, ch);
    let ring: Vec<bool> = (0..cw * ch)
        .map(|i| !inside[i] && d_out[i] <= 1.5)
        .collect();
    let raw = image.as_raw();
    let mut px = syn.px.clone();
    for c in 0..3 {
        let plane: Vec<f32> = (0..cw * ch)
            .map(|i| {
                if !ring[i] {
                    return 0.0;
                }
                let (x, y) = (i % cw, i / cw);
                raw[((cy0 + y) * w + cx0 + x) * 4 + c] as f32 - syn.ring[i][c]
            })
            .collect();
        let corr = smooth_fill(&plane, &ring, cw, ch);
        for i in 0..cw * ch {
            if inside[i] {
                let k = match (&depth_in, fade) {
                    (Some(d), Some(band)) => (1.0 - d[i] / band).clamp(0.0, 1.0),
                    _ => 1.0,
                };
                px[i][c] += corr[i] * k;
            }
        }
    }
    px
}

/// Rebuilds the hole from the voted gradients (see `vote_gradients`) with
/// the photograph around it as the boundary: a Poisson solve, by
/// over-relaxed red-black Gauss-Seidel from the colour fill as the starting
/// point. The fill then meets the photograph exactly at the selection's
/// edge and has no seams inside where copied regions meet, while keeping
/// all of their detail (Darabi et al., "Image Melding", 2012).
fn poisson_refine(
    px: &mut [[f32; 3]],
    inside: &[bool],
    (gx, gy): (&[[f32; 3]], &[[f32; 3]]),
    w: usize,
    h: usize,
    iterations: usize,
) {
    const OMEGA: f32 = 1.9;
    // The two colours of a checkerboard: each only reads the other, so a
    // colour's cells update in parallel with the same result as in order.
    let cells: [Vec<usize>; 2] = std::array::from_fn(|colour| {
        (0..w * h)
            .filter(|&i| {
                let (x, y) = (i % w, i / w);
                inside[i] && (x + y) % 2 == colour && x > 0 && y > 0 && x + 1 < w && y + 1 < h
            })
            .collect()
    });
    let mut next: Vec<[f32; 3]> = Vec::new();
    for _ in 0..iterations {
        for colour in &cells {
            let cur = &*px;
            colour
                .par_iter()
                .map(|&i| {
                    let mut v = [0.0f32; 3];
                    for c in 0..3 {
                        let sum = (cur[i + 1][c] - gx[i][c])
                            + (cur[i - 1][c] + gx[i - 1][c])
                            + (cur[i + w][c] - gy[i][c])
                            + (cur[i - w][c] + gy[i - w][c]);
                        v[c] = (1.0 - OMEGA) * cur[i][c] + OMEGA * 0.25 * sum;
                    }
                    v
                })
                .collect_into_vec(&mut next);
            for (&i, v) in colour.iter().zip(&next) {
                px[i] = *v;
            }
        }
    }
}

/// Sweeps of the Poisson solve. It starts from the seam-corrected fill, so
/// only seams and steps a few dozen pixels wide are left for it to remove
/// (measured: 100 and 400 sweeps scored alike on the fill test set).
const POISSON_ITERATIONS: usize = 120;

/// Hole pixels filled as one job, with their bounding box.
struct Group {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    members: Vec<u32>,
}

/// Labels groups of hole pixels that lie within `reach` of each other, so
/// neighbouring specks are filled as one job.
fn hole_groups(hole: &[bool], w: usize, h: usize, reach: usize) -> Vec<Group> {
    // Coarse grid of `reach`-sized cells: hole cells connected through
    // 8-neighbourhood form a group.
    let cell = reach.max(1);
    let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
    let mut occupied = vec![false; gw * gh];
    for y in 0..h {
        for x in 0..w {
            if hole[y * w + x] {
                occupied[(y / cell) * gw + x / cell] = true;
            }
        }
    }
    let mut label = vec![0u32; gw * gh];
    let mut next = 0u32;
    let mut stack = Vec::new();
    for start in 0..gw * gh {
        if !occupied[start] || label[start] != 0 {
            continue;
        }
        next += 1;
        label[start] = next;
        stack.push(start);
        while let Some(c) = stack.pop() {
            let (cx, cy) = ((c % gw) as i32, (c / gw) as i32);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx < 0 || ny < 0 || nx >= gw as i32 || ny >= gh as i32 {
                        continue;
                    }
                    let n = ny as usize * gw + nx as usize;
                    if occupied[n] && label[n] == 0 {
                        label[n] = next;
                        stack.push(n);
                    }
                }
            }
        }
    }
    let mut groups: Vec<Group> = (0..next)
        .map(|_| Group {
            x0: usize::MAX,
            y0: usize::MAX,
            x1: 0,
            y1: 0,
            members: Vec::new(),
        })
        .collect();
    for y in 0..h {
        for x in 0..w {
            if hole[y * w + x] {
                let g = &mut groups[label[(y / cell) * gw + x / cell] as usize - 1];
                g.x0 = g.x0.min(x);
                g.y0 = g.y0.min(y);
                g.x1 = g.x1.max(x);
                g.y1 = g.y1.max(y);
                g.members.push((y * w + x) as u32);
            }
        }
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.members.len()));
    groups
}

/// What `fill` says when a selection has nothing around it to copy from.
pub const NOTHING_TO_COPY: &str =
    "Nothing nearby to copy from \u{2014} try a smaller selection or Generative AI";

/// Content-aware fill of every pixel `mask` selects (any non-zero value,
/// grown by two pixels to take in anti-aliased object edges). Returns the
/// whole image with those pixels rebuilt; the caller composites through its
/// own (soft) mask.
///
/// Errors when the mask does not match the image, or when a hole has no
/// usable photograph anywhere in the frame to copy from (it never hands the
/// photo back unchanged as if it had filled it).
pub fn fill(image: &RgbaImage, mask: &GrayImage, options: &Options) -> Result<RgbaImage, String> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let mut out = image.clone();
    if mask.dimensions() != image.dimensions() {
        return Err(format!(
            "Content-aware fill: the selection is {}x{} but the photo is {w}x{h}",
            mask.width(),
            mask.height()
        ));
    }
    // Hole: the selection grown by 2 px.
    let sel: Vec<bool> = mask.as_raw().iter().map(|v| *v > 0).collect();
    let mut hole = sel.clone();
    for _ in 0..2 {
        let prev = hole.clone();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if prev[i] {
                    continue;
                }
                let near = (x > 0 && prev[i - 1])
                    || (x + 1 < w && prev[i + 1])
                    || (y > 0 && prev[i - w])
                    || (y + 1 < h && prev[i + w]);
                if near {
                    hole[i] = true;
                }
            }
        }
    }
    let raw = image.as_raw();
    let usable: Vec<bool> = (0..w * h)
        .map(|i| {
            if hole[i] {
                return false;
            }
            if options.exclude_clipped {
                let p = &raw[i * 4..i * 4 + 3];
                if p[0].min(p[1]).min(p[2]) >= CLIPPED_AT {
                    return false;
                }
            }
            true
        })
        .collect();
    let turned = options.orientation_steps % 2 == 1;

    for (gi, group) in hole_groups(&hole, w, h, 24).into_iter().enumerate() {
        let Group {
            x0: gx0,
            y0: gy0,
            x1: gx1,
            y1: gy1,
            members,
        } = group;
        let span = (gx1 - gx0 + 1).max(gy1 - gy0 + 1);
        // Context: three quarters of the hole's span on every side.
        let mut margin = (span * 3 / 4).clamp(64, 1600);
        let fill_with =
            |margin: usize, out: &RgbaImage| -> Option<(usize, usize, usize, usize, Synthesized)> {
                let cx0 = gx0.saturating_sub(margin);
                let cy0 = gy0.saturating_sub(margin);
                let cx1 = (gx1 + margin).min(w - 1);
                let cy1 = (gy1 + margin).min(h - 1);
                let (cw, ch) = (cx1 - cx0 + 1, cy1 - cy0 + 1);
                let mut px = Vec::with_capacity(cw * ch);
                let mut lhole = vec![false; cw * ch];
                let mut lusable = Vec::with_capacity(cw * ch);
                let oraw = out.as_raw();
                for y in cy0..=cy1 {
                    for x in cx0..=cx1 {
                        let i = y * w + x;
                        px.push([
                            oraw[i * 4] as f32,
                            oraw[i * 4 + 1] as f32,
                            oraw[i * 4 + 2] as f32,
                        ]);
                        lusable.push(usable[i]);
                    }
                }
                for &m in &members {
                    let (x, y) = (m as usize % w, m as usize / w);
                    lhole[(y - cy0) * cw + (x - cx0)] = true;
                }
                if !lusable.iter().any(|u| *u) {
                    return None;
                }
                let level = Level {
                    w: cw,
                    h: ch,
                    px,
                    hole: lhole,
                    usable: lusable,
                    lambda_y: LAMBDA_Y,
                    turned,
                    energy: None,
                    texture_weight: TEXTURE_WEIGHT,
                };
                synthesize(level, mix(options.seed, gi as u64)).map(|p| (cx0, cy0, cw, ch, p))
            };
        let mut result = fill_with(margin, &out);
        // Nothing usable nearby (a selection ringed by clipped white): look
        // further afield, up to the whole frame.
        while result.is_none() && margin < w.max(h) {
            margin = (margin * 2).min(w.max(h));
            result = fill_with(margin, &out);
        }
        let Some((cx0, cy0, cw, ch, syn)) = result else {
            return Err(NOTHING_TO_COPY.to_string());
        };
        let mut inside = vec![false; cw * ch];
        for &m in &members {
            let (x, y) = (m as usize % w - cx0, m as usize / w - cy0);
            inside[y * cw + x] = true;
        }
        // A clipped selection is ringed by glow: matching the fill to that
        // ring all the way in would lift it back toward white (measured:
        // part of a rebuilt sky clipped again). There the correction only
        // joins the edge, fading out over the hole's depth, as before.
        let fade = options.exclude_clipped.then(|| (syn.depth * 0.75).max(8.0));
        let mut px = seam_corrected(&out, &inside, w, (cx0, cy0, cw, ch), &syn, fade);
        if !options.exclude_clipped
            && let Some((gx, gy)) = &syn.grad
        {
            poisson_refine(&mut px, &inside, (gx, gy), cw, ch, POISSON_ITERATIONS);
        }
        for &m in &members {
            let (x, y) = (m as usize % w, m as usize / w);
            let p = px[(y - cy0) * cw + (x - cx0)];
            let a = out.get_pixel(x as u32, y as u32)[3];
            out.put_pixel(
                x as u32,
                y as u32,
                Rgba([
                    p[0].round().clamp(0.0, 255.0) as u8,
                    p[1].round().clamp(0.0, 255.0) as u8,
                    p[2].round().clamp(0.0, 255.0) as u8,
                    a,
                ]),
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    /// Diagonal stripes: a texture with one right answer inside any hole.
    fn stripes(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let v = if ((x + y) / 6) % 2 == 0 { 40 } else { 200 };
            Rgba([v, v / 2 + 20, 255 - v, 255])
        })
    }

    fn square_mask(w: u32, h: u32, x0: u32, y0: u32, s: u32) -> GrayImage {
        GrayImage::from_fn(w, h, |x, y| {
            if x >= x0 && x < x0 + s && y >= y0 && y < y0 + s {
                Luma([255])
            } else {
                Luma([0])
            }
        })
    }

    fn in_mask_error(a: &RgbaImage, b: &RgbaImage, m: &GrayImage) -> f32 {
        let (mut sum, mut n) = (0.0f32, 0.0f32);
        for (x, y, v) in m.enumerate_pixels() {
            if v[0] > 0 {
                let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
                for k in 0..3 {
                    sum += (p[k] as f32 - q[k] as f32).abs();
                }
                n += 3.0;
            }
        }
        sum / n
    }

    #[test]
    fn rebuilds_a_regular_texture() {
        let truth = stripes(160, 120);
        let mask = square_mask(160, 120, 60, 40, 36);
        let mut damaged = truth.clone();
        for (x, y, v) in mask.enumerate_pixels() {
            if v[0] > 0 {
                damaged.put_pixel(x, y, Rgba([255, 0, 255, 255]));
            }
        }
        let filled = fill(
            &damaged,
            &mask,
            &Options {
                seed: 1,
                exclude_clipped: false,
                ..Default::default()
            },
        )
        .unwrap();
        let err = in_mask_error(&filled, &truth, &mask);
        // Blur or a wrong phase would sit near 80 (half the stripe contrast).
        assert!(err < 20.0, "in-mask error {err}");
        // Outside the selection nothing changes.
        for (x, y, v) in mask.enumerate_pixels() {
            if v[0] == 0 {
                assert_eq!(filled.get_pixel(x, y), damaged.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn hole_touching_the_frame_edge_takes_the_neighbouring_region() {
        // Sky (blue noise) over ground (brown noise); a wide hole in the sky
        // that touches the top of the frame must fill with sky, not ground.
        let mut rng = Rng::new(5);
        let (w, h) = (600u32, 300u32);
        let img = RgbaImage::from_fn(w, h, |_, y| {
            let n = (rng.next() % 24) as u8;
            if y < 150 {
                Rgba([110 + n, 160 + n, 220, 255])
            } else {
                Rgba([140 + n, 110 + n, 70 + n / 2, 255])
            }
        });
        let mask = GrayImage::from_fn(w, h, |x, y| {
            let dx = (x as f32 - 330.0) / 200.0;
            let dy = (y as f32 - 20.0) / 110.0;
            Luma([if dx * dx + dy * dy <= 1.0 { 255 } else { 0 }])
        });
        let filled = fill(
            &img,
            &mask,
            &Options {
                seed: 1,
                exclude_clipped: false,
                ..Default::default()
            },
        )
        .unwrap();
        let mut brown = 0;
        let mut n = 0;
        for (x, y, v) in mask.enumerate_pixels() {
            if v[0] > 0 && y < 120 {
                let p = filled.get_pixel(x, y);
                n += 1;
                if p[2] < 150 {
                    brown += 1;
                }
            }
        }
        assert!(
            brown * 50 < n,
            "{brown} of {n} sky pixels filled with ground"
        );
    }

    #[test]
    fn same_seed_same_result_new_seed_differs() {
        // Noise has no single right answer, so seeds must matter.
        let mut rng = Rng::new(7);
        let img = RgbaImage::from_fn(128, 128, |_, _| {
            let v = (rng.next() % 200) as u8 + 20;
            Rgba([v, v, v, 255])
        });
        let mask = square_mask(128, 128, 50, 50, 28);
        let a = fill(
            &img,
            &mask,
            &Options {
                seed: 3,
                exclude_clipped: false,
                ..Default::default()
            },
        )
        .unwrap();
        let b = fill(
            &img,
            &mask,
            &Options {
                seed: 3,
                exclude_clipped: false,
                ..Default::default()
            },
        )
        .unwrap();
        let c = fill(
            &img,
            &mask,
            &Options {
                seed: 4,
                exclude_clipped: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(a.as_raw(), b.as_raw());
        assert_ne!(a.as_raw(), c.as_raw());
    }

    #[test]
    fn never_copies_clipped_white_when_asked() {
        // Left: textured sky-blue. Right: clipped white. The hole straddles
        // the blue; with clipped sources excluded no white may appear.
        let mut rng = Rng::new(11);
        let img = RgbaImage::from_fn(200, 100, |x, _| {
            if x >= 120 {
                Rgba([255, 255, 255, 255])
            } else {
                let n = (rng.next() % 30) as u8;
                Rgba([90 + n, 140 + n, 210 + n / 2, 255])
            }
        });
        let mask = square_mask(200, 100, 70, 30, 40);
        let filled = fill(
            &img,
            &mask,
            &Options {
                seed: 1,
                exclude_clipped: true,
                ..Default::default()
            },
        )
        .unwrap();
        for (x, y, v) in mask.enumerate_pixels() {
            if v[0] > 0 {
                let p = filled.get_pixel(x, y);
                assert!(
                    p[0].min(p[1]).min(p[2]) < 240,
                    "white copied at {x},{y}: {p:?}"
                );
            }
        }
    }

    #[test]
    fn height_prior_follows_the_displayed_vertical() {
        let level = |turned| Level {
            w: 200,
            h: 100,
            px: vec![[0.0; 3]; 200 * 100],
            hole: vec![false; 200 * 100],
            usable: vec![true; 200 * 100],
            lambda_y: LAMBDA_Y,
            turned,
            energy: None,
            texture_weight: 0.0,
        };
        // Upright: a source above costs, one beside is free.
        let upright = level(false);
        assert!(height_prior(&upright, (100, 50), (100, 10)) > 0.0);
        assert_eq!(height_prior(&upright, (100, 50), (20, 50)), 0.0);
        // Turned a quarter: the stored x axis is the displayed vertical.
        let turned = level(true);
        assert_eq!(height_prior(&turned, (100, 50), (100, 10)), 0.0);
        assert!(height_prior(&turned, (100, 50), (20, 50)) > 0.0);
    }

    #[test]
    fn turned_photo_keeps_its_layers_apart() {
        // The frame-edge test above, stored turned a quarter: the sky is
        // the left of the stored image and the hole touches the left edge.
        let mut rng = Rng::new(5);
        let (w, h) = (300u32, 600u32);
        let img = RgbaImage::from_fn(w, h, |x, _| {
            let n = (rng.next() % 24) as u8;
            if x < 150 {
                Rgba([110 + n, 160 + n, 220, 255])
            } else {
                Rgba([140 + n, 110 + n, 70 + n / 2, 255])
            }
        });
        let mask = GrayImage::from_fn(w, h, |x, y| {
            let dx = (x as f32 - 20.0) / 110.0;
            let dy = (y as f32 - 330.0) / 200.0;
            Luma([if dx * dx + dy * dy <= 1.0 { 255 } else { 0 }])
        });
        let filled = fill(
            &img,
            &mask,
            &Options {
                seed: 1,
                orientation_steps: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let (mut brown, mut n) = (0, 0);
        for (x, y, v) in mask.enumerate_pixels() {
            if v[0] > 0 && x < 120 {
                n += 1;
                if filled.get_pixel(x, y)[2] < 150 {
                    brown += 1;
                }
            }
        }
        assert!(
            brown * 50 < n,
            "{brown} of {n} sky pixels filled with ground"
        );
    }

    #[test]
    fn a_soft_surface_is_filled_with_soft_texture() {
        // Left: soft, out-of-focus shading. Right: sharp grain of the same
        // mean tone. A hole in the soft side must not fill with grain.
        let mut rng = Rng::new(3);
        let (w, h) = (240u32, 160u32);
        let img = RgbaImage::from_fn(w, h, |x, y| {
            let v = if x < 120 {
                120.0 + 30.0 * (x as f32 / 9.0).sin() * (y as f32 / 11.0).cos()
            } else {
                60.0 + (rng.next() % 120) as f32
            };
            let v = v as u8;
            Rgba([v, v, v, 255])
        });
        let mask = square_mask(w, h, 40, 50, 60);
        let filled = fill(&img, &mask, &Options::default()).unwrap();
        // Mean absolute Laplacian: grain is ~100, the soft shading ~1.
        let lap = |im: &RgbaImage, x: u32, y: u32| {
            let v = |x: u32, y: u32| im.get_pixel(x, y)[0] as f32;
            (4.0 * v(x, y) - v(x - 1, y) - v(x + 1, y) - v(x, y - 1) - v(x, y + 1)).abs()
        };
        let (mut sum, mut n) = (0.0f32, 0.0f32);
        for y in 52..108 {
            for x in 42..98 {
                sum += lap(&filled, x, y);
                n += 1.0;
            }
        }
        let mean = sum / n;
        assert!(mean < 8.0, "grain copied into a soft surface: {mean}");
    }

    #[test]
    fn nothing_to_copy_is_an_error_not_a_silent_no_op() {
        // All clipped white with clipped sources excluded: no source exists.
        let img = RgbaImage::from_pixel(120, 90, Rgba([255, 255, 255, 255]));
        let mask = square_mask(120, 90, 40, 30, 20);
        let err = fill(
            &img,
            &mask,
            &Options {
                seed: 1,
                exclude_clipped: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(err, NOTHING_TO_COPY);
        // A selection covering the whole frame has nothing outside it.
        let all = GrayImage::from_pixel(120, 90, Luma([255]));
        let img = stripes(120, 90);
        assert!(fill(&img, &all, &Options::default()).is_err());
        // A mismatched mask is an error too, not the photo handed back.
        assert!(fill(&img, &GrayImage::new(10, 10), &Options::default()).is_err());
    }
}
