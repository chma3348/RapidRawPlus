//! Experimental calibrated Bayer development, independent of the legacy path.
use super::{
    config::*,
    input::{DecodedFrame, InputProvenance},
    spaces,
};
use anyhow::{ensure, Context, Result};
use glam::{DMat3, DVec3};
use image::{DynamicImage, ImageBuffer, Rgba};
use rawler::{
    decoders::{Orientation, RawDecodeParams},
    imgop::{
        develop::{DemosaicAlgorithm, Intermediate, ProcessingStep, RawDevelop},
        xyz::Illuminant,
    },
    rawimage::{RawImage, RawImageData, RawPhotometricInterpretation},
    rawsource::RawSource,
};

/// Cancellation is checked between expensive stages. The dependency's decoder
/// and demosaicer themselves are not interruptible.
pub fn decode_raw(
    bytes: &[u8],
    fast: bool,
    check_cancel: impl Fn() -> Result<()>,
) -> Result<DecodedFrame> {
    check_cancel()?;
    let source = RawSource::new_from_slice(bytes);
    let decoder = rawler::get_decoder(&source)?;
    let raw = decoder.raw_image(&source, &RawDecodeParams::default(), false)?;
    check_cancel()?;
    let metadata = decoder.raw_metadata(&source, &RawDecodeParams::default())?;
    let orientation = metadata
        .exif
        .orientation
        .map(Orientation::from_u16)
        .unwrap_or(raw.orientation);
    develop(raw, orientation, fast, check_cancel)
}

fn calibration(matrix: &[f32], wb: &[f32; 4]) -> Result<DMat3> {
    ensure!(
        matrix.len() == 9 && matrix.iter().all(|v| v.is_finite()),
        "V3 RAW needs a finite 3x3 D65 camera matrix"
    );
    ensure!(
        wb[..3].iter().all(|v| v.is_finite() && *v > 0.0),
        "V3 RAW needs valid as-shot RGB white balance"
    );
    let xyz_to_camera = DMat3::from_cols_array(
        &matrix
            .iter()
            .map(|v| *v as f64)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap(),
    )
    .transpose();
    let rgb_to_camera = xyz_to_camera * spaces::rgb_to_xyz(Primaries::Srgb);
    let sums = rgb_to_camera * DVec3::ONE;
    ensure!(
        sums.to_array().iter().all(|v| v.is_finite() && *v > 1e-10),
        "Invalid camera matrix neutral response"
    );
    // Adopt the pinned decoder's row-normalized calibration convention, but
    // perform the matrix operation ourselves, without its color clipping.
    let normalized = DMat3::from_diagonal(sums.recip()) * rgb_to_camera;
    ensure!(
        normalized.determinant().abs() > 1e-10,
        "Singular camera calibration"
    );
    let inverse = normalized.inverse();
    let norm = |m: DMat3| m.to_cols_array().iter().map(|v| v * v).sum::<f64>().sqrt();
    ensure!(
        norm(normalized) * norm(inverse) < 1e5,
        "Ill-conditioned camera calibration"
    );
    let gains = DVec3::new(wb[0] as f64, wb[1] as f64, wb[2] as f64) / wb[1] as f64;
    let result = inverse * DMat3::from_diagonal(gains);
    ensure!(result.is_finite(), "Non-finite camera transform");
    Ok(result)
}

fn normalize_sensor(
    values: &mut [f32],
    width: usize,
    black: [f32; 4],
    white: [f32; 4],
) -> Result<()> {
    ensure!(width > 0, "Empty RAW width");
    for i in 0..4 {
        ensure!(
            black[i].is_finite() && white[i].is_finite() && white[i] > black[i],
            "Invalid sensor black/white levels"
        );
    }
    for (i, value) in values.iter_mut().enumerate() {
        ensure!(value.is_finite(), "Non-finite sensor sample");
        let site = ((i / width) % 2) * 2 + (i % width) % 2;
        // Sensor black floor only. No upper clamp, highlight desaturation, or
        // RGB gamut clipping. Above-white samples remain available downstream.
        *value = (*value - black[site]).max(0.0) / (white[site] - black[site]);
        ensure!(value.is_finite(), "Sensor normalization overflow");
    }
    Ok(())
}

fn develop(
    mut raw: RawImage,
    orientation: Orientation,
    fast: bool,
    check_cancel: impl Fn() -> Result<()>,
) -> Result<DecodedFrame> {
    check_cancel()?;
    let cfa = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(config) => config,
        _ => anyhow::bail!(
            "V3 RAW currently supports Bayer mosaics, not LinearRaw or monochrome files"
        ),
    };
    ensure!(
        raw.cpp == 1 && cfa.cfa.is_rgb() && cfa.cfa.width == 2 && cfa.cfa.height == 2,
        "V3 RAW currently requires a 2x2 RGB Bayer mosaic"
    );
    ensure!(
        raw.width >= 16 && raw.height >= 16 && raw.width % 2 == 0 && raw.height % 2 == 0,
        "Unsupported RAW dimensions"
    );
    ensure!(
        raw.blacklevel.cpp == 1
            && ((raw.blacklevel.width == 1
                && raw.blacklevel.height == 1
                && raw.blacklevel.levels.len() == 1)
                || (raw.blacklevel.width == 2
                    && raw.blacklevel.height == 2
                    && raw.blacklevel.levels.len() == 4)),
        "Unsupported RAW black-level layout"
    );
    ensure!(
        raw.whitelevel.0.len() == 1,
        "V3 RAW currently requires one sensor white level; multi-channel/site ordering needs a verified adapter"
    );
    let matrix = raw.color_matrix.get(&Illuminant::D65).context(
        "No D65 camera calibration; v3 will not select an arbitrary fallback illuminant",
    )?;
    let transform = calibration(matrix, &raw.wb_coeffs)?;
    let black = raw.blacklevel.as_bayer_array();
    let white = raw.whitelevel.as_bayer_array();
    let record = serde_json::json!({
        "camera_make": raw.clean_make, "camera_model": raw.clean_model,
        "xyz_to_camera_d65": matrix, "white_balance_rgb": &raw.wb_coeffs[..3],
        "camera_to_linear_srgb": transform.to_cols_array(), "matrix_layout": "column_major",
        "black_levels": black, "white_levels": white,
        "demosaic": if fast {"speed"} else {"quality"},
        "sensor_floor": "zero_after_black_subtraction", "upper_clamp": false,
        "calibration_revision": "row_normalized_d65_green_normalized_wb_v1"
    });
    let mut samples = raw.data.as_f32().into_owned();
    ensure!(
        samples.len()
            == raw
                .width
                .checked_mul(raw.height)
                .context("RAW size overflow")?,
        "RAW sample count mismatch"
    );
    normalize_sensor(&mut samples, raw.width, black, white)?;
    raw.data = RawImageData::Float(samples);
    check_cancel()?;
    let mut developer = RawDevelop::default();
    developer.steps.retain(|s| {
        !matches!(
            s,
            ProcessingStep::Rescale
                | ProcessingStep::Calibrate
                | ProcessingStep::WhiteBalance
                | ProcessingStep::SRgb
        )
    });
    developer.demosaic_algorithm = if fast {
        DemosaicAlgorithm::Speed
    } else {
        DemosaicAlgorithm::Quality
    };
    let intermediate = developer.develop_intermediate(&raw)?;
    check_cancel()?;
    let Intermediate::ThreeColor(camera) = intermediate else {
        anyhow::bail!("Demosaicing did not produce three camera channels");
    };
    let mut output = Vec::with_capacity(camera.data.len() * 4);
    for p in &camera.data {
        let rgb = transform * DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
        let pixel = [rgb.x as f32, rgb.y as f32, rgb.z as f32, 1.0];
        ensure!(
            pixel.iter().all(|v| v.is_finite()),
            "Non-finite developed RAW pixel"
        );
        output.extend_from_slice(&pixel);
    }
    let image: ImageBuffer<Rgba<f32>, Vec<f32>> =
        ImageBuffer::from_raw(camera.width as u32, camera.height as u32, output)
            .context("Invalid developed dimensions")?;
    let pixels =
        crate::image_processing::apply_orientation(DynamicImage::ImageRgba32F(image), orientation)
            .into_rgba32f();
    check_cancel()?;
    Ok(DecodedFrame { pixels, color: SourceColor {primaries: Primaries::Srgb,transfer: Transfer::Linear,reference: ReferenceDomain::Scene}, provenance: InputProvenance {
        decoder_revision: "v3-bayer-input-1-rawler-424cc109", interpretation: "calibrated_scene_linear_srgb".into(),
        profile_hash: None, calibration: Some(record),
        warnings: vec!["Experimental Bayer calibration; no clipped-sensor highlight reconstruction or dual-illuminant interpolation yet.".into()],
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> RawImage {
        use rawler::{
            cfa::{PlaneColor, CFA},
            decoders::Camera,
            rawimage::{BlackLevel, CFAConfig, WhiteLevel},
        };
        let mut camera = Camera::default();
        camera.cfa = CFA::new("RGGB");
        camera.plane_color = PlaneColor::new("RGB");
        let cfa = CFAConfig::new_from_camera(&camera);
        let mut raw = RawImage::new_with_data(
            camera,
            RawImageData::Float(vec![2100.0; 32 * 32]),
            32,
            32,
            1,
            [2.0, 1.0, 1.5, f32::NAN],
            RawPhotometricInterpretation::Cfa(cfa),
            Some(BlackLevel::new(&[100u32], 1, 1, 1)),
            Some(WhiteLevel::new(vec![1100])),
            false,
        );
        // Identity XYZ->camera is synthetic, not a real camera characterization.
        raw.color_matrix.insert(
            Illuminant::D65,
            vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        );
        raw
    }
    #[test]
    fn sensor_normalization_has_no_upper_ceiling() {
        let mut v = [50., 200., 600., 2200.];
        normalize_sensor(
            &mut v,
            2,
            [100., 100., 100., 200.],
            [1100., 1100., 1100., 1200.],
        )
        .unwrap();
        assert_eq!(v, [0., 0.1, 0.5, 2.]);
        assert!(normalize_sensor(&mut v, 2, [100.; 4], [100.; 4]).is_err());
    }
    #[test]
    fn calibration_preserves_signed_hdr_and_scales_linearly() {
        let raw = fixture();
        let m = calibration(
            raw.color_matrix.get(&Illuminant::D65).unwrap(),
            &raw.wb_coeffs,
        )
        .unwrap();
        let p = m * DVec3::new(2., 0.1, 0.2);
        assert!(p.max_element() > 1. && p.min_element() < 0.);
        assert!((m * (DVec3::new(2., 0.1, 0.2) * 2.) - p * 2.).length() < 1e-10);
        let neutral = m * DVec3::new(0.5, 1., 1. / 1.5);
        assert!((neutral - DVec3::ONE).length() < 1e-10);
        assert!(calibration(&[0.; 9], &raw.wb_coeffs).is_err());
        assert!(calibration(&[1.; 9], &[f32::NAN; 4]).is_err());
    }
    #[test]
    fn real_demosaicers_keep_flat_field_headroom() {
        let full = develop(fixture(), Orientation::Normal, false, || Ok(())).unwrap();
        let fast = develop(fixture(), Orientation::Normal, true, || Ok(())).unwrap();
        let a = full
            .pixels
            .get_pixel(full.pixels.width() / 2, full.pixels.height() / 2);
        let b = fast
            .pixels
            .get_pixel(fast.pixels.width() / 2, fast.pixels.height() / 2);
        assert!(a[0] > 2.5 && b[0] > 2.5, "{a:?} {b:?}");
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() < 1e-4, "{a:?} {b:?}");
        }
        assert_eq!(full.color.reference, ReferenceDomain::Scene);
        assert!(full.provenance.calibration.is_some());
    }
    #[test]
    fn encoded_dng_runs_through_real_decoder_and_development() {
        use rawler::formats::tiff::{writer::TiffWriter, Rational, Value};
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut writer = TiffWriter::new(&mut bytes).unwrap();
            let offset = writer.write_data_u16_le(&vec![2100; 32 * 32]).unwrap();
            let mut directory = writer.new_directory();
            for (tag, value) in [
                (256, 32u32),
                (257, 32),
                (273, offset),
                (278, 32),
                (279, 2048),
                (50717, 1100),
            ] {
                directory.add_untyped_tag(tag, value);
            }
            for (tag, value) in [
                (258, 16u16),
                (259, 1),
                (262, 32803),
                (277, 1),
                (284, 1),
                (274, 1),
                (50778, 21),
                (50714, 100),
            ] {
                directory.add_untyped_tag(tag, value);
            }
            directory.add_untyped_tag(271, "RapidRAW Test");
            directory.add_untyped_tag(272, "Synthetic Bayer");
            directory.add_untyped_tag(50706, Value::Byte(vec![1, 4, 0, 0]));
            directory.add_untyped_tag(33421, Value::Short(vec![2, 2]));
            directory.add_untyped_tag(33422, Value::Byte(vec![0, 1, 1, 2]));
            directory.add_untyped_tag(
                50721,
                Value::Rational(
                    [1, 0, 0, 0, 1, 0, 0, 0, 1]
                        .map(|v| Rational::new(v, 1))
                        .to_vec(),
                ),
            );
            directory.add_untyped_tag(
                50728,
                Value::Rational(vec![
                    Rational::new(1, 2),
                    Rational::new(1, 1),
                    Rational::new(2, 3),
                ]),
            );
            writer.build(directory).unwrap();
        }
        let frame = decode_raw(bytes.get_ref(), false, || Ok(())).unwrap();
        let expected = develop(fixture(), Orientation::Normal, false, || Ok(())).unwrap();
        assert_eq!(frame.pixels.dimensions(), expected.pixels.dimensions());
        for (a, b) in frame.pixels.as_raw().iter().zip(expected.pixels.as_raw()) {
            assert!((a - b).abs() < 1e-5, "{a} {b}");
        }
    }
    #[test]
    fn invalid_metadata_and_cancellation_fail_closed() {
        let mut raw = fixture();
        raw.color_matrix.clear();
        assert!(develop(raw, Orientation::Normal, false, || Ok(())).is_err());
        let mut raw = fixture();
        raw.photometric = RawPhotometricInterpretation::LinearRaw;
        assert!(develop(raw, Orientation::Normal, false, || Ok(())).is_err());
        assert!(
            develop(fixture(), Orientation::Normal, false, || anyhow::bail!(
                "cancelled"
            ))
            .is_err()
        );
    }
}
