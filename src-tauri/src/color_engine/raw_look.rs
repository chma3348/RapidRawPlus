//! How a RAW opens before any edit: Lightroom's default rendering of it
//! (its Adobe Standard profile), measured on pairs of the same RAWs developed
//! by both (tools/fit_raw_look.py; docs/raw-look.md). RapidRAW develops a
//! RAW at the camera's metered exposure through Resolve's gentle rendering,
//! which left RAWs about a stop and a third darker, flatter and a third less
//! colourful than Lightroom's; rendered photographs (JPEGs and the like)
//! already open as Lightroom shows them and are left alone.
//!
//! Two steps on the calibrated scene-linear pixels, once per decode (the
//! decoded source is cached), so everything after — the tone zones' keys,
//! the photo's tones, every control — sees the developed picture:
//!
//! 1. Tone: each pixel's tonal key (its brightness in DaVinci Intermediate,
//!    judged with positive weights as the tone zones judge it) moves along
//!    a fitted curve, by one gain on all three channels, so hues stay put.
//! 2. Colour: in Oklab, chroma scaled and hue turned by a table over hue
//!    and the developed tonal key, as Lightroom's profile does (warm browns
//!    and oranges richer, near-whites paler).
use super::config::{Primaries, Transfer};
use super::raw_look_table::{BANDS, COLOUR, HUES, KNOTS, TONE};
use super::spaces;
use image::Rgba32FImage;
use rayon::prelude::*;

/// Linear sRGB to LMS and LMS (cube-rooted) to Oklab, and back.
const M1: [[f32; 3]; 3] = [
    [0.412_221_46, 0.536_332_55, 0.051_445_995],
    [0.211_903_5, 0.680_699_5, 0.107_396_96],
    [0.088_302_46, 0.281_718_85, 0.629_978_7],
];
const M2: [[f32; 3]; 3] = [
    [0.210_454_26, 0.793_617_8, -0.004_072_047],
    [1.977_998_5, -2.428_592_2, 0.450_593_7],
    [0.025_904_037, 0.782_771_77, -0.808_675_77],
];
const M2_INV: [[f32; 3]; 3] = [
    [1.0, 0.396_337_78, 0.215_803_76],
    [1.0, -0.105_561_346, -0.063_854_17],
    [1.0, -0.089_484_18, -1.291_485_5],
];
const M1_INV: [[f32; 3]; 3] = [
    [4.076_741_7, -3.307_711_6, 0.230_969_94],
    [-1.268_438, 2.609_757_4, -0.341_319_38],
    [-0.004_196_086_3, -0.703_418_6, 1.707_614_7],
];

fn mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn oklab(rgb: [f32; 3]) -> [f32; 3] {
    mul(&M2, mul(&M1, rgb).map(f32::cbrt))
}

fn linear_srgb(lab: [f32; 3]) -> [f32; 3] {
    mul(&M1_INV, mul(&M2_INV, lab).map(|v| v * v * v))
}

fn encode(y: f32) -> f32 {
    spaces::encode_intermediate(y.max(0.0) as f64) as f32
}

fn decode(k: f32) -> f32 {
    spaces::decode(k as f64, Transfer::DavinciIntermediate).max(0.0) as f32
}

/// The tone curve's offset at tonal key `k`; beyond the top knot, held.
fn tone_offset(k: f32) -> f32 {
    let x = k.clamp(0.0, 1.0) * (KNOTS - 1) as f32;
    let i = (x.floor() as usize).min(KNOTS - 2);
    let f = x - i as f32;
    TONE[i] * (1.0 - f) + TONE[i + 1] * f
}

/// [chroma scale, hue turn in radians] at a hue angle and tonal key:
/// bilinear over the table, wrapping around the hue circle.
fn colour(hue: f32, key: f32) -> [f32; 2] {
    let h = hue.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU * HUES as f32;
    let h0 = (h.floor() as usize) % HUES;
    let h1 = (h0 + 1) % HUES;
    let fh = h - h.floor();
    let b = key.clamp(0.0, 1.0) * (BANDS - 1) as f32;
    let b0 = (b.floor() as usize).min(BANDS - 2);
    let fb = b - b0 as f32;
    let at = |band: usize, n: usize| -> f32 {
        let row = &COLOUR[band];
        row[h0][n] * (1.0 - fh) + row[h1][n] * fh
    };
    std::array::from_fn(|n| at(b0, n) * (1.0 - fb) + at(b0 + 1, n) * fb)
}

/// One pixel of calibrated scene-linear sRGB, developed.
pub fn develop(rgb: [f32; 3], to_working: &[[f32; 3]; 3]) -> [f32; 3] {
    let working = mul(to_working, rgb);
    let y =
        0.2126 * working[0].max(0.0) + 0.7152 * working[1].max(0.0) + 0.0722 * working[2].max(0.0);
    let k = encode(y);
    let target = (k + tone_offset(k)).max(0.0);
    let (from, to) = (decode(k), decode(target));
    let gain = if from > 1e-6 {
        (to / from).min(64.0)
    } else {
        1.0
    };
    // Where no gain can reach (pure black lifted), the rest is neutral.
    let fill = (to - from * gain).max(0.0);
    let toned = rgb.map(|v| v * gain + fill);
    let [l, a, b] = oklab(toned);
    let chroma = a.hypot(b);
    if chroma < 1e-6 {
        return toned;
    }
    let [scale, turn] = colour(b.atan2(a), target);
    let hue = b.atan2(a) + turn;
    let c = chroma * scale.max(0.0);
    linear_srgb([l, c * hue.cos(), c * hue.sin()])
}

/// Develop a decoded RAW's pixels (calibrated scene-linear sRGB) in place.
pub fn apply(pixels: &mut Rgba32FImage) {
    let m = spaces::conversion(Primaries::Srgb, Primaries::DavinciWideGamut)
        .transpose()
        .to_cols_array_2d();
    let to_working: [[f32; 3]; 3] =
        std::array::from_fn(|r| std::array::from_fn(|c| m[r][c] as f32));
    pixels.par_chunks_mut(4).for_each(|p| {
        let out = develop([p[0], p[1], p[2]], &to_working);
        p[..3].copy_from_slice(&out);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklab_round_trips() {
        for rgb in [
            [0.18, 0.18, 0.18],
            [0.9, 0.2, 0.05],
            [0.02, 0.3, 0.8],
            [3.0, 2.0, 1.0],
        ] {
            let back = linear_srgb(oklab(rgb));
            for c in 0..3 {
                assert!(
                    (back[c] - rgb[c]).abs() < 1e-4 * rgb[c].max(1.0),
                    "{rgb:?} -> {back:?}"
                );
            }
        }
    }

    #[test]
    fn greys_stay_grey_and_tones_keep_their_order() {
        let m = spaces::conversion(Primaries::Srgb, Primaries::DavinciWideGamut)
            .transpose()
            .to_cols_array_2d();
        let to_working: [[f32; 3]; 3] =
            std::array::from_fn(|r| std::array::from_fn(|c| m[r][c] as f32));
        let mut previous = -1.0f32;
        for i in 0..=400 {
            let v = 2f32.powf(i as f32 / 25.0 - 12.0);
            let out = develop([v, v, v], &to_working);
            assert!(
                (out[0] - out[1]).abs() < 1e-4 * out[1].max(1e-3)
                    && (out[2] - out[1]).abs() < 1e-4 * out[1].max(1e-3),
                "grey {v} -> {out:?}"
            );
            assert!(
                out[1] >= previous,
                "tones reversed at {v}: {} after {previous}",
                out[1]
            );
            previous = out[1];
        }
    }
}
