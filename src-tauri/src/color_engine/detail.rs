//! Sharpening, texture, clarity, structure and noise reduction for v3.
//!
//! Everything else in v3 is pointwise, which is why its GPU pass can walk the
//! image in one-dimensional chunks: no pixel needs its neighbours. Detail is
//! the opposite — every one of these controls is defined by a neighbourhood —
//! so it runs as its own stage, on the prepared image, before the pointwise
//! pass.
//!
//! **What it acts on.** Luminance, in log2. Working on luminance and scaling
//! RGB by the ratio means sharpening cannot put colour fringes on an edge, and
//! working in stops means a control does the same thing in the shadows as in
//! the highlights instead of being dominated by the bright end. Colour noise
//! reduction is the one exception, and acts on chromaticity — the colour with
//! the luminance divided out — so it cannot change brightness at all.
//!
//! **Radii.** The previous engine's: 1, 3.5, 8 and 40 pixels for sharpening,
//! texture, clarity and structure, in full-resolution pixels, scaled with the
//! preview so a control keeps its size relative to the photograph. Sharpening
//! at one pixel cannot be shown faithfully in a small preview — no preview can
//! — so it is only exact at 100%.
//!
//! **The tiling contract.** A large export is processed in horizontal strips,
//! each read with a halo wider than every filter that runs on it, so a strip
//! boundary never changes a pixel: `strips_match_the_whole_image` holds that
//! to within float rounding. Horizontal passes see whole rows either way; the
//! vertical ones accumulate in f64 so starting a running sum at a different
//! row changes nothing a person or an eight-bit file could see.

use anyhow::{Result, ensure};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Radii, in full-resolution pixels, matching the previous engine's.
const SHARPEN_SIGMA: f32 = 1.0;
const TEXTURE_SIGMA: f32 = 3.5;
const CLARITY_SIGMA: f32 = 8.0;
const STRUCTURE_SIGMA: f32 = 40.0;
/// The smallest blur that still does something at preview scale.
const MIN_SIGMA: f32 = 0.6;
/// Offset before the logarithm, so black is a finite number of stops down
/// (about 14 below white) rather than minus infinity.
const LOG_FLOOR: f32 = 1.0 / 16384.0;
/// Mid grey in log2, the centre of clarity's midtone weighting.
const MID_GREY_LOG: f32 = -2.473_931_2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Detail {
    /// -100..100. Negative softens.
    pub sharpening: f32,
    /// 0..80. Detail smaller than this is left unsharpened, so noise is not.
    pub threshold: f32,
    pub texture: f32,
    pub clarity: f32,
    pub structure: f32,
    /// 0..100.
    pub luminance_noise: f32,
    /// 0..100.
    pub color_noise: f32,
}

impl Default for Detail {
    fn default() -> Self {
        Self {
            sharpening: 0.,
            threshold: 15.,
            texture: 0.,
            clarity: 0.,
            structure: 0.,
            luminance_noise: 0.,
            color_noise: 0.,
        }
    }
}

impl Detail {
    pub fn validate(&self) -> Result<()> {
        let within = |v: f32, a: f32, b: f32| v.is_finite() && (a..=b).contains(&v);
        for v in [self.sharpening, self.texture, self.clarity, self.structure] {
            ensure!(
                within(v, -100., 100.),
                "Detail controls must be within -100..100"
            );
        }
        ensure!(
            within(self.threshold, 0., 80.),
            "Sharpening threshold must be within 0..80"
        );
        ensure!(
            within(self.luminance_noise, 0., 100.) && within(self.color_noise, 0., 100.),
            "Noise reduction must be within 0..100"
        );
        Ok(())
    }

    /// The threshold only means something while there is sharpening to gate.
    pub fn is_neutral(&self) -> bool {
        self.sharpening == 0.
            && self.texture == 0.
            && self.clarity == 0.
            && self.structure == 0.
            && self.luminance_noise == 0.
            && self.color_noise == 0.
    }
}

/// Filter sizes for one image scale, fixed before any pixel is touched so the
/// halo can be computed from exactly what will run.
struct Plan {
    sharpen: Option<Gaussian>,
    texture: Option<Gaussian>,
    clarity: Option<Gaussian>,
    structure: Option<Gaussian>,
    luminance_radius: Option<usize>,
    color_radius: Option<usize>,
}

impl Plan {
    fn new(detail: &Detail, scale: f32) -> Self {
        let band = |amount: f32, sigma: f32| {
            (amount != 0.).then(|| Gaussian::new((sigma * scale).max(MIN_SIGMA)))
        };
        Self {
            sharpen: band(detail.sharpening, SHARPEN_SIGMA),
            texture: band(detail.texture, TEXTURE_SIGMA),
            clarity: band(detail.clarity, CLARITY_SIGMA),
            structure: band(detail.structure, STRUCTURE_SIGMA),
            luminance_radius: (detail.luminance_noise > 0.)
                .then(|| ((3.0 * scale).round() as usize).max(1)),
            color_radius: (detail.color_noise > 0.).then(|| {
                (((2.0 + 8.0 * detail.color_noise / 100.) * scale).round() as usize).max(1)
            }),
        }
    }

    /// How far any output pixel can see, in rows. Stages run in sequence, so
    /// their reaches add: the bands read the denoised luminance, which read
    /// its own neighbourhood first. A guided filter reads twice its radius.
    fn halo(&self) -> usize {
        let bands = [&self.sharpen, &self.texture, &self.clarity, &self.structure]
            .into_iter()
            .flatten()
            .map(|g| g.support())
            .max()
            .unwrap_or(0);
        let luminance = self.luminance_radius.map_or(0, |r| 2 * r);
        let color = self.color_radius.map_or(0, |r| 2 * r);
        luminance + color.max(bands)
    }
}

/// A Gaussian approximated by three box passes, which costs the same at any
/// radius — structure's 40 pixels at full resolution included.
#[derive(Clone, Copy)]
struct Gaussian {
    radii: [usize; 3],
}

impl Gaussian {
    fn new(sigma: f32) -> Self {
        let n = 3.0f32;
        let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
        let mut low = ideal.floor() as i32;
        if low % 2 == 0 {
            low -= 1;
        }
        let low = low.max(1);
        let high = low + 2;
        let lowf = low as f32;
        let m = ((12.0 * sigma * sigma - n * lowf * lowf - 4.0 * n * lowf - 3.0 * n)
            / (-4.0 * lowf - 4.0))
            .round() as i32;
        let width = |i: i32| if i < m { low } else { high };
        Self {
            radii: std::array::from_fn(|i| ((width(i as i32) - 1) / 2) as usize),
        }
    }

    fn support(&self) -> usize {
        self.radii.iter().sum()
    }
}

/// Apply `detail` to an image in `primaries`, whose longer edge is `scale`
/// times the full-resolution photograph's.
pub fn apply(
    image: &mut image::Rgba32FImage,
    detail: &Detail,
    luminance_weights: [f32; 3],
    scale: f32,
) {
    if detail.is_neutral() {
        return;
    }
    let (width, height) = image.dimensions();
    // Strips only pay for themselves once a whole-image pass would hold
    // several full-resolution planes at once.
    let strip = if (width as usize) * (height as usize) > 8_000_000 {
        1024
    } else {
        height as usize
    };
    apply_in_strips(image, detail, luminance_weights, scale, strip);
}

fn apply_in_strips(
    image: &mut image::Rgba32FImage,
    detail: &Detail,
    weights: [f32; 3],
    scale: f32,
    strip_rows: usize,
) {
    let plan = Plan::new(detail, scale);
    let (width, height) = (image.width() as usize, image.height() as usize);
    let halo = plan.halo();
    let source = image.as_raw().clone();
    let output = image.as_mut();
    let mut start = 0;
    while start < height {
        let end = (start + strip_rows).min(height);
        let top = start.saturating_sub(halo);
        let bottom = (end + halo).min(height);
        let region = &source[top * width * 4..bottom * width * 4];
        let processed = process(region, width, bottom - top, detail, &plan, weights);
        let skip = (start - top) * width * 4;
        output[start * width * 4..end * width * 4]
            .copy_from_slice(&processed[skip..skip + (end - start) * width * 4]);
        start = end;
    }
}

/// One region, whole. Its edges are treated as image edges, which is exactly
/// why a strip is read with a halo.
fn process(
    rgba: &[f32],
    width: usize,
    height: usize,
    detail: &Detail,
    plan: &Plan,
    weights: [f32; 3],
) -> Vec<f32> {
    let pixels = width * height;
    let luminance: Vec<f32> = (0..pixels)
        .into_par_iter()
        .map(|i| {
            let p = &rgba[i * 4..i * 4 + 3];
            weights[0] * p[0] + weights[1] * p[1] + weights[2] * p[2]
        })
        .collect();
    let log: Vec<f32> = luminance
        .par_iter()
        .map(|y| (y.max(0.0) + LOG_FLOOR).log2())
        .collect();

    // Noise first: sharpening afterwards should not be sharpening the noise.
    let denoised = match plan.luminance_radius {
        Some(radius) => {
            let s = detail.luminance_noise / 100.;
            guided(&log, &log, width, height, radius, (0.3 * s).powi(2) + 1e-8)
        }
        None => log.clone(),
    };

    let mut graded = denoised.clone();
    let mut add_band =
        |gaussian: Option<Gaussian>, amount: f32, limit: f32, midtones: bool, gate: f32| {
            let Some(gaussian) = gaussian else { return };
            let blurred = blur(&denoised, width, height, gaussian);
            graded
                .par_iter_mut()
                .zip(denoised.par_iter())
                .zip(blurred.par_iter())
                .for_each(|((out, l), b)| {
                    let mut d = l - b;
                    if gate > 0.0 {
                        d *= d * d / (d * d + gate * gate);
                    }
                    // A soft limit on how far a band can push a pixel, which is
                    // what keeps a large-radius control from drawing halos.
                    let d = limit * (d / limit).tanh();
                    let weight = if midtones {
                        (-((l - MID_GREY_LOG) / 3.0).powi(2)).exp()
                    } else {
                        1.0
                    };
                    *out += amount * d * weight;
                });
        };
    add_band(
        plan.sharpen,
        detail.sharpening / 100. * 1.5,
        0.5,
        false,
        detail.threshold * 0.004,
    );
    add_band(plan.texture, detail.texture / 100., 0.5, false, 0.0);
    add_band(plan.clarity, detail.clarity / 100. * 0.8, 1.0, true, 0.0);
    add_band(
        plan.structure,
        detail.structure / 100. * 0.6,
        1.0,
        false,
        0.0,
    );

    // Chromaticity: the colour with luminance divided out. Smoothing it
    // cannot change brightness, and a guided filter steered by luminance keeps
    // colour from bleeding across the edges luminance can see.
    let lit = |i: usize| luminance[i] > 1e-6;
    let chroma: Option<[Vec<f32>; 3]> = plan.color_radius.map(|radius| {
        let s = detail.color_noise / 100.;
        std::array::from_fn(|c| {
            let ratio: Vec<f32> = (0..pixels)
                .into_par_iter()
                .map(|i| {
                    if lit(i) {
                        rgba[i * 4 + c] / luminance[i]
                    } else {
                        1.0
                    }
                })
                .collect();
            let smooth = guided(&denoised, &ratio, width, height, radius, 0.01);
            ratio
                .par_iter()
                .zip(smooth.par_iter())
                .map(|(r, q)| r + (q - r) * s)
                .collect()
        })
    });

    let mut out = rgba.to_vec();
    out.par_chunks_mut(4).enumerate().for_each(|(i, px)| {
        if !lit(i) {
            return;
        }
        let y = (graded[i].exp2() - LOG_FLOOR).max(0.0);
        match &chroma {
            Some(q) => {
                for c in 0..3 {
                    px[c] = q[c][i] * y;
                }
            }
            None => {
                let ratio = y / luminance[i];
                for v in px.iter_mut().take(3) {
                    *v *= ratio;
                }
            }
        }
    });
    out
}

/// He, Sun and Tang's guided filter: smooths `input` while keeping the edges
/// that `guide` has. Reaches twice `radius`.
fn guided(guide: &[f32], input: &[f32], w: usize, h: usize, radius: usize, eps: f32) -> Vec<f32> {
    let product: Vec<f32> = guide
        .par_iter()
        .zip(input.par_iter())
        .map(|(a, b)| a * b)
        .collect();
    let square: Vec<f32> = guide.par_iter().map(|a| a * a).collect();
    let mean_g = box_mean(guide, w, h, radius);
    let mean_i = box_mean(input, w, h, radius);
    let mean_gi = box_mean(&product, w, h, radius);
    let mean_gg = box_mean(&square, w, h, radius);
    let a: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|k| {
            let variance = (mean_gg[k] - mean_g[k] * mean_g[k]).max(0.0);
            (mean_gi[k] - mean_g[k] * mean_i[k]) / (variance + eps)
        })
        .collect();
    let b: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|k| mean_i[k] - a[k] * mean_g[k])
        .collect();
    let mean_a = box_mean(&a, w, h, radius);
    let mean_b = box_mean(&b, w, h, radius);
    (0..w * h)
        .into_par_iter()
        .map(|k| mean_a[k] * guide[k] + mean_b[k])
        .collect()
}

fn blur(plane: &[f32], w: usize, h: usize, g: Gaussian) -> Vec<f32> {
    let mut rows = plane.to_vec();
    for r in g.radii {
        rows = box_rows(&rows, w, h, r);
    }
    let mut cols = transpose(&rows, w, h);
    for r in g.radii {
        cols = box_rows(&cols, h, w, r);
    }
    transpose(&cols, h, w)
}

fn box_mean(plane: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let rows = box_rows(plane, w, h, r);
    let cols = box_rows(&transpose(&rows, w, h), h, w, r);
    transpose(&cols, h, w)
}

/// A running-sum box filter along each row, edges repeated. Accumulates in
/// f64 so where the sum starts does not show in the result.
fn box_rows(plane: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return plane.to_vec();
    }
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w)
        .zip(plane.par_chunks(w))
        .take(h)
        .for_each(|(dst, src)| {
            let at = |i: isize| src[i.clamp(0, w as isize - 1) as usize] as f64;
            let r = r as isize;
            let count = (2 * r + 1) as f64;
            let mut sum: f64 = (-r..=r).map(at).sum();
            for (i, d) in dst.iter_mut().enumerate() {
                *d = (sum / count) as f32;
                let i = i as isize;
                sum += at(i + r + 1) - at(i - r);
            }
        });
    out
}

fn transpose(plane: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, column)| {
        for (y, v) in column.iter_mut().enumerate() {
            *v = plane[y * w + x];
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB_Y: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> image::Rgba32FImage {
        image::ImageBuffer::from_fn(w, h, |x, y| {
            let c = f(x, y);
            image::Rgba([c[0], c[1], c[2], 1.0])
        })
    }

    /// Deterministic noise, so tests do not depend on a random seed.
    fn noise(x: u32, y: u32) -> f32 {
        let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2545_F491);
        (h >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    }

    /// Contrast across the step, `reach` pixels either side of it.
    fn edge_contrast_at(img: &image::Rgba32FImage, y: u32, reach: u32) -> f32 {
        let w = img.width();
        img.get_pixel(w / 2 - 1 + reach, y)[1] - img.get_pixel(w / 2 - reach, y)[1]
    }

    fn edge_contrast(img: &image::Rgba32FImage, y: u32) -> f32 {
        edge_contrast_at(img, y, 3)
    }

    #[test]
    fn neutral_is_identity() {
        let mut img = image(32, 16, |x, y| [0.1 + noise(x, y).abs(), 0.2, 0.3]);
        let before = img.clone();
        apply(&mut img, &Detail::default(), SRGB_Y, 1.0);
        assert_eq!(img, before);
    }

    #[test]
    fn a_flat_field_stays_flat_under_every_control() {
        let detail = Detail {
            sharpening: 100.,
            texture: 100.,
            clarity: 100.,
            structure: 100.,
            luminance_noise: 100.,
            color_noise: 100.,
            ..Detail::default()
        };
        let mut img = image(48, 24, |_, _| [0.3, 0.2, 0.1]);
        apply(&mut img, &detail, SRGB_Y, 1.0);
        for p in img.pixels() {
            for (c, want) in [0.3f32, 0.2, 0.1].iter().enumerate() {
                assert!((p[c] - want).abs() < 1e-4, "flat field changed: {p:?}");
            }
        }
    }

    #[test]
    fn sharpening_raises_edge_contrast_and_negative_lowers_it() {
        let edge = |x: u32, _: u32| if x < 32 { [0.1f32; 3] } else { [0.4f32; 3] };
        // One-pixel sharpening acts on the pixels right beside the step.
        let base = edge_contrast_at(&image(64, 8, edge), 4, 1);
        for (amount, sharper) in [(80.0, true), (-80.0, false)] {
            let mut img = image(64, 8, edge);
            let detail = Detail {
                sharpening: amount,
                threshold: 0.,
                ..Detail::default()
            };
            apply(&mut img, &detail, SRGB_Y, 1.0);
            let after = edge_contrast_at(&img, 4, 1);
            assert_eq!(after > base, sharper, "{amount}: {base} -> {after}");
        }
    }

    #[test]
    fn detail_changes_brightness_not_colour() {
        let mut img = image(64, 8, |x, _| {
            if x < 32 {
                [0.2, 0.1, 0.05]
            } else {
                [0.4, 0.2, 0.1]
            }
        });
        let detail = Detail {
            sharpening: 100.,
            clarity: 80.,
            threshold: 0.,
            ..Detail::default()
        };
        apply(&mut img, &detail, SRGB_Y, 1.0);
        for p in img.pixels() {
            // Same 4:2:1 ratio as the source: brightness moved, hue did not.
            assert!(
                (p[0] / p[1] - 2.0).abs() < 1e-3 && (p[1] / p[2] - 2.0).abs() < 1e-3,
                "{p:?}"
            );
        }
    }

    #[test]
    fn the_threshold_leaves_small_detail_unsharpened() {
        let grain = |x: u32, y: u32| [0.2 + 0.004 * noise(x, y); 3];
        let spread = |img: &image::Rgba32FImage| {
            let v: Vec<f32> = img.pixels().map(|p| p[1]).collect();
            let m = v.iter().sum::<f32>() / v.len() as f32;
            (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
        };
        let base = spread(&image(64, 64, grain));
        let run = |threshold: f32| {
            let mut img = image(64, 64, grain);
            apply(
                &mut img,
                &Detail {
                    sharpening: 100.,
                    threshold,
                    ..Detail::default()
                },
                SRGB_Y,
                1.0,
            );
            spread(&img)
        };
        let ungated = run(0.);
        let gated = run(80.);
        assert!(
            ungated > base * 1.3,
            "sharpening should amplify fine grain: {base} -> {ungated}"
        );
        assert!(
            gated < base * 1.1,
            "a high threshold should leave grain alone: {base} -> {gated}"
        );
    }

    #[test]
    fn noise_reduction_quiets_noise_and_keeps_the_edge() {
        let noisy = |x: u32, y: u32| {
            let v = if x < 32 { 0.1 } else { 0.4 } * (1.0 + 0.15 * noise(x, y));
            [v, v, v]
        };
        let variance = |img: &image::Rgba32FImage| {
            let v: Vec<f32> = (0..24).map(|x| img.get_pixel(x, 8)[1]).collect();
            let m = v.iter().sum::<f32>() / v.len() as f32;
            v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32
        };
        let before = image(64, 16, noisy);
        let mut after = before.clone();
        apply(
            &mut after,
            &Detail {
                luminance_noise: 100.,
                ..Detail::default()
            },
            SRGB_Y,
            1.0,
        );
        assert!(
            variance(&after) < variance(&before) * 0.5,
            "noise not reduced"
        );
        assert!(
            edge_contrast(&after, 8) > edge_contrast(&before, 8) * 0.8,
            "edge was blurred away"
        );
    }

    #[test]
    fn colour_noise_reduction_never_moves_luminance() {
        let mut img = image(48, 48, |x, y| {
            let n = noise(x, y) * 0.1;
            [0.2 + n, 0.2 - n * 0.5, 0.2 + n * 0.3]
        });
        let luminance =
            |p: &image::Rgba<f32>| SRGB_Y[0] * p[0] + SRGB_Y[1] * p[1] + SRGB_Y[2] * p[2];
        let before: Vec<f32> = img.pixels().map(luminance).collect();
        apply(
            &mut img,
            &Detail {
                color_noise: 100.,
                ..Detail::default()
            },
            SRGB_Y,
            1.0,
        );
        for (b, p) in before.iter().zip(img.pixels()) {
            assert!(
                (luminance(p) - b).abs() < 2e-5,
                "luminance moved: {b} -> {}",
                luminance(p)
            );
        }
    }

    /// The tiling contract: a strip boundary must never change a pixel.
    #[test]
    fn strips_match_the_whole_image() {
        let detail = Detail {
            sharpening: 60.,
            texture: 40.,
            clarity: 50.,
            structure: 70.,
            luminance_noise: 40.,
            color_noise: 60.,
            threshold: 10.,
        };
        let make = || {
            image(96, 200, |x, y| {
                let n = noise(x, y) * 0.05;
                let base = if (x / 13 + y / 17) % 2 == 0 {
                    0.15
                } else {
                    0.45
                };
                [base + n, base * 0.8 - n * 0.3, base * 0.6 + n * 0.2]
            })
        };
        let mut whole = make();
        apply_in_strips(&mut whole, &detail, SRGB_Y, 1.0, 200);
        for strip in [7, 32, 61] {
            let mut tiled = make();
            apply_in_strips(&mut tiled, &detail, SRGB_Y, 1.0, strip);
            for (a, b) in whole.as_raw().iter().zip(tiled.as_raw()) {
                assert!(
                    (a - b).abs() < 1e-5,
                    "strips of {strip} rows changed a pixel: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn radii_follow_the_preview_scale() {
        let plan = Plan::new(
            &Detail {
                structure: 50.,
                ..Detail::default()
            },
            1.0,
        );
        let small = Plan::new(
            &Detail {
                structure: 50.,
                ..Detail::default()
            },
            0.25,
        );
        let full = plan.structure.unwrap().support() as f32;
        let quarter = small.structure.unwrap().support() as f32;
        assert!((quarter / full - 0.25).abs() < 0.05, "{full} vs {quarter}");
    }

    /// What detail costs on a 33-megapixel export. Ignored by default because
    /// it measures the machine as much as the code:
    /// `cargo test --release --lib full_resolution_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn full_resolution_cost() {
        let make = || image(7008, 4672, |x, y| [0.2 + 0.05 * noise(x, y), 0.2, 0.18]);
        for (name, detail) in [
            (
                "clarity + structure",
                Detail {
                    clarity: 50.,
                    structure: 50.,
                    ..Detail::default()
                },
            ),
            (
                "sharpening",
                Detail {
                    sharpening: 50.,
                    ..Detail::default()
                },
            ),
            (
                "both noise reductions",
                Detail {
                    luminance_noise: 50.,
                    color_noise: 50.,
                    ..Detail::default()
                },
            ),
            (
                "everything",
                Detail {
                    sharpening: 50.,
                    texture: 50.,
                    clarity: 50.,
                    structure: 50.,
                    luminance_noise: 50.,
                    color_noise: 50.,
                    threshold: 15.,
                },
            ),
        ] {
            let mut img = make();
            let start = std::time::Instant::now();
            apply(&mut img, &detail, SRGB_Y, 1.0);
            println!(
                "{name:24} {:>7.0} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}
