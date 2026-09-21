//! Lens and film effects for v3: chromatic aberration correction, glow,
//! halation and light flares.
//!
//! Like detail, these are defined by a neighbourhood, so they run on the CPU
//! on the prepared image rather than in the pointwise GPU pass. Correction
//! comes first — it undoes something the lens did, so sharpening should see
//! the corrected edge — and the light effects come after detail, because they
//! add light that sharpening has no business sharpening.
//!
//! **Linear light, before exposure.** Glow, halation and flare are light
//! added to light, so they are computed on linear values and added there,
//! before any tone or colour control. What they respond to, though, is how
//! bright a highlight *will be*: the thresholds are set in post-exposure
//! units, so raising exposure makes more of the picture glow, as it does
//! through a real lens. The shapes and colours are the previous engine's,
//! defined in linear sRGB, and converted to whatever primaries the prepared
//! image is in.
//!
//! **Scale.** Glow, halation and flare are soft, so they are computed on a
//! grid no larger than [`GRID`] pixels on its long edge, and their radii are
//! fractions of that edge. A preview and a full-size export therefore see the
//! same glow, just sampled more finely. The threshold is applied at full
//! resolution before averaging onto the grid, so a small bright point still
//! glows instead of being averaged into its dark surroundings first.

use super::{
    config::Primaries,
    controls::Effects,
    detail::{Gaussian, blur},
    spaces,
};
use rayon::prelude::*;

/// The long edge of the grid the light effects are computed on.
const GRID: usize = 1024;
/// The flare map, in the previous engine's normalised coordinates.
const FLARE_MAP: usize = 256;
/// The previous engine's flare, measured on a photograph of a projected
/// slide, washed the whole screen out at 60; this brings 60 to a flare.
const FLARE_GAIN: f32 = 0.3;

/// The previous engine divided its Centre slider by this.
pub const CENTRE_SCALE: f32 = 250.;

/// Does this leave the picture alone? (Only the spatial parts; Centre's
/// exposure and colour run in the GPU pass.)
pub fn is_neutral(e: &Effects) -> bool {
    e.ca_red_cyan == 0. && e.ca_blue_yellow == 0. && e.centre == 0. && light_is_neutral(e)
}

/// Centre's radial weight, as the shader computes it.
fn centre_weight(x: f32, y: f32, w: f32, h: f32) -> f32 {
    let aspect = h / w;
    let (u, v) = ((x / w - 0.5) * 2., (y / h - 0.5) * 2. * aspect);
    let d = (u * u + v * v).sqrt() * 0.5;
    1. - smoothstep(0.4 - 0.375, 0.4 + 0.375, d)
}

/// Centre's local-contrast half: clarity that is positive in the middle of
/// the frame and negative toward the edges, as the previous engine's was.
/// `clarified` is the image with clarity at the Centre's full strength; this
/// blends toward it by `2m - 1`, which for the edges runs the other way.
pub fn centre_clarity(e: &Effects) -> Option<super::detail::Detail> {
    // The previous engine's clarity strength at full weight was
    // centre * 0.9; v3's clarity slider is strength / 0.8 * 100.
    (e.centre != 0.).then(|| super::detail::Detail {
        clarity: (e.centre / CENTRE_SCALE * 0.9 / 0.8 * 100.).clamp(-100., 100.),
        ..Default::default()
    })
}

pub fn blend_centre(image: &mut image::Rgba32FImage, clarified: &image::Rgba32FImage) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    image
        .as_mut()
        .par_chunks_mut(w * 4)
        .zip(clarified.as_raw().par_chunks(w * 4))
        .enumerate()
        .for_each(|(y, (row, target))| {
            for x in 0..w {
                let k = 2. * centre_weight(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32) - 1.;
                for c in 0..3 {
                    let i = x * 4 + c;
                    row[i] += (target[i] - row[i]) * k;
                }
            }
        });
}

pub fn light_is_neutral(e: &Effects) -> bool {
    e.glow_amount == 0. && e.halation_amount == 0. && e.flare_amount == 0.
}

/// Chromatic aberration correction: red and blue are scaled about the
/// centre of the frame, relative to green. The slider is the previous
/// engine's: ±100 moves a channel by one percent of its distance from the
/// centre, which is the same fraction at any size.
pub fn correct_chromatic_aberration(image: &mut image::Rgba32FImage, e: &Effects) {
    let (red, blue) = (e.ca_red_cyan / 10000., e.ca_blue_yellow / 10000.);
    if red == 0. && blue == 0. {
        return;
    }
    let (w, h) = (image.width() as usize, image.height() as usize);
    let source = image.as_raw().clone();
    let (cx, cy) = (w as f32 / 2., h as f32 / 2.);
    image
        .as_mut()
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let dy = y as f32 + 0.5 - cy;
            for x in 0..w {
                let dx = x as f32 + 0.5 - cx;
                for (channel, amount) in [(0, red), (2, blue)] {
                    if amount != 0. {
                        let k = 1. - amount;
                        row[x * 4 + channel] =
                            bilinear(&source, 4, w, h, cx + dx * k, cy + dy * k, channel);
                    }
                }
            }
        });
}

/// Sample channel `c` of an interleaved image at pixel-centre coordinates,
/// edges repeated.
fn bilinear(plane: &[f32], stride: usize, w: usize, h: usize, x: f32, y: f32, c: usize) -> f32 {
    let x = (x - 0.5).clamp(0., (w - 1) as f32);
    let y = (y - 0.5).clamp(0., (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |x: usize, y: usize| plane[(y * w + x) * stride + c];
    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
    let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
    top + (bottom - top) * fy
}

/// The quadratic soft knee the previous engine used: zero below
/// `threshold - knee`, `x - threshold` above `threshold + knee`, and a
/// parabola joining them with matching slope.
fn soft_excess(x: f32, threshold: f32, knee: f32) -> f32 {
    let t = x - threshold + knee;
    if t <= 0. {
        0.
    } else if t < 2. * knee {
        t * t / (4. * knee)
    } else {
        t - knee
    }
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// Linear luminance to the previous engine's "perceptual" luminance: a 2.2
/// gamma below one, continued above it.
fn perceptual(y: f32) -> f32 {
    if y <= 1. {
        y.max(0.).powf(1. / 2.2)
    } else {
        1. + (y - 1.).powf(1. / 2.2)
    }
}

const SRGB_Y: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn matrix(m: glam::DMat3) -> [[f32; 3]; 3] {
    let m = m.transpose().to_cols_array_2d();
    m.map(|r| r.map(|v| v as f32))
}

fn apply(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

/// The strengths and thresholds, from the sliders, in post-exposure linear
/// sRGB units.
struct Light {
    glow: f32,
    glow_threshold: f32,
    halation: f32,
    halation_threshold: f32,
    flare: f32,
    flare_threshold: f32,
}

impl Light {
    fn new(e: &Effects) -> Self {
        let glow = (e.glow_amount / 100.).clamp(0., 1.);
        let halation = (e.halation_amount / 100.).clamp(0., 1.);
        let flare = (e.flare_amount / 100.).clamp(0., 1.);
        Self {
            glow,
            glow_threshold: 0.85 + (0.3 - 0.85) * glow,
            halation,
            halation_threshold: 0.9 + (0.4 - 0.9) * halation,
            flare,
            flare_threshold: 0.88 + (0.5 - 0.88) * flare,
        }
    }
}

/// Planes on the grid, all in post-exposure linear sRGB.
struct Grid {
    w: usize,
    h: usize,
    /// Full-resolution pixels per grid cell, along each axis.
    step: usize,
    glow: [Vec<f32>; 3],
    halation: Vec<f32>,
    flare: [Vec<f32>; 3],
}

impl Grid {
    /// Threshold at full resolution, then average onto the grid.
    fn build(
        source: &[f32],
        w: usize,
        h: usize,
        to_srgb: &[[f32; 3]; 3],
        gain: f32,
        light: &Light,
    ) -> Self {
        let step = w.max(h).div_ceil(GRID).max(1);
        let (gw, gh) = (w.div_ceil(step), h.div_ceil(step));
        // Seven planes, interleaved per cell while accumulating.
        let mut cells = vec![0.0f32; gw * gh * 7];
        cells
            .par_chunks_mut(gw * 7)
            .enumerate()
            .for_each(|(gy, row)| {
                let mut counts = vec![0u32; gw];
                for y in gy * step..((gy + 1) * step).min(h) {
                    for x in 0..w {
                        let i = (y * w + x) * 4;
                        let rgb = apply(to_srgb, [source[i], source[i + 1], source[i + 2]])
                            .map(|v| v.max(0.) * gain);
                        let luma = dot(rgb, SRGB_Y);
                        let cell = &mut row[(x / step) * 7..(x / step) * 7 + 7];
                        counts[x / step] += 1;
                        if luma <= 1e-6 {
                            continue;
                        }
                        if light.glow > 0. {
                            let t = light.glow_threshold;
                            let k = soft_excess(luma, t, t * 0.5) / luma;
                            for c in 0..3 {
                                cell[c] += rgb[c] * k;
                            }
                        }
                        if light.halation > 0. {
                            let t = light.halation_threshold;
                            cell[3] += soft_excess(luma, t, t * 0.5);
                        }
                        if light.flare > 0. {
                            // Capped, as before: a flare is driven by where
                            // the light is, not by how far past white it goes.
                            let bright =
                                soft_excess(luma.min(1.), light.flare_threshold, 0.15) / luma;
                            for c in 0..3 {
                                cell[4 + c] += rgb[c] * bright;
                            }
                        }
                    }
                }
                for (cell, n) in row.chunks_mut(7).zip(counts) {
                    if n > 0 {
                        cell.iter_mut().for_each(|v| *v /= n as f32);
                    }
                }
            });
        let plane = |c: usize| (0..gw * gh).map(|i| cells[i * 7 + c]).collect::<Vec<_>>();
        Self {
            w: gw,
            h: gh,
            step,
            glow: std::array::from_fn(plane),
            halation: plane(3),
            flare: std::array::from_fn(|c| plane(4 + c)),
        }
    }

    /// A blur whose sigma is `fraction` of the grid's long edge.
    fn blur(&self, plane: &[f32], fraction: f32) -> Vec<f32> {
        let sigma = (fraction * self.w.max(self.h) as f32).max(0.6);
        blur(plane, self.w, self.h, Gaussian::new(sigma))
    }
}

/// Mix two blurs of `plane`: a tight core and a wide spill.
fn two_scale(grid: &Grid, plane: &[f32], core: f32, spill: f32, mix: f32) -> Vec<f32> {
    let a = grid.blur(plane, core);
    let b = grid.blur(plane, spill);
    a.iter().zip(b).map(|(a, b)| a + (b - a) * mix).collect()
}

/// Add glow, halation and flare to an image in `primaries` whose values will
/// be multiplied by `2^exposure` before anything else happens to them.
pub fn add_light(
    image: &mut image::Rgba32FImage,
    e: &Effects,
    exposure: f32,
    primaries: Primaries,
) {
    if light_is_neutral(e) {
        return;
    }
    let light = Light::new(e);
    let gain = exposure.exp2();
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 {
        return;
    }
    let to_srgb = matrix(spaces::conversion(primaries, Primaries::Srgb));
    let from_srgb = matrix(spaces::conversion(Primaries::Srgb, primaries));
    let grid = Grid::build(image.as_raw(), w, h, &to_srgb, gain, &light);

    // Glow: the highlight's own colour, a little warm, spread at two scales.
    let glow: Option<[Vec<f32>; 3]> = (light.glow > 0.).then(|| {
        let warm = [1.03, 1.0, 0.97];
        std::array::from_fn(|c| {
            two_scale(&grid, &grid.glow[c], 0.012, 0.05, 0.45)
                .into_iter()
                .map(|v| v * warm[c] * 1.2 * light.glow)
                .collect()
        })
    });
    // Halation: light that went through the emulsion, reflected off the
    // film base and came back through the red-sensitive layer — a red-orange
    // spill, hottest near the source.
    let halation =
        (light.halation > 0.).then(|| two_scale(&grid, &grid.halation, 0.006, 0.025, 0.6));
    let flare = (light.flare > 0.).then(|| flare_map(&grid, w as f32 / h as f32, light.flare));

    let (gw, gh, step) = (grid.w, grid.h, grid.step as f32);
    image
        .as_mut()
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let gy = (y as f32 + 0.5) / step;
            let v = (y as f32 + 0.5) / h as f32;
            for x in 0..w {
                let px = &mut row[x * 4..x * 4 + 3];
                let rgb = apply(&to_srgb, [px[0], px[1], px[2]]).map(|v| v * gain);
                let luma = dot(rgb.map(|v| v.max(0.)), SRGB_Y);
                let gx = (x as f32 + 0.5) / step;
                let mut added = [0.0f32; 3];
                // Light added to something already far past white changes
                // nothing a display can show; holding it back keeps the
                // brightest areas from being pushed further into clipping.
                let protection = 1. - smoothstep(1.0, 2.2, luma);
                if let Some(glow) = &glow {
                    for c in 0..3 {
                        added[c] += bilinear(&glow[c], 1, gw, gh, gx, gy, 0) * protection;
                    }
                }
                if let Some(halation) = &halation {
                    // Only light that travelled: the spread excess, less
                    // what this spot contributes itself. Inside a large
                    // highlight the two cancel and it stays white; the red
                    // shows where film shows it, around the source.
                    let spread = bilinear(halation, 1, gw, gh, gx, gy, 0);
                    let own = bilinear(&grid.halation, 1, gw, gh, gx, gy, 0);
                    let spill = (spread - own).max(0.);
                    let core = [1.0, 0.15, 0.03];
                    let fringe = [1.0, 0.32, 0.10];
                    let t = smoothstep(0., 0.7, (spill * 2.).min(1.));
                    for c in 0..3 {
                        let tint = fringe[c] + (core[c] - fringe[c]) * t;
                        added[c] += tint * spill * 3.0 * light.halation * protection;
                    }
                }
                if let Some(map) = &flare {
                    let u = (x as f32 + 0.5) / w as f32;
                    let keep = 1. - smoothstep(0.7, 1.8, perceptual(luma));
                    for (c, added) in added.iter_mut().enumerate() {
                        let f = bilinear(
                            map,
                            3,
                            FLARE_MAP,
                            FLARE_MAP,
                            u * FLARE_MAP as f32,
                            v * FLARE_MAP as f32,
                            c,
                        ) * 1.4;
                        *added += f * f * light.flare * keep * FLARE_GAIN;
                    }
                }
                let back = apply(&from_srgb, added);
                for c in 0..3 {
                    px[c] += back[c] / gain;
                }
            }
        });
}

/// The previous engine's flare: a starburst, ghosts reflected through the
/// centre, halos and an anamorphic streak, all driven by the thresholded
/// highlights. Computed on a square map in normalised coordinates, as it was.
fn flare_map(grid: &Grid, aspect: f32, amount: f32) -> Vec<f32> {
    let n = FLARE_MAP;
    // The thresholded highlights, resampled to the map.
    let mut bright = vec![0.0f32; n * n * 3];
    bright
        .par_chunks_mut(n * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..n {
                let gx = (x as f32 + 0.5) / n as f32 * grid.w as f32;
                let gy = (y as f32 + 0.5) / n as f32 * grid.h as f32;
                for c in 0..3 {
                    row[x * 3 + c] = bilinear(&grid.flare[c], 1, grid.w, grid.h, gx, gy, 0);
                }
            }
        });
    let dims = n as f32;
    let sample = |u: f32, v: f32| -> [f32; 3] {
        let x = u.clamp(0., 1.) * dims;
        let y = v.clamp(0., 1.) * dims;
        std::array::from_fn(|c| bilinear(&bright, 3, n, n, x, y, c))
    };
    let inside = |u: f32, v: f32| (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v);
    let mut out = vec![0.0f32; n * n * 3];
    out.par_chunks_mut(n * 3).enumerate().for_each(|(y, row)| {
        for x in 0..n {
            let u = (x as f32 + 0.5) / dims;
            let v = (y as f32 + 0.5) / dims;
            let mut flare = [0.0f32; 3];
            let mut add = |rgb: [f32; 3], tint: [f32; 3], k: f32| {
                for c in 0..3 {
                    flare[c] += rgb[c] * tint[c] * k;
                }
            };
            let direction = |spike: usize| {
                let angle = spike as f32 * std::f32::consts::PI / 6. + std::f32::consts::FRAC_PI_6;
                let (dx, dy) = (angle.cos() / aspect, angle.sin());
                let len = (dx * dx + dy * dy).sqrt();
                (dx / len, dy / len)
            };
            // Long rays, with a little lateral colour.
            let mut rays = [0.0f32; 3];
            for spike in 0..6 {
                let (dx, dy) = direction(spike);
                let (mut ray, mut weights) = ([0.0f32; 3], 0.0f32);
                for i in 1..=24 {
                    let t = i as f32 / 24.;
                    let d = t * t * 0.65;
                    let falloff = (-d * 2.5).exp() + 0.4 * (-d * 0.8).exp();
                    for sign in [1.0f32, -1.0] {
                        let (pu, pv) = (u + sign * dx * d, v + sign * dy * d);
                        if inside(pu, pv) {
                            let r = sample(u + sign * dx * d * 1.01, v + sign * dy * d * 1.01);
                            let g = sample(pu, pv);
                            let b = sample(u + sign * dx * d * 0.99, v + sign * dy * d * 0.99);
                            ray[0] += r[0] * falloff;
                            ray[1] += g[1] * falloff;
                            ray[2] += b[2] * falloff;
                            weights += falloff;
                        }
                    }
                }
                if weights > 0. {
                    for c in 0..3 {
                        rays[c] += ray[c] / weights;
                    }
                }
            }
            add(rays, [1.0, 0.95, 0.85], 3.0 / 6. * 3.5);
            // Short inner rays.
            let mut inner = [0.0f32; 3];
            for spike in 0..6 {
                let (dx, dy) = direction(spike);
                let (mut ray, mut weights) = ([0.0f32; 3], 0.0f32);
                for i in 1..=16 {
                    let d = i as f32 / 16. * 0.2;
                    let falloff = (-d * 8.).exp();
                    for sign in [1.0f32, -1.0] {
                        let (pu, pv) = (u + sign * dx * d, v + sign * dy * d);
                        if inside(pu, pv) {
                            let s = sample(pu, pv);
                            for c in 0..3 {
                                ray[c] += s[c] * falloff;
                            }
                            weights += falloff;
                        }
                    }
                }
                if weights > 0. {
                    for c in 0..3 {
                        inner[c] += ray[c] / weights;
                    }
                }
            }
            add(inner, [1.0, 0.9, 0.8], 2.0 / 6. * 1.5);
            // A soft radial glow.
            {
                let (mut glow, mut weights) = (sample(u, v).map(|s| s * 2.), 2.0f32);
                for ring in 1..=3 {
                    let radius = ring as f32 / 3. * 0.08;
                    let w = (-radius * radius * 200.).exp();
                    for s in 0..12 {
                        let angle = s as f32 * std::f32::consts::TAU / 12. + ring as f32 * 0.5;
                        let (pu, pv) =
                            (u + angle.cos() * radius / aspect, v + angle.sin() * radius);
                        if inside(pu, pv) {
                            let s = sample(pu, pv);
                            for c in 0..3 {
                                glow[c] += s[c] * w;
                            }
                            weights += w;
                        }
                    }
                }
                add(glow.map(|g| g / weights), [1.0, 0.95, 0.9], 0.4);
            }
            let centred = |u: f32, v: f32| {
                let (a, b) = ((u - 0.5) * aspect, v - 0.5);
                (a * a + b * b).sqrt()
            };
            let (fu, fv) = (1. - u, 1. - v);
            let flipped = sample(fu, fv);
            // Iris rings.
            {
                let d = centred(u, v);
                let angle = (v - 0.5).atan2((u - 0.5) * aspect);
                let hex = 0.9 + 0.1 * (angle * 3.).cos().abs().powi(4);
                let mut iris = 0.;
                for (radius, width, k) in [
                    (0.15, 0.02, 0.4),
                    (0.25, 0.025, 0.3),
                    (0.35, 0.03, 0.2),
                    (0.48, 0.035, 0.15),
                ] {
                    iris += (-((d - radius) / width).powi(2)).exp() * k * hex;
                }
                add(flipped, [0.7, 0.8, 1.0], iris * 0.2);
            }
            // Ghosts: the highlights reflected through the centre at
            // several magnifications.
            for (scale, flip, tint, k, inner_edge, outer_edge, bounded) in [
                (0.75, true, [1.0, 0.92, 0.85], 0.05, 0.15, 0.6, false),
                (0.4, true, [0.92, 1.0, 0.95], 0.07, 0.1, 0.45, false),
                (0.2, true, [0.95, 0.97, 1.0], 0.08, 0.08, 0.35, false),
                (0.12, true, [1.0, 1.0, 0.97], 0.07, 0.05, 0.25, false),
                (1.8, false, [0.85, 0.9, 1.0], 0.03, 0.25, 0.75, true),
                (1.3, true, [1.0, 0.9, 0.95], 0.03, 0.2, 0.55, true),
                (0.55, true, [0.97, 0.95, 1.0], 0.04, 0.2, 0.5, false),
            ] {
                let (bu, bv) = if flip { (fu, fv) } else { (u, v) };
                let (gu, gv) = (0.5 + (bu - 0.5) * scale, 0.5 + (bv - 0.5) * scale);
                if bounded && !(gu > 0. && gu < 1. && gv > 0. && gv < 1.) {
                    continue;
                }
                let vignette = 1. - smoothstep(inner_edge, outer_edge, centred(gu, gv));
                add(sample(gu, gv), tint, k * vignette);
            }
            // Halos around the centre.
            {
                let d = centred(u, v);
                for (radius, width, tint, k) in [
                    (0.4, 0.05, [0.85, 0.92, 1.0], 0.07),
                    (0.22, 0.035, [0.92, 0.88, 1.0], 0.05),
                    (0.55, 0.06, [0.85, 0.95, 0.97], 0.03),
                ] {
                    add(flipped, tint, (-((d - radius) / width).powi(2)).exp() * k);
                }
            }
            // A horizontal streak.
            {
                let length = 0.4 / aspect;
                let (mut streak, mut total) = ([0.0f32; 3], 0.0f32);
                for i in 0..64 {
                    let t = i as f32 / 63. * 2. - 1.;
                    let offset = t * length;
                    let w = (-t * t * 3.5).exp();
                    total += w;
                    let su = u + offset;
                    if su > 0. && su < 1. {
                        streak[0] += sample(u + offset * 1.015, v)[0] * w;
                        streak[1] += sample(su, v)[1] * w;
                        streak[2] += sample(u + offset * 0.985, v)[2] * w;
                    }
                }
                add(streak.map(|s| s / total), [0.85, 0.92, 1.0], 1.0);
            }
            for c in 0..3 {
                row[x * 3 + c] = flare[c] * amount * 1.5;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> image::Rgba32FImage {
        image::Rgba32FImage::from_fn(w, h, |x, y| {
            let [r, g, b] = f(x, y);
            image::Rgba([r, g, b, 1.])
        })
    }

    fn effects(f: impl FnOnce(&mut Effects)) -> Effects {
        let mut e = Effects::default();
        f(&mut e);
        e
    }

    /// A bright disc on a dark ground.
    fn disc(w: u32, h: u32, level: f32) -> image::Rgba32FImage {
        frame(w, h, |x, y| {
            let (dx, dy) = (x as f32 - w as f32 / 2., y as f32 - h as f32 / 2.);
            if (dx * dx + dy * dy).sqrt() < w as f32 * 0.03 {
                [level; 3]
            } else {
                [0.02; 3]
            }
        })
    }

    #[test]
    fn neutral_effects_change_nothing() {
        let mut image = disc(200, 150, 4.);
        let before = image.clone();
        correct_chromatic_aberration(&mut image, &Effects::default());
        add_light(&mut image, &Effects::default(), 0., Primaries::Srgb);
        assert_eq!(image, before);
    }

    #[test]
    fn chromatic_aberration_moves_red_and_blue_only() {
        // A vertical edge away from the centre: correction shifts the red and
        // blue edges and leaves green exactly where it was.
        let mut image = frame(400, 100, |x, _| if x < 300 { [0.; 3] } else { [1.; 3] });
        let before = image.clone();
        correct_chromatic_aberration(
            &mut image,
            &effects(|e| {
                e.ca_red_cyan = 100.;
                e.ca_blue_yellow = -100.;
            }),
        );
        let row = |img: &image::Rgba32FImage, c: usize| -> Vec<f32> {
            (0..400).map(|x| img.get_pixel(x, 50)[c]).collect()
        };
        assert_eq!(row(&image, 1), row(&before, 1));
        // 1% of the 100 px from the centre to the edge: one pixel, in
        // opposite directions for the two channels.
        assert!(image.get_pixel(300, 50)[0] < 0.5, "red not pushed outwards");
        assert!(image.get_pixel(299, 50)[2] > 0.5, "blue not pulled inwards");
    }

    #[test]
    fn glow_spreads_light_and_leaves_the_dark_alone_far_away() {
        let mut image = disc(400, 300, 4.);
        let before = image.clone();
        add_light(
            &mut image,
            &effects(|e| e.glow_amount = 60.),
            0.,
            Primaries::Srgb,
        );
        let near = image.get_pixel(200 + 20, 150)[1] - before.get_pixel(220, 150)[1];
        let far = image.get_pixel(5, 5)[1] - before.get_pixel(5, 5)[1];
        assert!(near > 0.01, "no glow beside the highlight: {near}");
        assert!(
            far < near * 0.05,
            "glow reaches the corner: {far} vs {near}"
        );
        // Nothing is ever taken away.
        for (a, b) in image.pixels().zip(before.pixels()) {
            assert!((0..3).all(|c| a[c] >= b[c] - 1e-6));
        }
    }

    #[test]
    fn halation_is_red() {
        let mut image = disc(400, 300, 4.);
        let before = image.clone();
        add_light(
            &mut image,
            &effects(|e| e.halation_amount = 80.),
            0.,
            Primaries::Srgb,
        );
        let (a, b) = (image.get_pixel(222, 150), before.get_pixel(222, 150));
        let added = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        assert!(added[0] > 0.01, "no halation: {added:?}");
        assert!(added[0] > 2. * added[1] && added[1] > added[2], "{added:?}");
    }

    #[test]
    fn halation_fringes_a_large_highlight_instead_of_tinting_it() {
        // A bright window a third of the frame wide: its middle stays the
        // colour it was, and the red appears just outside its edge.
        let window = frame(600, 400, |x, y| {
            if (200..400).contains(&x) && (100..300).contains(&y) {
                [3.; 3]
            } else {
                [0.02; 3]
            }
        });
        let mut image = window.clone();
        add_light(
            &mut image,
            &effects(|e| e.halation_amount = 90.),
            0.,
            Primaries::Srgb,
        );
        let added = |x, y| image.get_pixel(x, y)[0] - window.get_pixel(x, y)[0];
        assert!(
            added(300, 200) < 0.02 * 3.,
            "tinted the middle: {}",
            added(300, 200)
        );
        assert!(added(405, 200) > 0.02, "no fringe: {}", added(405, 200));
    }

    #[test]
    fn exposure_decides_what_glows() {
        // A mid-grey disc does not glow at its own exposure, but does once
        // it is pushed three stops — and the added light scales with it, so
        // the result is the same picture the exposure control would make.
        let dim = disc(400, 300, 0.25);
        let quiet = effects(|e| e.glow_amount = 20.);
        let mut unexposed = dim.clone();
        add_light(&mut unexposed, &quiet, 0., Primaries::Srgb);
        assert!(unexposed.get_pixel(220, 150)[1] - 0.02 < 1e-4);
        let mut exposed = dim.clone();
        add_light(&mut exposed, &quiet, 3., Primaries::Srgb);
        assert!(exposed.get_pixel(220, 150)[1] - 0.02 > 1e-3);
    }

    #[test]
    fn preview_and_export_agree() {
        // The same photograph at two sizes: the glow at a point is the same
        // fraction of the picture's light, so it matches after resampling.
        let big = disc(1600, 1200, 4.);
        let small = disc(400, 300, 4.);
        let e = effects(|e| {
            e.glow_amount = 50.;
            e.halation_amount = 50.;
        });
        let (mut a, mut b) = (big.clone(), small.clone());
        add_light(&mut a, &e, 0., Primaries::Srgb);
        add_light(&mut b, &e, 0., Primaries::Srgb);
        for (x, y) in [(260, 150), (300, 180), (100, 100)] {
            let added_small = b.get_pixel(x, y)[0] - small.get_pixel(x, y)[0];
            let added_big =
                a.get_pixel(x * 4 + 2, y * 4 + 2)[0] - big.get_pixel(x * 4 + 2, y * 4 + 2)[0];
            assert!(
                (added_small - added_big).abs() < 0.1 * added_small.max(added_big) + 2e-3,
                "{x},{y}: {added_small} vs {added_big}"
            );
        }
    }

    #[test]
    fn flare_appears_away_from_a_highlight() {
        // A bright source off-centre throws ghosts across the centre.
        let image = frame(400, 300, |x, y| {
            let (dx, dy) = (x as f32 - 100., y as f32 - 80.);
            if (dx * dx + dy * dy).sqrt() < 8. {
                [8.; 3]
            } else {
                [0.01; 3]
            }
        });
        let mut flared = image.clone();
        add_light(
            &mut flared,
            &effects(|e| e.flare_amount = 80.),
            0.,
            Primaries::Srgb,
        );
        let total: f32 = flared
            .pixels()
            .zip(image.pixels())
            .map(|(a, b)| a[1] - b[1])
            .sum();
        assert!(total > 1., "no flare: {total}");
        // Reflected through the centre: the opposite side gains light.
        let opposite = flared.get_pixel(300, 220)[1] - image.get_pixel(300, 220)[1];
        assert!(opposite > 0., "no ghost opposite the source");
    }

    #[test]
    fn wide_gamut_sources_get_the_same_light() {
        // The effect is defined in linear sRGB: the same picture held in DWG
        // gains the same light, converted.
        let srgb = disc(200, 150, 4.);
        let to_dwg = matrix(spaces::conversion(
            Primaries::Srgb,
            Primaries::DavinciWideGamut,
        ));
        let mut dwg = srgb.clone();
        for p in dwg.pixels_mut() {
            let v = apply(&to_dwg, [p[0], p[1], p[2]]);
            p[0] = v[0];
            p[1] = v[1];
            p[2] = v[2];
        }
        let e = effects(|e| e.halation_amount = 70.);
        let (mut a, mut b) = (srgb.clone(), dwg);
        add_light(&mut a, &e, 0., Primaries::Srgb);
        add_light(&mut b, &e, 0., Primaries::DavinciWideGamut);
        let back = matrix(spaces::conversion(
            Primaries::DavinciWideGamut,
            Primaries::Srgb,
        ));
        let p = b.get_pixel(111, 75);
        let v = apply(&back, [p[0], p[1], p[2]]);
        let q = a.get_pixel(111, 75);
        for c in 0..3 {
            assert!((v[c] - q[c]).abs() < 1e-3, "{c}: {} vs {}", v[c], q[c]);
        }
    }
}
