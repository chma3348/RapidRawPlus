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
//! **Radii.** In full-resolution pixels, scaled with the preview so a control
//! keeps its size relative to the photograph. Sharpening at one pixel cannot
//! be shown faithfully in a small preview — no preview can — so it is only
//! exact at 100%.
//!
//! **Sharpening and noise reduction are Lightroom's**, slider for slider
//! (Amount, Radius, Detail and Masking; Color and Luminance noise reduction
//! with their Detail, Smoothness and Contrast), fitted to its exports by
//! tools/fit_sharpen.py (sharpen_table.rs).
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
const STRUCTURE_SIGMA: f32 = 40.0;
/// Negative Dehaze's scattering blurs, as the texture and structure radii.
const HAZE_FINE_SIGMA: f32 = 3.5;
const HAZE_BROAD_SIGMA: f32 = 120.0;
/// Sharpening's tone weighting judges each pixel's tone from the luminance
/// blurred this much, so the weight is as smooth as the picture.
const TONE_SIGMA: f32 = 2.0;
/// Colour noise is judged against a local mean this wide (full-resolution
/// pixels): high-ISO colour noise comes in blotches several pixels across.
const NOISE_RADIUS: f32 = 4.0;
/// How the colour noise judged at a preview's size follows the scale
/// (measured on an ISO 12800 frame: 1.3x the full-size figure at 0.43).
const PREVIEW_NOISE_POWER: f32 = -0.31;
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
    /// -100..150: Lightroom's Sharpening Amount (40 is its default for a
    /// RAW). Negative softens.
    pub sharpening: f32,
    /// 0.5..3.0: Lightroom's Radius, the size of the detail sharpened.
    pub sharpen_radius: f32,
    /// 0..100: Lightroom's Detail. Low holds back the halos on strong edges;
    /// high sharpens the finest texture harder.
    pub sharpen_detail: f32,
    /// 0..100: Lightroom's Masking. Higher leaves smooth areas unsharpened
    /// and sharpens only edges.
    pub sharpen_masking: f32,
    /// 0..80. Detail smaller than this is left unsharpened, so noise is not.
    pub threshold: f32,
    pub texture: f32,
    pub clarity: f32,
    pub structure: f32,
    /// 0..100: Lightroom's (luminance) Noise Reduction.
    pub luminance_noise: f32,
    /// 0..100: its Detail. Higher keeps more fine detail.
    pub luminance_noise_detail: f32,
    /// 0..100: its Contrast. Higher keeps more local contrast.
    pub luminance_noise_contrast: f32,
    /// 0..100: Lightroom's Color Noise Reduction (25 is its default for a
    /// RAW).
    pub color_noise: f32,
    /// 0..100: its Detail. Higher keeps small colour detail.
    pub color_noise_detail: f32,
    /// 0..100: its Smoothness. Higher also smooths broad colour mottling.
    pub color_noise_smoothness: f32,
    /// -100..100. Positive removes haze, negative adds it.
    pub dehaze: f32,
}

impl Default for Detail {
    fn default() -> Self {
        Self {
            sharpening: 0.,
            sharpen_radius: 1.,
            sharpen_detail: 25.,
            sharpen_masking: 0.,
            threshold: 0.,
            texture: 0.,
            clarity: 0.,
            structure: 0.,
            luminance_noise: 0.,
            luminance_noise_detail: 50.,
            luminance_noise_contrast: 0.,
            color_noise: 0.,
            color_noise_detail: 50.,
            color_noise_smoothness: 50.,
            dehaze: 0.,
        }
    }
}

impl Detail {
    pub fn validate(&self) -> Result<()> {
        let within = |v: f32, a: f32, b: f32| v.is_finite() && (a..=b).contains(&v);
        ensure!(
            within(self.sharpening, -100., 150.),
            "Sharpening must be within -100..150"
        );
        ensure!(
            within(self.sharpen_radius, 0.5, 3.0),
            "Sharpening radius must be within 0.5..3"
        );
        for v in [
            self.sharpen_detail,
            self.sharpen_masking,
            self.luminance_noise_detail,
            self.luminance_noise_contrast,
            self.color_noise_detail,
            self.color_noise_smoothness,
        ] {
            ensure!(
                within(v, 0., 100.),
                "Sharpening and noise reduction settings must be within 0..100"
            );
        }
        for v in [self.texture, self.clarity, self.structure, self.dehaze] {
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

    /// A RAW opens as in Lightroom, with its default Sharpening (40) and
    /// Color Noise Reduction (25), wherever `given` (the saved `v3.detail`)
    /// leaves them unset. A rendered photograph opens with neither.
    pub fn with_raw_defaults(mut self, given: &serde_json::Value) -> Self {
        let unset = |key: &str| given.get(key).is_none_or(|v| v.is_null());
        if unset("sharpening") {
            self.sharpening = 40.;
        }
        if unset("color_noise") {
            self.color_noise = 25.;
        }
        self
    }

    /// The threshold only means something while there is sharpening to gate.
    pub fn is_neutral(&self) -> bool {
        self.sharpening == 0.
            && self.texture == 0.
            && self.clarity == 0.
            && self.structure == 0.
            && self.luminance_noise == 0.
            && self.color_noise == 0.
            && self.dehaze == 0.
    }
}

/// Filter sizes for one image scale, fixed before any pixel is touched so the
/// halo can be computed from exactly what will run.
struct Plan {
    sharpen: Option<Sharpen>,
    /// Negative sharpening: one-pixel softening.
    soften: Option<Gaussian>,
    /// Texture and Clarity: local contrast at several sizes, each band with
    /// its own amount (detail_table.rs, fitted to Lightroom's).
    texture: Vec<(Gaussian, f32)>,
    clarity: Vec<(Gaussian, f32)>,
    /// Positive Dehaze's local contrast beyond its haze removal (Lightroom's
    /// raises contrast at every scale, the broadest included).
    dehaze_bands: Vec<(Gaussian, f32)>,
    structure: Option<Gaussian>,
    luminance: Option<LuminanceNr>,
    colour: Option<ColourNr>,
    /// Dehaze: the dark channel's minimum-filter radius and the guided
    /// filter's radius that refines the transmission map.
    dehaze: Option<(usize, usize)>,
    /// Negative Dehaze's scattering: the fine and broad blurs light is mixed
    /// toward, and how much of each (its veil curve and colour are the
    /// grading pass's).
    haze: Option<(Gaussian, Gaussian, [f32; 2])>,
}

/// Lightroom's sharpening at one scale: bands (the log luminance less its
/// exact Gaussian blur at `sigma` preview pixels) with their amounts, the
/// soft limit (stops) on their sum, and the edge mask's thresholds (log2
/// per full-resolution pixel).
struct Sharpen {
    bands: Vec<(f32, f32)>,
    limit: f32,
    mask: Option<[f32; 2]>,
    scale: f32,
}

impl Sharpen {
    fn new(detail: &Detail, scale: f32) -> Option<Self> {
        use super::sharpen_table as t;
        if detail.sharpening <= 0. {
            return None;
        }
        // Lightroom was measured at Detail 25 and 75; between and beyond them
        // the kernel's shape and the limit move in proportion.
        let along = ((detail.sharpen_detail - 25.) / 50.).clamp(-0.5, 1.5);
        let strength = (detail.sharpening / 40.).powf(t::POWER);
        let full: Vec<(f32, f32)> = (0..3)
            .map(|j| {
                let shape = t::D25[j] + (t::D75[j] - t::D25[j]) * along;
                (t::SIGMAS[j] * detail.sharpen_radius, shape * strength)
            })
            .collect();
        let bands = if scale >= 0.999 {
            full
        } else {
            preview_bands(&full, scale)
        };
        let limit = (t::D25_LIMIT.ln() + (t::D75_LIMIT.ln() - t::D25_LIMIT.ln()) * along).exp();
        let mask = (detail.sharpen_masking > 0.)
            .then(|| t::MASKING_50.map(|v| v * detail.sharpen_masking / 50.));
        Some(Self {
            bands,
            limit,
            mask,
            scale,
        })
    }

    fn reach(&self) -> usize {
        let bands = self
            .bands
            .iter()
            .map(|&(s, _)| exact_radius(s))
            .max()
            .unwrap_or(0);
        let mask = self.mask.map_or(0, |_| exact_radius(self.scale) + 1);
        bands.max(mask).max(exact_radius(TONE_SIGMA * self.scale))
    }
}

/// Sharpening for a preview smaller than the photograph. Its bands shrink
/// below a pixel there, and a blur that small does almost nothing, so the
/// preview would look softer than the full-size result shrunk (measured: at
/// 0.43 of the size, 1.15x on the finest preview detail against Lightroom's
/// 1.41x). Instead: the full-size bands' response at the frequencies the
/// preview can show, `full` at frequency f x scale, matched (least squares)
/// by bands the preview can hold, at the same sizes but none under half a
/// preview pixel.
fn preview_bands(full: &[(f32, f32)], scale: f32) -> Vec<(f32, f32)> {
    let response = |sigma: f32, f: f32| {
        // 1 minus the sampled kernel's response: what a band passes.
        let r = exact_radius(sigma);
        if r == 0 || sigma <= 0.0 {
            return 0.0;
        }
        let (mut sum, mut at) = (0.0f32, 0.0f32);
        for n in -(r as i32)..=(r as i32) {
            let w = (-(n * n) as f32 / (2.0 * sigma * sigma)).exp();
            sum += w;
            at += w * (std::f32::consts::TAU * f * n as f32).cos();
        }
        1.0 - at / sum
    };
    let sigmas: Vec<f32> = full
        .iter()
        .enumerate()
        .map(|(j, &(s, _))| (s * scale).max(0.5 * (j + 1) as f32))
        .collect();
    let freqs: Vec<f32> = (1..=40).map(|i| i as f32 / 80.0).collect();
    let target: Vec<f32> = freqs
        .iter()
        .map(|&f| full.iter().map(|&(s, a)| a * response(s, f * scale)).sum())
        .collect();
    // Normal equations, with a little ridge so near-equal bands stay tame.
    let n = sigmas.len();
    let basis: Vec<Vec<f32>> = sigmas
        .iter()
        .map(|&s| freqs.iter().map(|&f| response(s, f)).collect())
        .collect();
    let mut m = vec![vec![0f64; n + 1]; n];
    for i in 0..n {
        for j in 0..n {
            m[i][j] = basis[i]
                .iter()
                .zip(&basis[j])
                .map(|(a, b)| f64::from(a * b))
                .sum();
        }
        m[i][i] += 1e-4;
        m[i][n] = basis[i]
            .iter()
            .zip(&target)
            .map(|(a, b)| f64::from(a * b))
            .sum();
    }
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))
            .unwrap();
        m.swap(col, pivot);
        for row in 0..n {
            if row != col && m[col][col].abs() > 1e-12 {
                let k = m[row][col] / m[col][col];
                let pivot_row = m[col].clone();
                for (v, p) in m[row].iter_mut().zip(&pivot_row).skip(col) {
                    *v -= k * p;
                }
            }
        }
    }
    sigmas
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let a = if m[i][i].abs() > 1e-12 {
                m[i][n] / m[i][i]
            } else {
                0.0
            };
            (s, a as f32)
        })
        .collect()
}

/// Luminance noise reduction: a guided filter on the log luminance, with
/// some of the local contrast it flattened given back (Contrast).
struct LuminanceNr {
    radius: usize,
    eps: f32,
    contrast: Option<(Gaussian, f32)>,
}

/// Colour noise reduction, as Lightroom's. It smooths opponent colour in
/// cube-root light (as Lab's a* and b* see it, so brightness noise cannot
/// leave colour flicker behind) and rebuilds each pixel at its own
/// luminance. A fine stage (a guided filter steered by luminance), then an
/// adaptive one that smooths colour variation within the photo's own colour
/// noise over a wide area while keeping colour edges (a guided filter
/// steered by the colour itself). Measured: on an ISO 12800 photo
/// Lightroom's 25 removes 95% of colour variation out to 8 px; on a clean
/// one, mostly the single-pixel speckle.
struct ColourNr {
    radius: usize,
    eps: f32,
    mix: f32,
    /// The adaptive stage's radius, how many times the noise counts as
    /// noise (growing as (noise / 0.01) ^ power: Lightroom's is
    /// disproportionately stronger on a noisy photo), how much is mixed in.
    adaptive: (usize, f32, f32, f32),
    /// The noise estimate's neighbourhood.
    noise_radius: usize,
    /// Blur of the colour guide: one full-resolution pixel.
    guide_sigma: f32,
}

impl ColourNr {
    fn new(detail: &Detail, scale: f32) -> Option<Self> {
        if detail.color_noise <= 0. {
            return None;
        }
        // Measured at Lightroom's 25 with Detail and Smoothness at 50. Beyond
        // 25 what counts as noise grows with the square root of the amount
        // and the mixes close on all of it; below, the mixes scale down.
        // Detail raises the bar for colour to be kept as detail (lower keeps
        // less); Smoothness widens the adaptive stage. Not yet measured
        // against Lightroom away from 25 / 50 / 50.
        let [radius, eps, mix, radius2, k, mix2, power] = super::sharpen_table::COLOUR_25;
        let c = detail.color_noise / 25.;
        let toward = |m: f32| if c <= 1. { m * c } else { 1. - (1. - m) / c };
        let detail_factor = 2f32.powf((50. - detail.color_noise_detail) / 50.);
        let smooth = 0.5 + detail.color_noise_smoothness / 100.;
        Some(Self {
            radius: ((radius * scale).round() as usize).max(1),
            eps: eps * detail_factor * detail_factor,
            mix: toward(mix),
            adaptive: (
                ((radius2 * smooth * scale).round() as usize).max(1),
                k * c.max(1.).sqrt() * detail_factor,
                toward(mix2),
                power,
            ),
            guide_sigma: scale,
            noise_radius: ((NOISE_RADIUS * scale).round() as usize).max(1),
        })
    }
}

impl Plan {
    fn new(detail: &Detail, scale: f32) -> Self {
        let band = |amount: f32, sigma: f32| {
            (amount != 0.).then(|| Gaussian::new((sigma * scale).max(MIN_SIGMA)))
        };
        Self {
            sharpen: Sharpen::new(detail, scale),
            soften: band(detail.sharpening.min(0.), SHARPEN_SIGMA),
            texture: bands(
                detail.texture,
                &super::detail_table::TEXTURE_SIGMAS,
                &super::detail_table::TEXTURE,
                scale,
            ),
            clarity: bands(
                detail.clarity,
                &super::detail_table::CLARITY_SIGMAS,
                &super::detail_table::CLARITY,
                scale,
            ),
            dehaze_bands: bands(
                detail.dehaze.max(0.),
                &super::detail_table::DEHAZE_SIGMAS,
                &super::detail_table::DEHAZE,
                scale,
            ),
            structure: band(detail.structure, STRUCTURE_SIGMA),
            luminance: (detail.luminance_noise > 0.).then(|| {
                let s = detail.luminance_noise / 100.;
                LuminanceNr {
                    radius: ((3.0 * scale).round() as usize).max(1),
                    eps: (0.3 * s).powi(2) * 2f32.powf((50. - detail.luminance_noise_detail) / 25.)
                        + 1e-8,
                    contrast: (detail.luminance_noise_contrast > 0.).then(|| {
                        (
                            Gaussian::new((1.5 * scale).max(MIN_SIGMA)),
                            0.5 * detail.luminance_noise_contrast / 100.,
                        )
                    }),
                }
            }),
            colour: ColourNr::new(detail, scale),
            dehaze: (detail.dehaze > 0.).then(|| {
                (
                    ((15.0 * scale).round() as usize).max(1),
                    ((30.0 * scale).round() as usize).max(1),
                )
            }),
            haze: (detail.dehaze < 0.).then(|| {
                let a = (-detail.dehaze / 100.).min(1.0);
                let [half, full] = super::dehaze_table::SCATTER;
                let mix: [f32; 2] = if a <= 0.5 {
                    std::array::from_fn(|i| half[i] * a * 2.0)
                } else {
                    std::array::from_fn(|i| half[i] + (full[i] - half[i]) * (a * 2.0 - 1.0))
                };
                (
                    Gaussian::new((HAZE_FINE_SIGMA * scale).max(MIN_SIGMA)),
                    Gaussian::new((HAZE_BROAD_SIGMA * scale).max(MIN_SIGMA)),
                    mix,
                )
            }),
        }
    }

    /// How far any output pixel can see, in rows. Stages run in sequence, so
    /// their reaches add: the bands read the denoised luminance, which read
    /// its own neighbourhood first. A guided filter reads twice its radius.
    fn halo(&self) -> usize {
        let bands = [&self.soften, &self.structure]
            .into_iter()
            .flatten()
            .chain(
                self.texture
                    .iter()
                    .chain(&self.clarity)
                    .chain(&self.dehaze_bands)
                    .map(|(g, _)| g),
            )
            .map(|g| g.support())
            .max()
            .unwrap_or(0)
            .max(self.sharpen.as_ref().map_or(0, Sharpen::reach));
        let luminance = self.luminance.as_ref().map_or(0, |n| {
            2 * n.radius + n.contrast.map_or(0, |(g, _)| g.support())
        });
        let color = self.colour.as_ref().map_or(0, |n| {
            2 * n.radius + exact_radius(n.guide_sigma) + 2 * n.adaptive.0
        });
        // Dehaze runs first and everything after reads its result.
        let dehaze = self.dehaze.map_or(0, |(min, guide)| min + 2 * guide);
        let haze = self.haze.map_or(0, |(_, broad, _)| broad.support());
        haze + dehaze + luminance + color.max(bands)
    }
}

/// A control's bands at slider `value` (-100..100): each band's amount from
/// the table's +50, +100, -50 and -100 columns, straight between them and
/// toward zero (Lightroom's 50 is not half its 100).
fn bands<const N: usize>(
    value: f32,
    sigmas: &[f32; N],
    table: &[[f32; 4]; N],
    scale: f32,
) -> Vec<(Gaussian, f32)> {
    if value == 0. {
        return Vec::new();
    }
    let a = (value.abs() / 100.).min(1.0);
    let (half, full) = if value > 0. { (0, 1) } else { (2, 3) };
    (0..N)
        .map(|b| {
            let amount = if a <= 0.5 {
                table[b][half] * a * 2.0
            } else {
                table[b][half] + (table[b][full] - table[b][half]) * (a * 2.0 - 1.0)
            };
            (Gaussian::new((sigmas[b] * scale).max(MIN_SIGMA)), amount)
        })
        .collect()
}

/// A Gaussian approximated by three box passes, which costs the same at any
/// radius — structure's 40 pixels at full resolution included.
#[derive(Clone, Copy)]
pub(super) struct Gaussian {
    radii: [usize; 3],
}

impl Gaussian {
    pub(super) fn new(sigma: f32) -> Self {
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
/// `photo_noise` is the photograph's own colour noise
/// (`photo_colour_noise`, at full resolution), so every render of it reduces
/// it alike whatever its size; measured on `image` when not given.
pub fn apply(
    image: &mut image::Rgba32FImage,
    detail: &Detail,
    luminance_weights: [f32; 3],
    scale: f32,
    photo_noise: Option<f32>,
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
    apply_in_strips(image, detail, luminance_weights, scale, strip, photo_noise);
}

/// The photograph's colour noise at full resolution: `chroma_noise` over a
/// grid of samples (a whole frame's statistic, and stable from a few
/// hundred thousand pixels), measured once per photo.
pub fn photo_colour_noise(image: &image::Rgba32FImage) -> f32 {
    const SIDE: u32 = 384;
    let (w, h) = image.dimensions();
    if w <= SIDE * 2 || h <= SIDE * 2 {
        return chroma_noise(
            image.as_raw(),
            w as usize,
            h as usize,
            1.0,
            NOISE_RADIUS as usize,
        );
    }
    let mut samples = Vec::new();
    for gy in 0..3 {
        for gx in 0..3 {
            let x = (w - SIDE) * (2 * gx + 1) / 6;
            let y = (h - SIDE) * (2 * gy + 1) / 6;
            let crop = image::imageops::crop_imm(image, x, y, SIDE, SIDE).to_image();
            samples.push(chroma_noise(
                crop.as_raw(),
                SIDE as usize,
                SIDE as usize,
                1.0,
                NOISE_RADIUS as usize,
            ));
        }
    }
    samples.sort_by(f32::total_cmp);
    samples[samples.len() / 2]
}

fn apply_in_strips(
    image: &mut image::Rgba32FImage,
    detail: &Detail,
    weights: [f32; 3],
    scale: f32,
    strip_rows: usize,
    photo_noise: Option<f32>,
) {
    let plan = Plan::new(detail, scale);
    let (width, height) = (image.width() as usize, image.height() as usize);
    let halo = plan.halo();
    // The colour of the haze belongs to the whole photograph. Estimated per
    // strip, two strips would disagree about it and the seam would show.
    let airlight = plan.dehaze.map(|_| airlight(image.as_raw()));
    // So is its colour noise, which colour noise reduction adapts to. It is
    // judged at this size (`here`) and for the photograph (`photo`), and the
    // two follow from the photograph's own noise by the same rule at every
    // size, so a preview, a zoomed region and an export of one photo never
    // disagree. The rule is what measuring at each size gave (the noise
    // filter's own reach shrinks with the preview), which is also what makes
    // a preview match Lightroom's full-size result shrunk.
    let noise = plan.colour.as_ref().map(|nr| {
        let s = scale.min(1.0);
        let here = match photo_noise {
            Some(photo) => photo * s.powf(PREVIEW_NOISE_POWER),
            None => chroma_noise(
                image.as_raw(),
                width,
                height,
                nr.guide_sigma,
                nr.noise_radius,
            ),
        };
        (here, here / s)
    });
    let row = width * 4;
    if strip_rows >= height {
        let processed = process(
            image.as_raw(),
            width,
            height,
            detail,
            &plan,
            weights,
            airlight,
            noise,
        );
        image.as_mut().copy_from_slice(&processed);
        return;
    }
    // Each strip is written back as soon as it is done, so the picture is
    // never copied whole. The rows the next strip still has to read as they
    // were (its upper halo) are kept aside before they are overwritten.
    let mut kept: Vec<f32> = Vec::new();
    let mut kept_from = 0;
    let mut start = 0;
    while start < height {
        let end = (start + strip_rows).min(height);
        let top = start.saturating_sub(halo);
        let bottom = (end + halo).min(height);
        let mut region = Vec::with_capacity((bottom - top) * row);
        region.extend_from_slice(&kept[(top - kept_from) * row..]);
        region.extend_from_slice(&image.as_raw()[start * row..bottom * row]);
        let processed = process(
            &region,
            width,
            bottom - top,
            detail,
            &plan,
            weights,
            airlight,
            noise,
        );
        drop(region);
        let next_top = end.saturating_sub(halo);
        let mut next_kept = Vec::with_capacity((end - next_top) * row);
        if next_top < start {
            next_kept.extend_from_slice(&kept[(next_top - kept_from) * row..]);
        }
        next_kept.extend_from_slice(&image.as_raw()[next_top.max(start) * row..end * row]);
        kept = next_kept;
        kept_from = next_top;
        let skip = (start - top) * row;
        image.as_mut()[start * row..end * row]
            .copy_from_slice(&processed[skip..skip + (end - start) * row]);
        start = end;
    }
}

/// One region, whole. Its edges are treated as image edges, which is exactly
/// why a strip is read with a halo.
#[allow(clippy::too_many_arguments)]
fn process(
    rgba: &[f32],
    width: usize,
    height: usize,
    detail: &Detail,
    plan: &Plan,
    weights: [f32; 3],
    airlight: Option<[f32; 3]>,
    noise: Option<(f32, f32)>,
) -> Vec<f32> {
    let pixels = width * height;
    // Negative Dehaze scatters light: a share mixed toward a fine and a broad
    // blur, in linear light, so bright areas glow into their surroundings and
    // fine detail goes soft first, as Lightroom's does (measured: at -100 it
    // keeps 40% of the finest detail and 55% of the broadest).
    let scattered;
    let rgba = match plan.haze {
        Some((fine, broad, [f_fine, f_broad])) => {
            let channels: Vec<Vec<f32>> = (0..4)
                .map(|c| (0..pixels).map(|i| rgba[i * 4 + c]).collect())
                .collect();
            let blurred_fine: Vec<Vec<f32>> = channels[..3]
                .iter()
                .map(|p| blur(p, width, height, fine))
                .collect();
            let blurred_broad: Vec<Vec<f32>> = channels[..3]
                .iter()
                .map(|p| blur(p, width, height, broad))
                .collect();
            let mut out = rgba.to_vec();
            out.par_chunks_mut(4).enumerate().for_each(|(i, px)| {
                for c in 0..3 {
                    px[c] = px[c] * (1.0 - f_fine - f_broad)
                        + blurred_fine[c][i] * f_fine
                        + blurred_broad[c][i] * f_broad;
                }
            });
            scattered = out;
            &scattered[..]
        }
        None => rgba,
    };
    // Dehaze first: it changes colour as well as brightness, and every later
    // stage should act on the clearer image.
    let dehazed;
    let rgba = match (plan.dehaze, airlight) {
        (Some((min_radius, guide_radius)), Some(a)) => {
            dehazed = dehaze(
                rgba,
                width,
                height,
                detail.dehaze / 100.,
                a,
                min_radius,
                guide_radius,
                weights,
            );
            &dehazed[..]
        }
        _ => rgba,
    };
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
    let denoised = match &plan.luminance {
        Some(nr) => {
            let mut smooth = guided(&log, &log, width, height, nr.radius, nr.eps);
            // Contrast: give back some of what was flattened, blurred first so
            // the pixel noise does not come back with it.
            if let Some((g, share)) = nr.contrast {
                let lost: Vec<f32> = log.par_iter().zip(&smooth).map(|(l, s)| l - s).collect();
                let lost = blur(&lost, width, height, g);
                smooth
                    .par_iter_mut()
                    .zip(lost.par_iter())
                    .for_each(|(s, d)| *s += share * d);
            }
            smooth
        }
        None => log,
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
    add_band(plan.soften, detail.sharpening / 100. * 1.5, 0.5, false, 0.0);
    for &(g, amount) in &plan.texture {
        add_band(Some(g), amount, 0.5, false, 0.0);
    }
    for &(g, amount) in &plan.clarity {
        add_band(Some(g), amount, 1.0, true, 0.0);
    }
    for &(g, amount) in &plan.dehaze_bands {
        add_band(Some(g), amount, 1.0, false, 0.0);
    }
    add_band(
        plan.structure,
        detail.structure / 100. * 0.6,
        1.0,
        false,
        0.0,
    );

    // Sharpening, as Lightroom's: the bands' sum soft-limited as a whole (the
    // limit is what holds a strong edge's halo back), gated by the threshold,
    // and masked to the edges.
    if let Some(sharp) = &plan.sharpen {
        let mut total = vec![0f32; pixels];
        // The threshold judges the detail itself (the middle band, before
        // any amplifying), not what sharpening makes of it.
        let mut detail_size = Vec::new();
        for (j, &(sigma, amount)) in sharp.bands.iter().enumerate() {
            let blurred = exact_gaussian(&denoised, width, height, sigma);
            total
                .par_iter_mut()
                .zip(denoised.par_iter())
                .zip(blurred.par_iter())
                .for_each(|((t, l), b)| *t += amount * (l - b));
            if j == 1 && detail.threshold > 0. {
                detail_size = denoised.iter().zip(&blurred).map(|(l, b)| l - b).collect();
            }
        }
        let mask = sharp.mask.map(|[lo, hi]| {
            let edges = edge_strength(&denoised, width, height, sharp.scale);
            edges
                .into_par_iter()
                .map(|e| {
                    // Per full-resolution pixel, so the mask is the same at
                    // every preview size.
                    let t = ((e * sharp.scale - lo) / (hi - lo).max(1e-6)).clamp(0., 1.);
                    t * t * (3. - 2. * t)
                })
                .collect::<Vec<f32>>()
        });
        // Lightroom sharpens the shadows (and a little the highlights) less
        // than the midtones: measured, at Sharpening 40 the finest detail
        // gains 1.1x below L* 12 and 1.7x in the midtones.
        let tone = exact_gaussian(&denoised, width, height, TONE_SIGMA * sharp.scale);
        let weight = |l: f32| {
            use super::sharpen_table::{TONE_KNOTS as K, TONES as W};
            // The tone as the tone zones judge it: the DaVinci Intermediate key.
            let x = super::spaces::encode_intermediate(f64::from((l.exp2() - LOG_FLOOR).max(0.0)))
                as f32;
            if x <= K[0] {
                return W[0];
            }
            for i in 1..K.len() {
                if x <= K[i] {
                    let t = (x - K[i - 1]) / (K[i] - K[i - 1]);
                    return W[i - 1] + (W[i] - W[i - 1]) * t;
                }
            }
            W[K.len() - 1]
        };
        let gate = detail.threshold * 0.004;
        let limit = sharp.limit;
        graded
            .par_iter_mut()
            .zip(total.par_iter())
            .enumerate()
            .for_each(|(i, (out, &t))| {
                let mut d = t;
                if gate > 0.0 {
                    let s = detail_size[i];
                    d *= s * s / (s * s + gate * gate);
                }
                let d = limit * (d / limit).tanh() * weight(tone[i]);
                *out += d * mask.as_ref().map_or(1.0, |m| m[i]);
            });
    }

    // Colour: opponent colour in cube-root light, smoothed, and each pixel
    // rebuilt at its own luminance, so colour noise reduction cannot change
    // brightness at all.
    let lit = |i: usize| luminance[i] > 1e-6;
    let opponent: Option<[Vec<f32>; 2]> = plan.colour.as_ref().map(|nr| {
        let channel = |c: usize| -> Vec<f32> {
            (0..pixels)
                .into_par_iter()
                .map(|i| rgba[i * 4 + c].max(0.0).cbrt() - rgba[i * 4 + 1].max(0.0).cbrt())
                .collect()
        };
        // The fine stage, steered by luminance (its statistics shared by both
        // channels).
        let steer = Guide1::new(&denoised, width, height, nr.radius, nr.eps);
        let fine = [0, 2].map(|c| {
            let o = channel(c);
            let smooth = steer.filter(&o);
            o.par_iter()
                .zip(smooth.par_iter())
                .map(|(o, s)| o + (s - o) * nr.mix)
                .collect::<Vec<f32>>()
        });
        // The adaptive stage, steered by the colour the fine stage left: it
        // smooths what is within the photo's own noise and keeps colour edges.
        let (radius2, k, mix2, power) = nr.adaptive;
        // What counts as noise is judged by the noise at this size; how
        // strongly, by the photograph's own (Lightroom's strength follows the
        // photograph, however small it is shown).
        let (n, photo) = noise.unwrap_or((0.0, 0.0));
        let eps = (k * n * (photo / 0.01).powf(power)).powi(2) + 1e-10;
        // What is left of the colour noise shrinks as the photo gets noisier.
        let mix2 = 1.0 - (1.0 - mix2) * (0.01 / photo.max(1e-6)).min(1.0);
        let guide = [0, 1].map(|j| exact_gaussian(&fine[j], width, height, nr.guide_sigma));
        let steer = Guide2::new(&guide[0], &guide[1], width, height, radius2, eps);
        fine.map(|o| {
            let smooth = steer.filter(&o);
            o.par_iter()
                .zip(smooth.par_iter())
                .map(|(o, s)| o + (s - o) * mix2)
                .collect()
        })
    });

    let mut out = rgba.to_vec();
    out.par_chunks_mut(4).enumerate().for_each(|(i, px)| {
        if !lit(i) {
            return;
        }
        let y = (graded[i].exp2() - LOG_FLOOR).max(0.0);
        match &opponent {
            Some([o1, o2]) => {
                let rgb = rebuild(o1[i], o2[i], y, weights);
                px[..3].copy_from_slice(&rgb);
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

/// Light with opponent colour (`o1` = red less green, `o2` = blue less
/// green, in cube-root light) and luminance exactly `y`: Newton on the green
/// root, which the luminance grows with wherever the channels are positive.
fn rebuild(o1: f32, o2: f32, y: f32, weights: [f32; 3]) -> [f32; 3] {
    let mut g = y.max(0.0).cbrt();
    for _ in 0..8 {
        let (r, gg, b) = ((g + o1).max(0.0), g.max(0.0), (g + o2).max(0.0));
        let f = weights[0] * r * r * r + weights[1] * gg * gg * gg + weights[2] * b * b * b - y;
        let d = 3.0 * (weights[0] * r * r + weights[1] * gg * gg + weights[2] * b * b) + 1e-9;
        g -= f / d;
    }
    let (r, gg, b) = ((g + o1).max(0.0), g.max(0.0), (g + o2).max(0.0));
    [r * r * r, gg * gg * gg, b * b * b]
}

/// The haze colour: the mean of the pixels whose darkest channel is
/// brightest — the top 0.1% of the dark channel, from He, Sun and Tang.
/// Estimated from a subsample, since it is a statistic of the whole frame.
pub(crate) fn airlight(rgba: &[f32]) -> [f32; 3] {
    let pixels = rgba.len() / 4;
    let stride = (pixels / 1_000_000).max(1);
    let samples: Vec<(f32, usize)> = (0..pixels)
        .step_by(stride)
        .map(|i| {
            let p = &rgba[i * 4..i * 4 + 3];
            (p[0].min(p[1]).min(p[2]).max(0.0), i)
        })
        .collect();
    let take = (samples.len() / 1000).max(1);
    let mut darkest: Vec<f32> = samples.iter().map(|s| s.0).collect();
    let cut = darkest.len() - take;
    let threshold = *darkest.select_nth_unstable_by(cut, |a, b| a.total_cmp(b)).1;
    let mut sum = [0.0f64; 3];
    let mut n = 0.0f64;
    for &(dark, i) in &samples {
        if dark >= threshold {
            for c in 0..3 {
                sum[c] += rgba[i * 4 + c].max(0.0) as f64;
            }
            n += 1.0;
        }
    }
    sum.map(|v| ((v / n.max(1.0)) as f32).max(0.05))
}

/// Dark-channel dehazing (He, Sun and Tang): the haze model is
/// `I = J·t + A·(1 − t)`, so with the airlight `A` known and the transmission
/// `t` estimated from how bright the darkest channel is locally, the clear
/// scene `J` can be solved for. The transmission map is refined with a
/// guided filter so it follows the photograph's edges instead of the blocks
/// of the minimum filter. Only removes haze: adding it (negative Dehaze) is
/// the grading pass's veil (`haze_veil` in the shader), as Lightroom's is.
#[allow(clippy::too_many_arguments)]
fn dehaze(
    rgba: &[f32],
    width: usize,
    height: usize,
    amount: f32,
    a: [f32; 3],
    min_radius: usize,
    guide_radius: usize,
    weights: [f32; 3],
) -> Vec<f32> {
    let pixels = width * height;
    let mut out = rgba.to_vec();
    let dark_min: Vec<f32> = (0..pixels)
        .into_par_iter()
        .map(|i| {
            let p = &rgba[i * 4..i * 4 + 3];
            (0..3)
                .map(|c| p[c].max(0.0) / a[c])
                .fold(f32::MAX, f32::min)
        })
        .collect();
    let dark = min_filter(&dark_min, width, height, min_radius);
    let omega = 0.95 * amount.min(1.0);
    let raw: Vec<f32> = dark.par_iter().map(|d| 1.0 - omega * d).collect();
    let guide: Vec<f32> = (0..pixels)
        .into_par_iter()
        .map(|i| {
            let p = &rgba[i * 4..i * 4 + 3];
            weights[0] * p[0] + weights[1] * p[1] + weights[2] * p[2]
        })
        .collect();
    let transmission = guided(&guide, &raw, width, height, guide_radius, 1e-3);
    out.par_chunks_mut(4)
        .zip(transmission.par_iter())
        .for_each(|(p, t)| {
            let t = t.clamp(0.1, 1.0);
            for c in 0..3 {
                p[c] = (p[c] - a[c]) / t + a[c];
            }
        });
    out
}

/// Minimum over a square window, edges repeated: separable, and linear in
/// the image size whatever the radius (van Herk / Gil-Werman).
fn min_filter(plane: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let rows = min_rows(plane, w, h, r);
    let cols = min_rows(&transpose(&rows, w, h), h, w, r);
    transpose(&cols, h, w)
}

fn min_rows(plane: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return plane.to_vec();
    }
    let k = 2 * r + 1;
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w)
        .zip(plane.par_chunks(w))
        .take(h)
        .for_each(|(dst, src)| {
            // Pad with the edge values, then prefix/suffix minima per block.
            let padded: Vec<f32> = (0..w + 2 * r)
                .map(|i| src[(i as isize - r as isize).clamp(0, w as isize - 1) as usize])
                .collect();
            let n = padded.len();
            let mut prefix = padded.clone();
            let mut suffix = padded.clone();
            for i in 1..n {
                if i % k != 0 {
                    prefix[i] = prefix[i].min(prefix[i - 1]);
                }
            }
            for i in (0..n.saturating_sub(1)).rev() {
                if (i + 1) % k != 0 {
                    suffix[i] = suffix[i].min(suffix[i + 1]);
                }
            }
            for (x, d) in dst.iter_mut().enumerate() {
                *d = suffix[x].min(prefix[x + 2 * r]);
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

/// Opponent colour in cube-root light (roughly as Lab sees it, so shadow
/// noise is not exaggerated), blurred by `sigma`: what colour noise
/// reduction judges colour edges by.
fn chroma_guide(rgba: &[f32], w: usize, h: usize, sigma: f32) -> [Vec<f32>; 2] {
    let opponent = |c: usize| -> Vec<f32> {
        (0..w * h)
            .into_par_iter()
            .map(|i| rgba[i * 4 + c].max(0.0).cbrt() - rgba[i * 4 + 1].max(0.0).cbrt())
            .collect()
    };
    [0, 2].map(|c| exact_gaussian(&opponent(c), w, h, sigma))
}

/// The photo's colour noise: a robust spread (1.4826 x the median absolute
/// residual from a local mean of `radius`) of the colour guide, over the
/// whole frame.
fn chroma_noise(rgba: &[f32], w: usize, h: usize, sigma: f32, radius: usize) -> f32 {
    let mut residuals = Vec::new();
    for g in chroma_guide(rgba, w, h, sigma) {
        let mean = box_mean(&g, w, h, radius);
        residuals.extend(
            (0..h)
                .step_by(3)
                .flat_map(|y| (0..w).step_by(3).map(move |x| y * w + x))
                .map(|i| (g[i] - mean[i]).abs()),
        );
    }
    if residuals.is_empty() {
        return 0.0;
    }
    let mid = residuals.len() / 2;
    1.4826
        * *residuals
            .select_nth_unstable_by(mid, |a, b| a.total_cmp(b))
            .1
}

/// `guided`, with the guide's statistics worked out once for several inputs.
struct Guide1<'a> {
    g: &'a [f32],
    w: usize,
    h: usize,
    r: usize,
    mean: Vec<f32>,
    /// One over the guide's regularised local variance.
    inverse: Vec<f32>,
}

impl<'a> Guide1<'a> {
    fn new(g: &'a [f32], w: usize, h: usize, r: usize, eps: f32) -> Self {
        let mean = box_mean(g, w, h, r);
        let square: Vec<f32> = g.par_iter().map(|a| a * a).collect();
        let square = box_mean(&square, w, h, r);
        let inverse = mean
            .par_iter()
            .zip(square.par_iter())
            .map(|(m, s)| 1.0 / ((s - m * m).max(0.0) + eps))
            .collect();
        Self {
            g,
            w,
            h,
            r,
            mean,
            inverse,
        }
    }

    fn filter(&self, x: &[f32]) -> Vec<f32> {
        let (w, h, r) = (self.w, self.h, self.r);
        let mean_x = box_mean(x, w, h, r);
        let product: Vec<f32> = self
            .g
            .par_iter()
            .zip(x.par_iter())
            .map(|(a, b)| a * b)
            .collect();
        let mean_gx = box_mean(&product, w, h, r);
        drop(product);
        let a: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|k| (mean_gx[k] - self.mean[k] * mean_x[k]) * self.inverse[k])
            .collect();
        let b: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|k| mean_x[k] - a[k] * self.mean[k])
            .collect();
        drop((mean_x, mean_gx));
        let (a, b) = (box_mean(&a, w, h, r), box_mean(&b, w, h, r));
        (0..w * h)
            .into_par_iter()
            .map(|k| a[k] * self.g[k] + b[k])
            .collect()
    }
}

/// A guided filter steered by a two-channel guide (He, Sun and Tang). The
/// guide's own statistics are worked out once and shared by everything it
/// filters (both colour channels), which halves the work and the memory.
struct Guide2<'a> {
    g1: &'a [f32],
    g2: &'a [f32],
    w: usize,
    h: usize,
    r: usize,
    m1: Vec<f32>,
    m2: Vec<f32>,
    /// The inverse of the guide's regularised local covariance, [v11, v12, v22].
    inverse: Vec<[f32; 3]>,
}

impl<'a> Guide2<'a> {
    fn new(g1: &'a [f32], g2: &'a [f32], w: usize, h: usize, r: usize, eps: f32) -> Self {
        let product = |a: &[f32], b: &[f32]| -> Vec<f32> {
            a.par_iter().zip(b.par_iter()).map(|(a, b)| a * b).collect()
        };
        let m1 = box_mean(g1, w, h, r);
        let m2 = box_mean(g2, w, h, r);
        let s11 = box_mean(&product(g1, g1), w, h, r);
        let s22 = box_mean(&product(g2, g2), w, h, r);
        let s12 = box_mean(&product(g1, g2), w, h, r);
        let inverse = (0..w * h)
            .into_par_iter()
            .map(|k| {
                let v11 = s11[k] - m1[k] * m1[k] + eps;
                let v22 = s22[k] - m2[k] * m2[k] + eps;
                let v12 = s12[k] - m1[k] * m2[k];
                let det = v11 * v22 - v12 * v12;
                [v22 / det, -v12 / det, v11 / det]
            })
            .collect();
        Self {
            g1,
            g2,
            w,
            h,
            r,
            m1,
            m2,
            inverse,
        }
    }

    fn filter(&self, x: &[f32]) -> Vec<f32> {
        let (w, h, r) = (self.w, self.h, self.r);
        let mx = box_mean(x, w, h, r);
        let s1x: Vec<f32> = self
            .g1
            .par_iter()
            .zip(x.par_iter())
            .map(|(a, b)| a * b)
            .collect();
        let s1x = box_mean(&s1x, w, h, r);
        let s2x: Vec<f32> = self
            .g2
            .par_iter()
            .zip(x.par_iter())
            .map(|(a, b)| a * b)
            .collect();
        let s2x = box_mean(&s2x, w, h, r);
        let n = w * h;
        let mut a1 = vec![0f32; n];
        let mut a2 = vec![0f32; n];
        let mut b = vec![0f32; n];
        a1.par_iter_mut()
            .zip(a2.par_iter_mut())
            .zip(b.par_iter_mut())
            .enumerate()
            .for_each(|(k, ((a1, a2), b))| {
                let c1 = s1x[k] - self.m1[k] * mx[k];
                let c2 = s2x[k] - self.m2[k] * mx[k];
                let [i11, i12, i22] = self.inverse[k];
                *a1 = i11 * c1 + i12 * c2;
                *a2 = i12 * c1 + i22 * c2;
                *b = mx[k] - *a1 * self.m1[k] - *a2 * self.m2[k];
            });
        drop((mx, s1x, s2x));
        let (a1, a2, b) = (
            box_mean(&a1, w, h, r),
            box_mean(&a2, w, h, r),
            box_mean(&b, w, h, r),
        );
        (0..n)
            .into_par_iter()
            .map(|k| a1[k] * self.g1[k] + a2[k] * self.g2[k] + b[k])
            .collect()
    }
}

/// The radius of `exact_gaussian`'s kernel: three sigma, as scipy's.
fn exact_radius(sigma: f32) -> usize {
    (3.0 * sigma + 0.5) as usize
}

/// A true Gaussian, for the small sigmas sharpening works at, where three
/// box passes are too coarse. Edges repeated.
fn exact_gaussian(plane: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = exact_radius(sigma);
    if r == 0 || sigma <= 0.0 {
        return plane.to_vec();
    }
    let mut kernel: Vec<f32> = (0..=2 * r)
        .map(|i| {
            let x = i as f32 - r as f32;
            (-x * x / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|k| *k /= sum);
    let pass = |src: &[f32], w: usize, h: usize| {
        let mut out = vec![0f32; w * h];
        out.par_chunks_mut(w)
            .zip(src.par_chunks(w))
            .take(h)
            .for_each(|(dst, row)| {
                for (x, d) in dst.iter_mut().enumerate() {
                    let mut acc = 0f32;
                    for (k, &weight) in kernel.iter().enumerate() {
                        let at = (x + k).saturating_sub(r).min(w - 1);
                        acc += weight * row[at];
                    }
                    *d = acc;
                }
            });
        out
    };
    let rows = pass(plane, w, h);
    let cols = pass(&transpose(&rows, w, h), h, w);
    transpose(&cols, h, w)
}

/// The local gradient of the log luminance per preview pixel: Sobel of a
/// one-full-resolution-pixel blur, as Masking judges edges.
fn edge_strength(log: &[f32], w: usize, h: usize, scale: f32) -> Vec<f32> {
    let b = exact_gaussian(log, w, h, scale);
    let at = |x: isize, y: isize| {
        b[(y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize]
    };
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let gx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
            let gy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
            gx.hypot(gy) / 8.0
        })
        .collect()
}

pub(super) fn blur(plane: &[f32], w: usize, h: usize, g: Gaussian) -> Vec<f32> {
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
        apply(&mut img, &Detail::default(), SRGB_Y, 1.0, None);
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
        apply(&mut img, &detail, SRGB_Y, 1.0, None);
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
            apply(&mut img, &detail, SRGB_Y, 1.0, None);
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
        apply(&mut img, &detail, SRGB_Y, 1.0, None);
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
                None,
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

    /// Grain on both sides of a step: flat texture and one strong edge.
    fn grain_and_edge(x: u32, y: u32) -> [f32; 3] {
        let v = if x < 32 { 0.1 } else { 0.4 } * (1.0 + 0.04 * noise(x, y));
        [v, v, v]
    }

    fn grain_spread(img: &image::Rgba32FImage) -> f32 {
        let v: Vec<f32> = (2..24).map(|x| img.get_pixel(x, 8)[1]).collect();
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
    }

    fn sharpened(detail: Detail) -> image::Rgba32FImage {
        let mut img = image(64, 16, grain_and_edge);
        apply(&mut img, &detail, SRGB_Y, 1.0, None);
        img
    }

    #[test]
    fn masking_spares_flat_texture_but_still_sharpens_the_edge() {
        let before = image(64, 16, grain_and_edge);
        let open = sharpened(Detail {
            sharpening: 40.,
            ..Detail::default()
        });
        let masked = sharpened(Detail {
            sharpening: 40.,
            sharpen_masking: 100.,
            ..Detail::default()
        });
        let grown = |img: &image::Rgba32FImage| grain_spread(img) / grain_spread(&before);
        assert!(grown(&open) > 1.3, "sharpening left the grain alone");
        assert!(
            grown(&masked) < 1.0 + (grown(&open) - 1.0) * 0.5,
            "masking still sharpened the flat area: {} vs {}",
            grown(&masked),
            grown(&open)
        );
        assert!(
            edge_contrast_at(&masked, 8, 1) > edge_contrast_at(&before, 8, 1) * 1.05,
            "masking took the edge's sharpening away too"
        );
    }

    #[test]
    fn more_detail_sharpens_fine_texture_harder() {
        let at = |d: f32| {
            grain_spread(&sharpened(Detail {
                sharpening: 40.,
                sharpen_detail: d,
                ..Detail::default()
            }))
        };
        assert!(at(75.) > at(25.) * 1.1, "{} vs {}", at(75.), at(25.));
    }

    #[test]
    fn a_larger_radius_reaches_further_from_the_edge() {
        let edge = |x: u32, _: u32| if x < 32 { [0.1f32; 3] } else { [0.4f32; 3] };
        let reach = |radius: f32| {
            let mut img = image(64, 8, edge);
            apply(
                &mut img,
                &Detail {
                    sharpening: 60.,
                    sharpen_radius: radius,
                    ..Detail::default()
                },
                SRGB_Y,
                1.0,
                None,
            );
            // Overshoot four pixels from the step, on the bright side.
            img.get_pixel(36, 4)[1] - 0.4
        };
        assert!(
            reach(3.0) > reach(1.0) + 1e-3,
            "{} vs {}",
            reach(3.0),
            reach(1.0)
        );
    }

    #[test]
    fn a_raw_opens_with_lightroom_defaults_only_where_unset() {
        let given = serde_json::json!({"sharpening": 0, "clarity": 10});
        let d = Detail::default().with_raw_defaults(&given);
        assert_eq!(d.sharpening, 0.0, "an explicit 0 must stay 0");
        assert_eq!(d.color_noise, 25.0);
        let d = Detail::default().with_raw_defaults(&serde_json::Value::Null);
        assert_eq!((d.sharpening, d.color_noise), (40.0, 25.0));
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
            None,
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
            None,
        );
        for (b, p) in before.iter().zip(img.pixels()) {
            assert!(
                (luminance(p) - b).abs() < 2e-5,
                "luminance moved: {b} -> {}",
                luminance(p)
            );
        }
    }

    #[test]
    fn the_minimum_filter_is_exact() {
        let (w, h) = (23, 17);
        let plane: Vec<f32> = (0..w * h)
            .map(|i| noise((i % w) as u32, (i / w) as u32))
            .collect();
        for r in [1, 3, 5] {
            let fast = min_filter(&plane, w, h, r);
            for y in 0..h {
                for x in 0..w {
                    let mut m = f32::MAX;
                    for dy in -(r as isize)..=r as isize {
                        for dx in -(r as isize)..=r as isize {
                            let xx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                            let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                            m = m.min(plane[yy * w + xx]);
                        }
                    }
                    assert_eq!(fast[y * w + x], m, "r={r} at {x},{y}");
                }
            }
        }
    }

    /// Haze a known scene with the model dehazing inverts, and require the
    /// dehazed result to be much closer to the scene than the hazy one.
    #[test]
    fn dehaze_recovers_a_hazed_scene() {
        let airlight = [0.8f32, 0.82, 0.86];
        let scene = |x: u32, y: u32| {
            let block = (x / 12 + y / 9) % 3;
            let base = [[0.05f32, 0.2, 0.1], [0.3, 0.08, 0.04], [0.06, 0.07, 0.25]][block as usize];
            base.map(|v| v * (1.0 + 0.2 * noise(x, y)))
        };
        // A distant band at the top, almost pure haze — as a real hazy
        // photograph's sky or horizon is. That is where the haze colour is
        // read from; a frame hazed evenly everywhere has no pixel showing it.
        let transmission = |y: u32| if y < 14 { 0.03f32 } else { 0.45 };
        let hazy = image(96, 72, |x, y| {
            let (j, t) = (scene(x, y), transmission(y));
            std::array::from_fn(|c| j[c] * t + airlight[c] * (1.0 - t))
        });
        // Judged on the foreground, away from the transmission edge.
        let error = |img: &image::Rgba32FImage| {
            let mut sum = 0.0f32;
            let mut n = 0.0f32;
            for (x, y, p) in img.enumerate_pixels().filter(|(_, y, _)| *y >= 24) {
                let j = scene(x, y);
                sum += (0..3).map(|c| (p[c] - j[c]).abs()).sum::<f32>();
                n += 3.0;
            }
            sum / n
        };
        let mut clear = hazy.clone();
        apply(
            &mut clear,
            &Detail {
                dehaze: 100.,
                ..Detail::default()
            },
            SRGB_Y,
            1.0,
            None,
        );
        assert!(
            error(&clear) < error(&hazy) * 0.5,
            "dehaze did not recover the scene: {} -> {}",
            error(&hazy),
            error(&clear)
        );
        // Negative adds haze: the image moves toward the airlight.
        let mut hazier = hazy.clone();
        apply(
            &mut hazier,
            &Detail {
                dehaze: -60.,
                ..Detail::default()
            },
            SRGB_Y,
            1.0,
            None,
        );
        assert!(
            error(&hazier) > error(&hazy),
            "negative dehaze did not add haze"
        );
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
            dehaze: 50.,
            // Every Lightroom sub-control, so their reach is in the halo too.
            sharpen_radius: 2.5,
            sharpen_detail: 60.,
            sharpen_masking: 40.,
            luminance_noise_detail: 30.,
            luminance_noise_contrast: 50.,
            color_noise_detail: 70.,
            color_noise_smoothness: 80.,
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
        apply_in_strips(&mut whole, &detail, SRGB_Y, 1.0, 200, None);
        for strip in [7, 32, 61] {
            let mut tiled = make();
            apply_in_strips(&mut tiled, &detail, SRGB_Y, 1.0, strip, None);
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
                    dehaze: 50.,
                    sharpen_masking: 50.,
                    ..Detail::default()
                },
            ),
        ] {
            let mut img = make();
            let start = std::time::Instant::now();
            apply(&mut img, &detail, SRGB_Y, 1.0, None);
            println!(
                "{name:24} {:>7.0} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}
