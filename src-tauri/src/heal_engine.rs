//! Spot healing: where a spot borrows its texture from, and how it is blended in.
//!
//! A heal spot is the soft footprint of one brush stroke. Healing replaces it
//! with texture from elsewhere in the photo (the source) and takes the tone
//! from the spot's own surroundings, the way Lightroom's Heal does.
//!
//! Two things decide whether the repair disappears:
//!
//! - **The source.** Its surroundings must continue the target's structure
//!   (an edge running past the spot has to run past the source the same way),
//!   its inside must look like ordinary texture of that area rather than an
//!   edge or another blemish, and it must not overlap any spot, this one
//!   included. [`find_source`] scores candidates on exactly that, in colour,
//!   around the stroke's real shape rather than its bounding box.
//! - **The tone.** The correction added to the source is solved from a band
//!   of the surroundings, not from the single row of pixels at the stroke's
//!   edge: one dark pixel of a blemish's halo just outside the stroke would
//!   otherwise be pinned into the boundary and smeared across the repair.
//!   Outliers in the band are dropped before the solve.
//!
//! The brush's own soft edge is the blend's alpha, so the stroke's Feather is
//! what the repair's edge does.

use image::{GrayImage, RgbaImage};
use rayon::prelude::*;

/// One spot: the stroke's alpha over the box it touches, in image pixels.
#[derive(Clone, Debug)]
pub struct Spot {
    pub x0: i32,
    pub y0: i32,
    pub w: usize,
    pub h: usize,
    /// 0..1, row-major over the box.
    pub alpha: Vec<f32>,
}

impl Spot {
    /// The spot drawn in `mask` (full frame), or `None` when it is empty.
    pub fn from_mask(mask: &GrayImage) -> Option<Spot> {
        let (w, h) = mask.dimensions();
        let rows: Vec<Option<(u32, u32)>> = (0..h)
            .into_par_iter()
            .map(|y| {
                let mut lo = None;
                let mut hi = 0;
                for x in 0..w {
                    if mask.get_pixel(x, y)[0] > 0 {
                        lo.get_or_insert(x);
                        hi = x;
                    }
                }
                lo.map(|l| (l, hi))
            })
            .collect();
        let (mut x0, mut x1, mut y0, mut y1) = (u32::MAX, 0, u32::MAX, 0);
        for (y, r) in rows.iter().enumerate() {
            if let Some((l, hi)) = r {
                x0 = x0.min(*l);
                x1 = x1.max(*hi);
                y0 = y0.min(y as u32);
                y1 = y1.max(y as u32);
            }
        }
        if x0 == u32::MAX {
            return None;
        }
        let sw = (x1 - x0 + 1) as usize;
        let sh = (y1 - y0 + 1) as usize;
        let mut alpha = vec![0.0; sw * sh];
        for y in 0..sh {
            for x in 0..sw {
                alpha[y * sw + x] = mask.get_pixel(x0 + x as u32, y0 + y as u32)[0] as f32 / 255.0;
            }
        }
        Some(Spot {
            x0: x0 as i32,
            y0: y0 as i32,
            w: sw,
            h: sh,
            alpha,
        })
    }

    /// Alpha at an image pixel (0 outside the box).
    pub fn alpha_at(&self, x: i32, y: i32) -> f32 {
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx < 0 || ly < 0 || lx >= self.w as i32 || ly >= self.h as i32 {
            return 0.0;
        }
        self.alpha[ly as usize * self.w + lx as usize]
    }

    /// Radius of the largest disc inside the footprint: a click spot's
    /// radius, a stroke's half-width.
    fn inner_radius(&self) -> f32 {
        let seed: Vec<bool> = self.alpha.iter().map(|a| *a <= 0.0).collect();
        // Cells beyond the box are outside the footprint too.
        let d = distance_to(&seed, self.w, self.h, true);
        d.iter()
            .zip(&self.alpha)
            .filter(|(_, a)| **a > 0.0)
            .fold(1.0f32, |m, (d, _)| m.max(*d))
    }

    /// Width of the band of surroundings a spot is matched and toned by:
    /// half its radius, and wider for a long thin stroke (a hair), whose
    /// few pixels either side say little about the structure it crosses.
    fn band(&self) -> i32 {
        let long = self.w.max(self.h) as f32;
        ((self.inner_radius() * 0.5).max(long * 0.1).round() as i32).clamp(3, 32)
    }
}

/// Chamfer distance from every cell to the nearest `seed` cell. With
/// `edge_is_seed`, the area beyond the grid counts as seed too.
fn distance_to(seed: &[bool], w: usize, h: usize, edge_is_seed: bool) -> Vec<f32> {
    const FAR: f32 = 1.0e9;
    const D2: f32 = std::f32::consts::SQRT_2;
    let mut d: Vec<f32> = seed.iter().map(|s| if *s { 0.0 } else { FAR }).collect();
    let at = |d: &Vec<f32>, x: i64, y: i64| -> f32 {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            if edge_is_seed { 0.0 } else { FAR }
        } else {
            d[y as usize * w + x as usize]
        }
    };
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let i = y as usize * w + x as usize;
            if d[i] == 0.0 {
                continue;
            }
            let b = d[i]
                .min(at(&d, x - 1, y) + 1.0)
                .min(at(&d, x, y - 1) + 1.0)
                .min(at(&d, x - 1, y - 1) + D2)
                .min(at(&d, x + 1, y - 1) + D2);
            d[i] = b;
        }
    }
    for y in (0..h as i64).rev() {
        for x in (0..w as i64).rev() {
            let i = y as usize * w + x as usize;
            if d[i] == 0.0 {
                continue;
            }
            let b = d[i]
                .min(at(&d, x + 1, y) + 1.0)
                .min(at(&d, x, y + 1) + 1.0)
                .min(at(&d, x + 1, y + 1) + D2)
                .min(at(&d, x - 1, y + 1) + D2);
            d[i] = b;
        }
    }
    d
}

fn px(image: &RgbaImage, x: i32, y: i32) -> [f32; 3] {
    let p = image.get_pixel(x as u32, y as u32);
    [p[0] as f32, p[1] as f32, p[2] as f32]
}

/// Where a spot's matching samples sit, relative to nothing: image pixels.
struct Samples {
    /// Surroundings: the band just outside the stroke.
    ring: Vec<(i32, i32)>,
    /// The stroke itself.
    core: Vec<(i32, i32)>,
}

fn samples(spot: &Spot, band: i32, stride: i32, image_w: i32, image_h: i32) -> Samples {
    let pad = band + 1;
    let (bw, bh) = (spot.w + 2 * pad as usize, spot.h + 2 * pad as usize);
    let mut foot = vec![false; bw * bh];
    for y in 0..spot.h {
        for x in 0..spot.w {
            foot[(y + pad as usize) * bw + x + pad as usize] = spot.alpha[y * spot.w + x] > 0.0;
        }
    }
    let dist = distance_to(&foot, bw, bh, false);
    let mut ring = Vec::new();
    let mut core = Vec::new();
    let mut y = 0;
    while y < bh as i32 {
        let mut x = 0;
        while x < bw as i32 {
            let (ix, iy) = (spot.x0 - pad + x, spot.y0 - pad + y);
            if ix >= 0 && iy >= 0 && ix < image_w && iy < image_h {
                let i = y as usize * bw + x as usize;
                if foot[i] {
                    if spot.alpha_at(ix, iy) > 0.5 {
                        core.push((ix, iy));
                    }
                } else if dist[i] <= band as f32 {
                    ring.push((ix, iy));
                }
            }
            x += stride;
        }
        y += stride;
    }
    Samples { ring, core }
}

fn mean3(v: &[[f32; 3]]) -> [f32; 3] {
    let n = v.len().max(1) as f32;
    let mut m = [0.0; 3];
    for p in v {
        for c in 0..3 {
            m[c] += p[c];
        }
    }
    m.map(|s| s / n)
}

fn dev(p: [f32; 3], m: [f32; 3]) -> f32 {
    ((p[0] - m[0]).abs() + (p[1] - m[1]).abs() + (p[2] - m[2]).abs()) / 3.0
}

/// Picks the offset (source − target, image pixels) a heal spot borrows from.
///
/// `occupied` marks every spot on the photo (this one included): a source
/// that would copy any of them is never taken, and surroundings that touch
/// one are not compared. Candidates near a turned-down source (`avoid`) are
/// skipped; when nothing else fits, the best source is given again.
pub fn find_source(
    image: &RgbaImage,
    spot: &Spot,
    occupied: Option<&GrayImage>,
    avoid: &[(i32, i32)],
) -> (i32, i32) {
    let (iw, ih) = (image.width() as i32, image.height() as i32);
    let r = spot.inner_radius();
    let band = spot.band();
    let stride = ((r / 6.0).round() as i32).max(1);
    let s = samples(spot, band, stride, iw, ih);
    let size = spot.w.max(spot.h) as i32;
    let fallback = (0, -(size + 2 * band).max(16));
    if s.ring.len() < 6 || s.core.is_empty() {
        return fallback;
    }
    let busy = |x: i32, y: i32| occupied.is_some_and(|o| o.get_pixel(x as u32, y as u32)[0] > 0);

    let target: Vec<[f32; 3]> = s.ring.iter().map(|&(x, y)| px(image, x, y)).collect();
    let mu_t = mean3(&target);
    // How much the surroundings vary: the texture a source should carry.
    let sigma_t = target.iter().map(|p| dev(*p, mu_t)).sum::<f32>() / target.len() as f32;

    let score = |dx: i32, dy: i32| -> Option<f32> {
        // The source's inside: never on a spot, never outside the frame.
        let mut inside = Vec::with_capacity(s.core.len());
        for &(x, y) in &s.core {
            let (sx, sy) = (x + dx, y + dy);
            if sx < 0 || sy < 0 || sx >= iw || sy >= ih || busy(sx, sy) {
                return None;
            }
            inside.push(px(image, sx, sy));
        }
        let mut pairs = Vec::with_capacity(s.ring.len());
        for (k, &(x, y)) in s.ring.iter().enumerate() {
            let (sx, sy) = (x + dx, y + dy);
            if sx < 0 || sy < 0 || sx >= iw || sy >= ih {
                return None;
            }
            if busy(x, y) {
                continue;
            }
            if busy(sx, sy) {
                // The source's surroundings run into a spot: its tone there
                // is no guide, and it may be the very thing being removed.
                return None;
            }
            pairs.push((target[k], px(image, sx, sy)));
        }
        if pairs.len() * 4 < s.ring.len() * 3 {
            return None;
        }
        let mt = mean3(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
        let ms = mean3(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
        // Structure around the spot, tone set aside: the heal fixes tone.
        let mut ring = 0.0;
        for (t, q) in &pairs {
            let mut e = 0.0;
            for c in 0..3 {
                e += ((t[c] - mt[c]) - (q[c] - ms[c])).abs();
            }
            ring += e / 3.0;
        }
        ring /= pairs.len() as f32;
        let tone = dev(mt, ms);
        // The source's inside should be ordinary texture of this place: about
        // as busy as the target's surroundings, with nothing standing out
        // (an edge, another blemish).
        let mi = mean3(&inside);
        let amp = inside.iter().map(|p| dev(*p, mi)).sum::<f32>() / inside.len() as f32;
        let limit = 2.5 * sigma_t + 6.0;
        let outlier = inside
            .iter()
            .map(|p| (dev(*p, ms) - limit).max(0.0))
            .sum::<f32>()
            / inside.len() as f32;
        let dist = ((dx * dx + dy * dy) as f32).sqrt() / r.max(1.0);
        Some(ring + 0.25 * tone + 1.5 * outlier + 0.5 * (amp - sigma_t).abs() + 0.08 * dist)
    };

    let reach = (size / 2 + band) as f32;
    let max_shift = ((reach * 6.0) as i32).clamp(48, 1200);
    let clear = (size * 2 / 3).max(12);
    let allowed = |dx: i32, dy: i32| {
        avoid
            .iter()
            .all(|(ax, ay)| (dx - ax).pow(2) + (dy - ay).pow(2) >= clear * clear)
    };

    let search =
        |cx: i32, cy: i32, span: i32, step: i32, skip_avoid: bool| -> Option<(f32, i32, i32)> {
            let rows: Vec<i32> = (-span..=span).step_by(step as usize).collect();
            rows.par_iter()
                .filter_map(|&oy| {
                    let mut best: Option<(f32, i32, i32)> = None;
                    let mut ox = -span;
                    while ox <= span {
                        let (dx, dy) = (cx + ox, cy + oy);
                        if (dx != 0 || dy != 0)
                            && (skip_avoid || allowed(dx, dy))
                            && let Some(sc) = score(dx, dy)
                            && best.is_none_or(|b| sc < b.0)
                        {
                            best = Some((sc, dx, dy));
                        }
                        ox += step;
                    }
                    best
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
        };

    let coarse = ((r / 3.0).round() as i32).clamp(2, 24);
    let best = search(0, 0, max_shift, coarse, false).or_else(|| {
        // Only turned-down sources fit: give the best again.
        (!avoid.is_empty())
            .then(|| search(0, 0, max_shift, coarse, true))
            .flatten()
    });
    let Some(mut best) = best else {
        return fallback;
    };
    let mut step = coarse;
    while step > 1 {
        let fine = (step / 3).max(1);
        if let Some(b) = search(best.1, best.2, step, fine, !allowed(best.1, best.2))
            && b.0 < best.0
        {
            best = b;
        }
        step = fine;
    }
    (best.1, best.2)
}

/// Gaussian blur of a plane, normalised by blurred weights (normalised
/// convolution): the weighted local mean of `v` where `wt` > 0.
fn weighted_blur(v: &[f32], wt: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let rad = (sigma * 2.5).ceil() as i64;
    let k: Vec<f32> = (-rad..=rad)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0;
                for (j, kv) in k.iter().enumerate() {
                    let o = j as i64 - rad;
                    let (sx, sy) = if horizontal {
                        (x as i64 + o, y as i64)
                    } else {
                        (x as i64, y as i64 + o)
                    };
                    if sx >= 0 && sy >= 0 && sx < w as i64 && sy < h as i64 {
                        s += kv * src[sy as usize * w + sx as usize];
                    }
                }
                out[y * w + x] = s;
            }
        }
        out
    };
    let vw: Vec<f32> = v.iter().zip(wt).map(|(a, b)| a * b).collect();
    let num = pass(&pass(&vw, true), false);
    let den = pass(&pass(wt, true), false);
    num.iter()
        .zip(&den)
        .map(|(n, d)| if *d > 1e-4 { n / d } else { 0.0 })
        .collect()
}

/// Heals `spot` in place from `offset` (source − target, image pixels).
///
/// `occupied` marks every spot on the photo: surroundings that lie on one,
/// or whose source pixel does, say nothing about this spot's tone and are
/// left out (a source beside the very object being removed would otherwise
/// tint the repair with it).
pub fn heal_in_place(
    image: &mut RgbaImage,
    spot: &Spot,
    offset: (i32, i32),
    occupied: Option<&GrayImage>,
) {
    let (iw, ih) = (image.width() as i32, image.height() as i32);
    let band = spot.band();
    let pad = band + 2;
    let x0 = (spot.x0 - pad).max(0);
    let y0 = (spot.y0 - pad).max(0);
    let x1 = (spot.x0 + spot.w as i32 + pad).min(iw);
    let y1 = (spot.y0 + spot.h as i32 + pad).min(ih);
    let (bw, bh) = ((x1 - x0) as usize, (y1 - y0) as usize);
    if bw < 3 || bh < 3 {
        return;
    }
    let busy = |x: i32, y: i32| occupied.is_some_and(|o| o.get_pixel(x as u32, y as u32)[0] > 0);
    let mut alpha = vec![0.0f32; bw * bh];
    let mut tgt = vec![[0.0f32; 3]; bw * bh];
    let mut src = vec![[0.0f32; 3]; bw * bh];
    let mut usable = vec![true; bw * bh];
    for y in 0..bh {
        for x in 0..bw {
            let (ix, iy) = (x0 + x as i32, y0 + y as i32);
            let (sx, sy) = (
                (ix + offset.0).clamp(0, iw - 1),
                (iy + offset.1).clamp(0, ih - 1),
            );
            let i = y * bw + x;
            alpha[i] = spot.alpha_at(ix, iy);
            tgt[i] = px(image, ix, iy);
            src[i] = px(image, sx, sy);
            usable[i] = !busy(ix, iy) && !busy(sx, sy);
        }
    }
    let inside: Vec<bool> = alpha.iter().map(|a| *a > 0.0).collect();
    // The surroundings' tone difference, with the odd pixel that does not
    // belong (a blemish's halo, a speck beside the stroke) left out.
    let mut wt: Vec<f32> = inside
        .iter()
        .zip(&usable)
        .map(|(i, u)| if !*i && *u { 1.0 } else { 0.0 })
        .collect();
    if !wt.iter().any(|w| *w > 0.0) {
        wt = inside.iter().map(|i| if *i { 0.0 } else { 1.0 }).collect();
    }
    let sigma = (band as f32 * 0.5).max(1.5);
    let mut planes: Vec<Vec<f32>> = (0..3)
        .map(|c| tgt.iter().zip(&src).map(|(t, s)| t[c] - s[c]).collect())
        .collect();
    let smooth = |wt: &[f32], planes: &[Vec<f32>]| -> Vec<Vec<f32>> {
        planes
            .iter()
            .map(|p| weighted_blur(p, wt, bw, bh, sigma))
            .collect()
    };
    let first = smooth(&wt, &planes);
    let mut resid: Vec<f32> = (0..bw * bh)
        .filter(|i| wt[*i] > 0.0)
        .map(|i| {
            (0..3)
                .map(|c| (planes[c][i] - first[c][i]).abs())
                .fold(0.0, f32::max)
        })
        .collect();
    if !resid.is_empty() {
        let mid = resid.len() / 2;
        let (_, med, _) = resid.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
        let limit = 3.0 * 1.4826 * *med + 8.0;
        for i in 0..bw * bh {
            if wt[i] > 0.0 {
                let r = (0..3)
                    .map(|c| (planes[c][i] - first[c][i]).abs())
                    .fold(0.0, f32::max);
                if r > limit {
                    wt[i] = 0.0;
                }
            }
        }
    }
    let robust = smooth(&wt, &planes);
    for c in 0..3 {
        for i in 0..bw * bh {
            if !inside[i] {
                planes[c][i] = if wt[i] > 0.0 {
                    0.5 * (planes[c][i] + robust[c][i])
                } else {
                    robust[c][i]
                };
            }
        }
    }
    let corr: Vec<Vec<f32>> = planes
        .iter()
        .map(|p| crate::heal_blend::solve_membrane(p, &inside, bw, bh))
        .collect();
    for y in 0..bh {
        for x in 0..bw {
            let i = y * bw + x;
            let a = alpha[i];
            if a <= 0.0 {
                continue;
            }
            let p = image.get_pixel_mut((x0 + x as i32) as u32, (y0 + y as i32) as u32);
            for c in 0..3 {
                let v = src[i][c] + corr[c][i];
                p[c] = (tgt[i][c] * (1.0 - a) + v * a).clamp(0.0, 255.0).round() as u8;
            }
        }
    }
}

/// Chooses where a heal spot should sample from, so the user never has to.
///
/// A heal needs a source — it repairs by borrowing texture from elsewhere —
/// but picking that spot by hand is busywork. This scores candidate offsets
/// by how closely the photo around the *source* matches the photo around
/// the *destination*, which is exactly the condition under which the
/// gradient-domain blend disappears, and rejects any candidate that would
/// sample the hole being repaired. Nearer sources win ties, since texture
/// and lighting drift across a frame.
///
/// Sources already turned down (Refresh source) are kept clear of: no
/// candidate near one of `avoid` is taken. When nothing else fits, the best
/// source is given again.
pub fn legacy_find_source(
    image: &RgbaImage,
    mask: &GrayImage,
    bounds: (u32, u32, u32, u32),
    avoid: &[(i32, i32)],
) -> (i32, i32) {
    let (w, h) = image.dimensions();
    let (bx0, by0, bx1, by1) = bounds;
    let bw = (bx1 - bx0 + 1) as i32;
    let bh = (by1 - by0 + 1) as i32;
    let size = bw.max(bh).max(8);
    let luma = |x: i32, y: i32| -> f32 {
        let p = image.get_pixel(
            x.clamp(0, w as i32 - 1) as u32,
            y.clamp(0, h as i32 - 1) as u32,
        );
        0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
    };

    // Sample the clean photo in a band around the spot. These are the pixels
    // a good source has to agree with.
    let band = (size / 6).clamp(4, 48);
    let stride = (size / 16).max(2);
    let mut ring: Vec<(i32, i32)> = Vec::new();
    let mut y = by0 as i32 - band;
    while y <= by1 as i32 + band {
        let mut x = bx0 as i32 - band;
        while x <= bx1 as i32 + band {
            let inside_core = x > bx0 as i32 && x < bx1 as i32 && y > by0 as i32 && y < by1 as i32;
            let in_frame = x >= 0 && y >= 0 && x < w as i32 && y < h as i32;
            if in_frame && !inside_core && mask.get_pixel(x as u32, y as u32)[0] == 0 {
                ring.push((x, y));
            }
            x += stride;
        }
        y += stride;
    }
    // Grid of spot pixels used to check a candidate does not sample the hole.
    let mut core: Vec<(i32, i32)> = Vec::new();
    let mut y = by0 as i32;
    while y <= by1 as i32 {
        let mut x = bx0 as i32;
        while x <= bx1 as i32 {
            if mask.get_pixel(x as u32, y as u32)[0] > 127 {
                core.push((x, y));
            }
            x += stride;
        }
        y += stride;
    }
    if ring.len() < 8 || core.is_empty() {
        return (0, -size.max(40));
    }

    let score = |dx: i32, dy: i32| -> Option<f32> {
        // Never sample the area being repaired.
        let mut hit = 0usize;
        for (x, y) in &core {
            let (sx, sy) = (x + dx, y + dy);
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                return None;
            }
            if mask.get_pixel(sx as u32, sy as u32)[0] > 0 {
                hit += 1;
            }
        }
        if hit * 50 > core.len() {
            return None;
        }
        let mut err = 0.0f32;
        for (x, y) in &ring {
            let (sx, sy) = (x + dx, y + dy);
            if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
                return None;
            }
            err += (luma(*x, *y) - luma(sx, sy)).abs();
        }
        let mean = err / ring.len() as f32;
        // Mild pull toward nearer sources: lighting and texture drift.
        Some(mean + 0.004 * ((dx * dx + dy * dy) as f32).sqrt())
    };

    let min_shift = (size * 3) / 4;
    let max_shift = size * 3;
    // A turned-down source rules out the candidates that would look the same.
    let clear = (size * 2 / 3).max(12);
    let allowed = |dx: i32, dy: i32| {
        avoid
            .iter()
            .all(|(ax, ay)| (dx - ax).pow(2) + (dy - ay).pow(2) >= clear * clear)
    };
    if !avoid.is_empty() {
        let fresh = legacy_find_source(image, mask, bounds, &[]);
        if allowed(fresh.0, fresh.1) {
            return fresh;
        }
    }
    let mut best: Option<(f32, i32, i32)> = None;
    let coarse = (size / 6).max(8);
    let mut dy = -max_shift;
    while dy <= max_shift {
        let mut dx = -max_shift;
        while dx <= max_shift {
            let far_enough = dx * dx + dy * dy >= min_shift * min_shift && allowed(dx, dy);
            match far_enough.then(|| score(dx, dy)).flatten() {
                Some(sc) if best.is_none_or(|(b, _, _)| sc < b) => best = Some((sc, dx, dy)),
                _ => {}
            }
            dx += coarse;
        }
        dy += coarse;
    }
    let Some((_, cx, cy)) = best else {
        // Nothing but the turned-down sources fits: give the best again.
        return if avoid.is_empty() {
            (0, -size.max(40))
        } else {
            legacy_find_source(image, mask, bounds, &[])
        };
    };

    // Refine around the winner so the answer is not stuck on the coarse grid.
    let fine = (coarse / 3).max(2);
    let mut refined = (f32::MAX, cx, cy);
    let mut dy = cy - coarse;
    while dy <= cy + coarse {
        let mut dx = cx - coarse;
        while dx <= cx + coarse {
            let far_enough = dx * dx + dy * dy >= min_shift * min_shift && allowed(dx, dy);
            match far_enough.then(|| score(dx, dy)).flatten() {
                Some(sc) if sc < refined.0 => refined = (sc, dx, dy),
                _ => {}
            }
            dx += fine;
        }
        dy += fine;
    }
    (refined.1, refined.2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgba};

    /// Horizontal stripes with a dark dot, and a disc stroke over the dot.
    fn striped_with_dot() -> (RgbaImage, GrayImage) {
        let mut img = RgbaImage::from_fn(160, 120, |_, y| {
            let v = if (y / 6) % 2 == 0 { 170 } else { 120 };
            Rgba([v, v, v, 255])
        });
        let mut mask = GrayImage::new(160, 120);
        for y in 0..120i32 {
            for x in 0..160i32 {
                let d = (((x - 80).pow(2) + (y - 60).pow(2)) as f32).sqrt();
                if d < 4.0 {
                    img.put_pixel(x as u32, y as u32, Rgba([20, 20, 20, 255]));
                }
                if d < 9.0 {
                    mask.put_pixel(x as u32, y as u32, Luma([255]));
                }
            }
        }
        (img, mask)
    }

    #[test]
    fn heal_removes_a_dot_and_keeps_the_stripes() {
        let (mut img, mask) = striped_with_dot();
        let spot = Spot::from_mask(&mask).unwrap();
        let off = find_source(&img, &spot, Some(&mask), &[]);
        // The stripes repeat every 12 rows: a source that keeps them lined
        // up is a whole number of periods away vertically.
        assert_eq!(off.1.rem_euclid(12), 0, "source {off:?} breaks the stripes");
        heal_in_place(&mut img, &spot, off, Some(&mask));
        for y in 52..68u32 {
            let want = if (y / 6) % 2 == 0 { 170 } else { 120 };
            let got = img.get_pixel(80, y)[0] as i32;
            assert!((got - want).abs() <= 6, "row {y}: {got} vs {want}");
        }
    }

    #[test]
    fn source_never_lands_on_another_spot() {
        let (img, mask) = striped_with_dot();
        let spot = Spot::from_mask(&mask).unwrap();
        let first = find_source(&img, &spot, Some(&mask), &[]);
        // Mark the first choice's area as another spot: the next pick must
        // keep its inside off it.
        let mut occupied = mask.clone();
        for (x, y, m) in mask.enumerate_pixels() {
            if m[0] > 0 {
                let (sx, sy) = (x as i32 + first.0, y as i32 + first.1);
                occupied.put_pixel(sx as u32, sy as u32, Luma([255]));
            }
        }
        let second = find_source(&img, &spot, Some(&occupied), &[]);
        for (x, y, m) in mask.enumerate_pixels() {
            if m[0] > 127 {
                let (sx, sy) = (x as i32 + second.0, y as i32 + second.1);
                assert_eq!(
                    occupied.get_pixel(sx as u32, sy as u32)[0],
                    0,
                    "{second:?} copies a spot"
                );
            }
        }
    }

    #[test]
    fn refresh_moves_away_from_a_turned_down_source() {
        let (img, mask) = striped_with_dot();
        let spot = Spot::from_mask(&mask).unwrap();
        let first = find_source(&img, &spot, Some(&mask), &[]);
        let second = find_source(&img, &spot, Some(&mask), &[first]);
        let d = ((second.0 - first.0).pow(2) + (second.1 - first.1).pow(2)) as f32;
        assert!(d.sqrt() >= 12.0, "{first:?} then {second:?}");
    }
}
