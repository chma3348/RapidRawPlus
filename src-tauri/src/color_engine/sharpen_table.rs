//! RapidRAW's Sharpening, fitted to Lightroom's by tools/fit_sharpen.py
//! from tools/adobe_sharpen.py's measurements; do not edit.
//!
//! Bands at sigma `SIGMAS` x Radius (full-resolution pixels): their amounts
//! at Lightroom's Sharpening 40, scaled by (Sharpening / 40) ^ `POWER`, and
//! the soft limit (stops) on their sum, at Detail 25 and at Detail 75.
//! `MASKING_50`: the edge mask's thresholds (log2 per pixel) at Masking 50.

pub const SIGMAS: [f32; 3] = [0.50, 1.00, 2.00];
pub const POWER: f32 = 1.49224;
/// The tone weighting: at `TONE_KNOTS` on the tonal key (DaVinci
/// Intermediate, of the luminance blurred at sigma 2), how much of the
/// sharpening is applied.
pub const TONE_KNOTS: [f32; 5] = [0.1575, 0.2286, 0.3182, 0.4258, 0.5951];
pub const TONES: [f32; 5] = [0.0000, 0.2810, 0.7269, 1.0000, 0.9306];
pub const D25: [f32; 3] = [0.77397, 1.28401, -0.21333];
pub const D25_LIMIT: f32 = 0.90049;
pub const D75: [f32; 3] = [2.02012, 2.10464, -0.26141];
pub const D75_LIMIT: f32 = 0.90008;
pub const MASKING_50: [f32; 2] = [0.00000, 0.07327];
/// Color NR at 25 (Detail 50, Smoothness 50). The fine stage: a guided
/// filter by luminance, its radius (full-resolution pixels), regularisation
/// and how much is mixed in. The adaptive stage: a guided filter by the
/// colour itself over `radius2`, smoothing colour variation smaller than
/// `noise_k` times the photo's own colour noise, mixed in by `mix2`.
/// Colour is opponent colour in cube-root light; noise counts as noise up
/// to noise_k x noise x (noise / 0.01) ^ `noise_power`.
pub const COLOUR_25: [f32; 7] = [2.0000, 0.092012, 0.6689, 24.0000, 3.1541, 0.6549, 0.1945];
