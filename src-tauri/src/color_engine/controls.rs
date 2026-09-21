use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ColorRange {
    /// Oklab hue degrees, chroma and lightness; sampled before selective edits.
    pub center: [f32; 3],
    pub width: [f32; 3],
    /// Hue degrees, chroma percent (+/- one stop), lightness percent.
    pub adjustment: [f32; 3],
}

/// Vignette, grain and the lens and film effects, with the previous
/// engine's slider meanings so they feel the same. All of them depend on
/// where a pixel is, not only its colour. Vignette and grain run in the GPU
/// pass; the rest are spatial and run in `optics`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Effects {
    /// -100..100: darken (negative) or lighten the corners, in stops.
    pub vignette_amount: f32,
    /// 0..100: where the falloff begins.
    pub vignette_midpoint: f32,
    /// -100..100: from rectangular to round.
    pub vignette_roundness: f32,
    /// 0..100: how gradual the falloff is.
    pub vignette_feather: f32,
    pub grain_amount: f32,
    pub grain_size: f32,
    pub grain_roughness: f32,
    /// 0..100: light spread from bright areas.
    pub glow_amount: f32,
    /// 0..100: red-orange spill around highlights, as film's does.
    pub halation_amount: f32,
    /// 0..100: starburst, ghosts and streak thrown by highlights.
    pub flare_amount: f32,
    /// -100..100: red and blue scaled about the centre, ±1% at the ends.
    pub ca_red_cyan: f32,
    pub ca_blue_yellow: f32,
    /// -100..100: the previous engine's Centre — the middle of the frame
    /// brighter, richer and crisper, the edges quieter; negative reverses it.
    pub centre: f32,
    /// 0..100: film's easing of saturation in deep shadows and near white.
    pub film_saturation: f32,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            vignette_amount: 0.,
            vignette_midpoint: 50.,
            vignette_roundness: 0.,
            vignette_feather: 50.,
            grain_amount: 0.,
            grain_size: 25.,
            grain_roughness: 50.,
            glow_amount: 0.,
            halation_amount: 0.,
            flare_amount: 0.,
            ca_red_cyan: 0.,
            ca_blue_yellow: 0.,
            centre: 0.,
            film_saturation: 0.,
        }
    }
}

impl Effects {
    pub fn validate(&self) -> Result<()> {
        let within = |v: f32, a: f32, b: f32| v.is_finite() && (a..=b).contains(&v);
        ensure!(
            within(self.vignette_amount, -100., 100.)
                && within(self.vignette_roundness, -100., 100.)
                && within(self.ca_red_cyan, -100., 100.)
                && within(self.ca_blue_yellow, -100., 100.)
                && within(self.centre, -100., 100.)
                && [
                    self.vignette_midpoint,
                    self.vignette_feather,
                    self.grain_amount,
                    self.grain_size,
                    self.grain_roughness,
                    self.glow_amount,
                    self.halation_amount,
                    self.flare_amount,
                    self.film_saturation
                ]
                .iter()
                .all(|v| within(*v, 0., 100.)),
            "Effect settings are out of range"
        );
        Ok(())
    }

    pub fn is_neutral(&self) -> bool {
        self.vignette_amount == 0.
            && self.grain_amount == 0.
            && self.centre == 0.
            && self.film_saturation == 0.
            && super::optics::is_neutral(self)
    }
}

/// The previous engine's camera calibration: each primary's hue and
/// saturation, and a green–magenta tint in the shadows, with its slider
/// meanings (-100..100). Defined, as it was, in linear sRGB primaries.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Calibration {
    pub shadows_tint: f32,
    pub red_hue: f32,
    pub red_saturation: f32,
    pub green_hue: f32,
    pub green_saturation: f32,
    pub blue_hue: f32,
    pub blue_saturation: f32,
}

impl Calibration {
    fn values(&self) -> [f32; 7] {
        [
            self.shadows_tint,
            self.red_hue,
            self.red_saturation,
            self.green_hue,
            self.green_saturation,
            self.blue_hue,
            self.blue_saturation,
        ]
    }

    pub fn is_neutral(&self) -> bool {
        self.values() == [0.; 7]
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.values()
                .iter()
                .all(|v| v.is_finite() && (-100. ..=100.).contains(v)),
            "Calibration settings are out of range"
        );
        Ok(())
    }
}

/// V3 controls have their own saved namespace. Legacy settings are never
/// reinterpreted or overwritten when the user opts into this engine.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Controls {
    pub revision: u32,
    pub exposure: f32,
    pub temperature: f32,
    pub tint: f32,
    pub contrast: f32,
    pub pivot: f32,
    pub shadows: f32,
    pub highlights: f32,
    pub blacks: f32,
    pub whites: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub hue: f32,
    /// Red, orange, yellow, green, aqua, blue, purple, magenta: degrees,
    /// chroma stops (UI percent mapped to +/-1), lightness percent.
    pub bands: [[f32; 3]; 8],
    /// Global, shadows, midtones, highlights: hue degrees, chroma amount,
    /// lightness amount. Neutral wheels have zero chroma and lightness.
    pub grading: [[f32; 3]; 4],
    /// Five fixed input knots in log2(1+16Y)/log2(17). Endpoints stay 0 and 1.
    pub curve: [f32; 5],
    /// Red, green and blue curves: the same five knots, applied to each
    /// channel in DaVinci Intermediate — the encoding Resolve's own curves
    /// act in — so, unlike the luminance curve, they change colour too.
    pub channel_curves: [[f32; 5]; 3],
    pub ranges: Vec<ColorRange>,
    /// Spatial controls, applied as their own stage before the pointwise GPU
    /// pass. Older v3 settings without this load as neutral.
    pub detail: super::detail::Detail,
    /// Position-dependent effects: a vignette in the working space, grain on
    /// the finished image. Older v3 settings without this load as neutral.
    pub effects: Effects,
    /// Camera calibration, the first thing the grade does.
    pub calibration: Calibration,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            revision: 1,
            exposure: 0.,
            temperature: 0.,
            tint: 0.,
            contrast: 0.,
            pivot: 0.18,
            shadows: 0.,
            highlights: 0.,
            blacks: 0.,
            whites: 0.,
            saturation: 0.,
            vibrance: 0.,
            hue: 0.,
            bands: [[0.; 3]; 8],
            grading: [[0.; 3]; 4],
            curve: Self::IDENTITY_CURVE,
            channel_curves: [Self::IDENTITY_CURVE; 3],
            ranges: Vec::new(),
            detail: super::detail::Detail::default(),
            effects: Effects::default(),
            calibration: Calibration::default(),
        }
    }
}
impl Controls {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.revision == 1, "Unsupported v3 control revision");
        let range = |v: f32, a: f32, b: f32| v.is_finite() && (a..=b).contains(&v);
        ensure!(
            range(self.exposure, -10., 10.) && range(self.pivot, 0.01, 1.),
            "Invalid exposure or contrast pivot"
        );
        for v in [
            self.temperature,
            self.tint,
            self.contrast,
            self.shadows,
            self.highlights,
            self.blacks,
            self.whites,
            self.saturation,
            self.vibrance,
        ] {
            ensure!(
                range(v, -100., 100.),
                "V3 control must be finite and within -100..100"
            );
        }
        ensure!(range(self.hue, -180., 180.), "Invalid hue rotation");
        for b in self.bands {
            ensure!(
                range(b[0], -60., 60.) && range(b[1], -100., 100.) && range(b[2], -100., 100.),
                "Invalid selective color band"
            );
        }
        for g in self.grading {
            ensure!(
                range(g[0], 0., 360.) && range(g[1], 0., 100.) && range(g[2], -100., 100.),
                "Invalid grading wheel"
            );
        }
        for curve in std::iter::once(&self.curve).chain(self.channel_curves.iter()) {
            ensure!(
                curve[0] == 0. && curve[4] == 1. && curve.iter().all(|v| v.is_finite()),
                "Invalid curve endpoints"
            );
            ensure!(
                curve.windows(2).all(|p| p[1] - p[0] >= 0.00999),
                "Curve points must stay ordered with at least 0.01 separation"
            );
        }
        self.detail.validate()?;
        self.effects.validate()?;
        self.calibration.validate()?;
        ensure!(
            self.ranges.len() <= 8,
            "At most eight custom color ranges are supported"
        );
        for r in &self.ranges {
            ensure!(
                range(r.center[0], 0., 360.)
                    && range(r.center[1], 0., 1.)
                    && range(r.center[2], 0., 2.),
                "Invalid color-range center"
            );
            ensure!(
                range(r.width[0], 1., 180.)
                    && range(r.width[1], 0.01, 1.)
                    && range(r.width[2], 0.01, 2.),
                "Invalid color-range width"
            );
            ensure!(
                range(r.adjustment[0], -60., 60.)
                    && range(r.adjustment[1], -100., 100.)
                    && range(r.adjustment[2], -100., 100.),
                "Invalid color-range adjustment"
            );
        }
        Ok(())
    }
    pub const IDENTITY_CURVE: [f32; 5] = [0., 0.25, 0.5, 0.75, 1.];

    /// Does the luminance chain — contrast, the four zones, the curve — leave
    /// luminance alone? When it does the shader skips it outright, so a stop
    /// of exposure stays an exact doubling instead of picking up the rounding
    /// of a divide the GPU is free to compute reciprocally.
    pub fn tone_is_neutral(&self) -> bool {
        self.contrast == 0.
            && self.shadows == 0.
            && self.highlights == 0.
            && self.blacks == 0.
            && self.whites == 0.
            && self.curve == Self::IDENTITY_CURVE
    }

    pub fn channel_curves_are_neutral(&self) -> bool {
        self.channel_curves == [Self::IDENTITY_CURVE; 3]
    }

    pub fn color_is_neutral(&self) -> bool {
        self.saturation == 0.
            && self.vibrance == 0.
            && self.hue == 0.
            && self.bands == [[0.; 3]; 8]
            && self.grading == [[0.; 3]; 4]
            && self.ranges.iter().all(|r| r.adjustment == [0.; 3])
            && self.calibration.is_neutral()
    }

    pub fn is_neutral(&self) -> bool {
        self.ranges.iter().all(|r| r.adjustment == [0.; 3])
            && self
                == &Self {
                    pivot: self.pivot,
                    ranges: self.ranges.clone(),
                    detail: self.detail.clone(),
                    effects: self.effects.clone(),
                    ..Self::default()
                }
    }
}

/// Monotone cubic Hermite slopes for uniformly spaced knots. Positive
/// secants and harmonic interior slopes prevent spline overshoot.
pub fn curve_parameters(curve: [f32; 5]) -> [[f32; 4]; 5] {
    let d: [f32; 4] = std::array::from_fn(|i| (curve[i + 1] - curve[i]) * 4.);
    std::array::from_fn(|i| {
        let m = if i == 0 {
            d[0]
        } else if i == 4 {
            d[3]
        } else {
            2. * d[i - 1] * d[i] / (d[i - 1] + d[i])
        };
        [curve[i], m, 0., 0.]
    })
}
