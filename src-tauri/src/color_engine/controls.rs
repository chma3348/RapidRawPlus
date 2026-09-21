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
    pub ranges: Vec<ColorRange>,
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
            ranges: Vec::new(),
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
        ensure!(
            self.curve[0] == 0. && self.curve[4] == 1. && self.curve.iter().all(|v| v.is_finite()),
            "Invalid curve endpoints"
        );
        ensure!(
            self.curve.windows(2).all(|p| p[1] - p[0] >= 0.00999),
            "Curve points must stay ordered with at least 0.01 separation"
        );
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

    pub fn color_is_neutral(&self) -> bool {
        self.saturation == 0.
            && self.vibrance == 0.
            && self.hue == 0.
            && self.bands == [[0.; 3]; 8]
            && self.grading == [[0.; 3]; 4]
            && self.ranges.iter().all(|r| r.adjustment == [0.; 3])
    }

    pub fn is_neutral(&self) -> bool {
        self.ranges.iter().all(|r| r.adjustment == [0.; 3])
            && self
                == &Self {
                    pivot: self.pivot,
                    ranges: self.ranges.clone(),
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
