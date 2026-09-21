//! Two-sample white balance: make one patch of the photo the colour of
//! another.
//!
//! The ordinary eyedropper assumes the thing you click should be neutral
//! grey. That is the wrong question for composited content. There, you have
//! two patches that *should already match* — a cloud in the inserted sky and
//! a cloud in the original, generated skin and the real skin beside it — and
//! the job is to make the first look like the second, whatever colour that
//! is.
//!
//! So the cast being removed is the ratio between the two samples, not the
//! cast of either one. Both are normalised to the same brightness first, so
//! clicking a bright cloud against a dark one shifts colour without dragging
//! exposure with it.

use serde::Serialize;

/// Relative luminance of a linear-light colour.
fn luminance(c: [f32; 3]) -> f32 {
    (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).max(1e-5)
}

fn to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Why a pair of samples cannot be matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleProblem {
    /// Blown out, or so dark it is all noise: no usable colour.
    Clipped,
    /// The two samples are already the same colour.
    NothingToDo,
}

impl std::fmt::Display for SampleProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl SampleProblem {
    pub fn message(self) -> &'static str {
        match self {
            Self::Clipped => {
                "That sample is clipped or almost black, so it carries no colour. Pick a mid-tone area."
            }
            Self::NothingToDo => "Those two samples are already the same colour.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhiteBalanceMatch {
    /// Change to add to the temperature slider.
    pub temperature: f32,
    /// Change to add to the tint slider.
    pub tint: f32,
    /// The per-channel gains those sliders stand for, in linear light.
    pub gains: [f32; 3],
    /// True when the correction is big enough to suggest the two samples
    /// are not really the same material.
    pub large: bool,
}

/// Luminance-preserving gains that turn `sample` into `reference`.
/// Inputs are display-encoded (sRGB) 0..1.
pub fn match_gains(sample: [f32; 3], reference: [f32; 3]) -> [f32; 3] {
    let s: [f32; 3] = std::array::from_fn(|c| to_linear(sample[c].clamp(0.0, 1.0)));
    let r: [f32; 3] = std::array::from_fn(|c| to_linear(reference[c].clamp(0.0, 1.0)));
    let (sl, rl) = (luminance(s), luminance(r));
    // Compare chromaticities: brightness is not part of white balance.
    let mut gains: [f32; 3] = std::array::from_fn(|c| ((r[c] / rl) + 1e-5) / ((s[c] / sl) + 1e-5));
    let gl = luminance(gains);
    for g in gains.iter_mut() {
        *g /= gl;
    }
    gains
}

/// Is this sample usable as a colour reference?
fn usable(sample: [f32; 3]) -> bool {
    let peak = sample[0].max(sample[1]).max(sample[2]);
    let low = sample[0].min(sample[1]).min(sample[2]);
    peak < 0.985 && low > 0.02
}

/// Match `sample` to `reference`, in the app's temperature/tint units.
///
/// The slider mapping mirrors the existing eyedropper, and the signs follow
/// the renderer: in `apply_white_balance`, positive temperature multiplies
/// red up and blue down (warmer), and positive tint multiplies red and blue
/// up and green down (magenta). So a sample that reads too blue next to its
/// reference asks for positive temperature, and one that reads too green
/// asks for positive tint. The 125 / 400 scaling is the existing picker's,
/// so both pickers move the sliders by comparable amounts.
pub fn match_samples(
    sample: [f32; 3],
    reference: [f32; 3],
) -> Result<WhiteBalanceMatch, SampleProblem> {
    if !usable(sample) || !usable(reference) {
        return Err(SampleProblem::Clipped);
    }
    let gains = match_gains(sample, reference);
    // The colour the correction has to neutralise: the sample seen relative
    // to the reference. Gains above 1 on blue mean the sample is short of
    // blue, i.e. it reads warm, and temperature should come down.
    let residual: [f32; 3] = std::array::from_fn(|c| 1.0 / gains[c]);
    let (r, g, b) = (residual[0], residual[1], residual[2]);
    let sum_rb = r + b;
    let temperature = if sum_rb > 1e-4 {
        ((b - r) / sum_rb) * 125.0
    } else {
        0.0
    };
    let mid = sum_rb / 2.0;
    let sum_gm = g + mid;
    let tint = if sum_gm > 1e-4 {
        ((g - mid) / sum_gm) * 400.0
    } else {
        0.0
    };
    if temperature.abs() < 0.25 && tint.abs() < 0.25 {
        return Err(SampleProblem::NothingToDo);
    }
    Ok(WhiteBalanceMatch {
        temperature,
        tint,
        gains,
        large: temperature.abs() > 45.0 || tint.abs() > 45.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32, what: &str) {
        assert!((a - b).abs() <= tol, "{what}: {a} vs {b}");
    }

    #[test]
    fn identical_samples_have_nothing_to_do() {
        assert_eq!(
            match_samples([0.5, 0.45, 0.4], [0.5, 0.45, 0.4]),
            Err(SampleProblem::NothingToDo)
        );
    }

    #[test]
    fn gains_turn_the_sample_into_the_reference_colour() {
        let sample = [0.45, 0.50, 0.70];
        let reference = [0.65, 0.52, 0.38];
        let gains = match_gains(sample, reference);
        // Applying the gains in linear light must line the chromaticities up.
        let lin = |c: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| to_linear(c[i])) };
        let (s, r) = (lin(sample), lin(reference));
        let corrected: [f32; 3] = std::array::from_fn(|i| s[i] * gains[i]);
        let (cl, rl) = (luminance(corrected), luminance(r));
        for c in 0..3 {
            approx(corrected[c] / cl, r[c] / rl, 0.02, "chromaticity");
        }
        // And brightness is untouched.
        approx(luminance(gains), 1.0, 1e-3, "gain luminance");
    }

    #[test]
    fn brightness_of_the_samples_does_not_matter() {
        let a = match_samples([0.30, 0.34, 0.48], [0.62, 0.50, 0.36]).unwrap();
        // Same colours, both samples twice as bright.
        let b = match_samples([0.55, 0.60, 0.74], [0.82, 0.72, 0.60]).unwrap();
        // Not identical (sRGB is non-linear), but the same correction in kind.
        assert!(
            a.temperature.signum() == b.temperature.signum(),
            "{a:?} {b:?}"
        );
        assert!(a.tint.signum() == b.tint.signum(), "{a:?} {b:?}");
    }

    #[test]
    fn a_blue_sample_against_a_warm_reference_warms_up() {
        let m = match_samples([0.40, 0.46, 0.70], [0.62, 0.50, 0.38]).unwrap();
        assert!(m.temperature > 0.0, "should warm: {m:?}");
        // Warming means holding back blue and lifting red.
        assert!(m.gains[0] > 1.0 && m.gains[2] < 1.0, "{m:?}");
    }

    #[test]
    fn a_warm_sample_against_a_cool_reference_cools_down() {
        let m = match_samples([0.62, 0.50, 0.38], [0.40, 0.46, 0.70]).unwrap();
        assert!(m.temperature < 0.0, "should cool: {m:?}");
        assert!(m.gains[2] > 1.0 && m.gains[0] < 1.0, "{m:?}");
    }

    /// Positive tint is magenta in the renderer, so a green sample asks for
    /// positive tint (add magenta to cancel it) and a magenta sample for
    /// negative.
    #[test]
    fn a_green_sample_asks_for_magenta_and_the_reverse() {
        let green = match_samples([0.40, 0.60, 0.40], [0.50, 0.50, 0.50]).unwrap();
        let magenta = match_samples([0.60, 0.40, 0.60], [0.50, 0.50, 0.50]).unwrap();
        assert!(green.tint > 0.0, "green should ask for magenta: {green:?}");
        assert!(
            magenta.tint < 0.0,
            "magenta should ask for green: {magenta:?}"
        );
        // And the correction cancels the cast: green's gains lift red/blue.
        assert!(green.gains[0] > 1.0 && green.gains[1] < 1.0, "{green:?}");
    }

    #[test]
    fn clipped_or_black_samples_are_refused() {
        assert_eq!(
            match_samples([1.0, 1.0, 1.0], [0.5, 0.5, 0.45]),
            Err(SampleProblem::Clipped)
        );
        assert_eq!(
            match_samples([0.5, 0.5, 0.45], [0.0, 0.0, 0.0]),
            Err(SampleProblem::Clipped)
        );
    }

    #[test]
    fn wildly_different_samples_are_flagged_as_large() {
        let m = match_samples([0.20, 0.55, 0.22], [0.70, 0.35, 0.30]).unwrap();
        assert!(m.large, "a green-to-red match should be flagged: {m:?}");
    }
}
