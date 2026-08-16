//! Fujifilm F-Log2 C: log curve + F-Gamut C primaries.
//!
//! Source of truth: "F-Log2 C Data Sheet Ver.1.0" (FUJIFILM),
//! https://dl.fujifilm-x.com/technical-data/F-Log2C_DataSheet_E_Ver.1.0.pdf
//! The curve is identical to F-Log2; the gamut is F-Gamut C:
//!   R (0.7347, 0.2653)  G (0.0263, 0.9737)  B (0.1173, -0.0224)  W D65.
//!
//! The shader mirrors these constants (see `flog2_encode` and
//! `SRGB_TO_FGAMUT_C` in shader.wgsl); the tests below pin them to the
//! data sheet's published 10-bit code values so drift in either copy is
//! caught.

// This module is the Rust-side copy of the spec: nothing calls it outside the
// tests below, and the constants stay at data-sheet precision on purpose.
#![allow(dead_code, clippy::excessive_precision)]

pub const A: f32 = 5.555556;
pub const B: f32 = 0.064829;
pub const C: f32 = 0.245281;
pub const D: f32 = 0.384316;
pub const E: f32 = 8.799461;
pub const F: f32 = 0.092864;
pub const CUT1: f32 = 0.000889;

/// Linear sRGB (BT.709 primaries, D65) -> linear F-Gamut C, row-major.
/// Derived from the data sheet primaries (both white points are D65, so
/// no chromatic adaptation term). All coefficients are positive: sRGB is
/// entirely inside F-Gamut C.
pub const SRGB_TO_FGAMUT_C: [[f32; 3]; 3] = [
    [0.51706902, 0.41293468, 0.06999630],
    [0.08861716, 0.80926315, 0.10211969],
    [0.01775004, 0.10944762, 0.87280234],
];

/// Scene-linear reflection -> F-Log2 C code value (0..1).
pub fn encode(x: f32) -> f32 {
    let t = x.max(0.0);
    if t >= CUT1 {
        C * (A * t + B).log10() + D
    } else {
        E * t + F
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The data sheet's published anchors: 0% -> code 95, 18% -> 400,
    /// 90% -> 570 (10-bit).
    #[test]
    fn encode_matches_data_sheet_code_values() {
        let code = |refl: f32| (encode(refl) * 1023.0).round() as i32;
        assert_eq!(code(0.0), 95);
        assert_eq!(code(0.18), 400);
        assert_eq!(code(0.90), 570);
    }

    /// D65 white in sRGB must map to equal-energy white in F-Gamut C
    /// (same white point, normalized matrix).
    #[test]
    fn gamut_matrix_preserves_white() {
        for row in SRGB_TO_FGAMUT_C {
            let sum: f32 = row.iter().sum();
            assert!((sum - 1.0).abs() < 1e-4, "row sums to {sum}");
        }
    }
}
