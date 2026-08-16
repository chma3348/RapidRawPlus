//! Correctness of the binary16 -> f32 conversion used to decode
//! half-float TIFFs (video-tool frame exports).

#[test]
fn half_to_f32_matches_reference_values() {
    let cases: &[(u16, f32)] = &[
        (0x0000, 0.0),
        (0x8000, -0.0),
        (0x3C00, 1.0),
        (0x3800, 0.5),
        (0xBC00, -1.0),
        (0x4000, 2.0),
        (0x7BFF, 65504.0),        // largest normal half
        (0x0400, 6.103515625e-5), // smallest normal half
        (0x0001, 5.9604645e-8),   // smallest subnormal half
        (0x0200, 3.0517578e-5),   // mid subnormal
        (0x3555, 0.333251953125), // ~1/3
    ];
    for (bits, expected) in cases {
        let got = rapidraw_lib::image_loader::half_to_f32(*bits);
        assert!(
            (got - expected).abs() <= expected.abs() * 1e-6 + 1e-12,
            "half 0x{bits:04X}: expected {expected}, got {got}"
        );
    }

    assert!(rapidraw_lib::image_loader::half_to_f32(0x7C00).is_infinite());
    assert!(rapidraw_lib::image_loader::half_to_f32(0x7E00).is_nan());
}
