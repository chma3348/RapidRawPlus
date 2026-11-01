//! Experimental calibrated development of Bayer and Fujifilm X-Trans
//! sensors, independent of the legacy path.
use super::{
    config::*,
    input::{DecodedFrame, InputProvenance},
    spaces,
};
use anyhow::{Context, Result, ensure};
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

/// RCD over the sensor's active area, then the dependency's own default
/// crop, exactly as its develop steps place them (Demosaic, CropActiveArea,
/// CropDefault), so framing and pixel positions are unchanged.
fn develop_rcd(raw: &RawImage, cfa: &rawler::cfa::CFA) -> rawler::pixarray::Color2D<f32, 3> {
    let mosaic =
        rawler::pixarray::PixF32::new_with(raw.data.as_f32().into_owned(), raw.width, raw.height);
    let roi = raw.active_area.unwrap_or(mosaic.rect());
    let mut rgb = super::rcd::demosaic(&mosaic, cfa, roi);
    if let Some(mut crop) = raw.crop_area {
        if let Some(active) = raw.active_area {
            crop = crop.intersection(&active).adapt(&active);
        }
        if !crop.is_empty() && crop.d != rgb.dim() {
            rgb = rgb.crop(crop);
        }
    }
    rgb
}

/// Cancellation is checked between expensive stages. The dependency's decoder
/// and demosaicer themselves are not interruptible.
pub fn decode_raw(
    bytes: &[u8],
    fast: bool,
    check_cancel: impl Fn() -> Result<()>,
) -> Result<DecodedFrame> {
    check_cancel()?;
    // Floating-point LinearRaw DNGs (HDR merges, ours or Lightroom's) are
    // already demosaiced linear light, which rawler does not read.
    if crate::hdr_dng::is_float_linear_dng(bytes) {
        return crate::hdr_dng::decode_float_dng(bytes);
    }
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
    let mut frame = develop(raw, orientation, fast, check_cancel)?;
    frame.provenance.source_hash = blake3::hash(bytes).to_hex().to_string();
    Ok(frame)
}

/// Discrete source-stage choice; the default preserves historical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    Off,
    #[default]
    NeutralGreenV1,
}

impl Recovery {
    pub fn from_edits(edits: &serde_json::Value) -> Result<Self> {
        match edits.get("v3RawRecovery").filter(|v| !v.is_null()) {
            Some(v) => {
                serde_json::from_value(v.clone()).context("Unsupported v3 RAW recovery mode")
            }
            None => Ok(Self::default()),
        }
    }
    /// Select once on a newly decoded frame. The source cache includes the
    /// mode; switching back develops from the unchanged source, not this output.
    pub(crate) fn apply(self, frame: &mut DecodedFrame) {
        if let Some(original) = &frame.unrecovered {
            if self == Self::Off {
                original.restore(&mut frame.pixels);
                frame
                    .provenance
                    .warnings
                    .retain(|w| !w.contains("clipped highlights are neutralised"));
                frame.provenance.warnings.push("RAW highlight recovery disabled; clipped sensor channels are not reconstructed.".into());
            }
            if let Some(calibration) = frame.provenance.calibration.as_mut() {
                calibration["recovery_mode"] = serde_json::to_value(self).unwrap();
                if self == Self::Off {
                    calibration["clipped_highlights"] = serde_json::json!("preserved_unrecovered");
                }
            }
        }
    }
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

/// Sensor levels repeating over the mosaic from its top-left corner, row
/// by row (DNG's BlackLevelRepeatDim): one level, 2x2 for a Bayer
/// sensor's sites, or 6x6 for an X-Trans one's.
#[derive(Clone, Debug, PartialEq)]
struct SiteLevels {
    levels: Vec<f32>,
    width: usize,
    height: usize,
}

impl SiteLevels {
    fn uniform(level: f32) -> Self {
        Self {
            levels: vec![level],
            width: 1,
            height: 1,
        }
    }
    fn at(&self, row: usize, col: usize) -> f32 {
        self.levels[(row % self.height) * self.width + col % self.width]
    }
}

fn normalize_sensor(
    values: &mut [f32],
    width: usize,
    black: &SiteLevels,
    white: &SiteLevels,
) -> Result<()> {
    ensure!(width > 0, "Empty RAW width");
    for levels in [black, white] {
        ensure!(
            levels.width > 0
                && levels.height > 0
                && levels.levels.len() == levels.width * levels.height
                && levels.levels.iter().all(|v| v.is_finite()),
            "Invalid sensor black/white levels"
        );
    }
    for row in 0..black.height.max(white.height) {
        for col in 0..black.width.max(white.width) {
            ensure!(
                white.at(row, col) > black.at(row, col),
                "Invalid sensor black/white levels"
            );
        }
    }
    for (i, value) in values.iter_mut().enumerate() {
        ensure!(value.is_finite(), "Non-finite sensor sample");
        let (row, col) = (i / width, i % width);
        let (black, white) = (black.at(row, col), white.at(row, col));
        // Sensor black floor only. No upper clamp, highlight desaturation, or
        // RGB gamut clipping. Above-white samples remain available downstream.
        *value = (*value - black).max(0.0) / (white - black);
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
            "V3 RAW currently supports Bayer and X-Trans mosaics, not LinearRaw or monochrome files"
        ),
    };
    let bayer = cfa.cfa.width == 2 && cfa.cfa.height == 2;
    // Fujifilm's X-Trans: a 6x6 pattern, demosaiced by the dependency.
    let xtrans = cfa.cfa.width == 6 && cfa.cfa.height == 6;
    ensure!(
        raw.cpp == 1 && cfa.cfa.is_rgb() && (bayer || xtrans),
        "V3 RAW currently requires an RGB Bayer (2x2) or X-Trans (6x6) mosaic"
    );
    ensure!(
        raw.width >= 16
            && raw.height >= 16
            && (xtrans || (raw.width.is_multiple_of(2) && raw.height.is_multiple_of(2))),
        "Unsupported RAW dimensions"
    );
    // One black level, or one per site of the sensor's pattern.
    let blacks = &raw.blacklevel;
    ensure!(
        blacks.cpp == 1
            && blacks.levels.len() == blacks.width * blacks.height
            && ((blacks.width == 1 && blacks.height == 1)
                || (blacks.width == cfa.cfa.width && blacks.height == cfa.cfa.height)),
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
    let black = SiteLevels {
        levels: blacks.levels.iter().map(|v| v.as_f32()).collect(),
        width: blacks.width,
        height: blacks.height,
    };
    let white = SiteLevels::uniform(raw.whitelevel.0[0] as f32);
    let rcd_cfa = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(config) if !fast && super::rcd::supports(&config.cfa) => {
            Some(config.cfa.clone())
        }
        _ => None,
    };
    let record = serde_json::json!({
        "camera_make": raw.clean_make, "camera_model": raw.clean_model,
        "xyz_to_camera_d65": matrix, "white_balance_rgb": &raw.wb_coeffs[..3],
        "camera_to_linear_srgb": transform.to_cols_array(), "matrix_layout": "column_major",
        "black_levels": &black.levels, "white_levels": &white.levels,
        "demosaic": if fast {"speed"} else if rcd_cfa.is_some() {"rcd"} else {"quality"},
        "sensor_floor": "zero_after_black_subtraction", "upper_clamp": false,
        "clipped_highlights": "neutral_where_green_clips_v1",
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
    normalize_sensor(&mut samples, raw.width, &black, &white)?;
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
    // A 2x2 RGB Bayer sensor is demosaiced with RCD (rcd.rs) at full quality;
    // X-Trans and the fast thumbnail path keep the dependency's own (for
    // X-Trans, its interpolation, or its quicker superpixel when fast).
    let camera = if let Some(cfa) = rcd_cfa {
        develop_rcd(&raw, &cfa)
    } else {
        let intermediate = developer.develop_intermediate(&raw)?;
        let Intermediate::ThreeColor(camera) = intermediate else {
            anyhow::bail!("Demosaicing did not produce three camera channels");
        };
        camera
    };
    check_cancel()?;
    // Clipped highlights. Where the sensor's green saturated, the pixel's
    // colour is gone: white balance then lifts red and blue past the clipped
    // green, and a white sun or window renders magenta (measured on a Sony
    // file: 231/170/217 where Resolve renders 245/242/245). There the colour
    // is made neutral and bright, easing in over the last 5% below
    // saturation. A pixel where only red or blue
    // clipped keeps its colour, so a red light stays red.
    let gains = DVec3::new(
        raw.wb_coeffs[0] as f64,
        raw.wb_coeffs[1] as f64,
        raw.wb_coeffs[2] as f64,
    ) / raw.wb_coeffs[1] as f64;
    let unbalance = transform * DMat3::from_diagonal(gains.recip());
    let smoothstep = |a: f64, b: f64, x: f64| {
        let t = ((x - a) / (b - a)).clamp(0., 1.);
        t * t * (3. - 2. * t)
    };
    let mut output = Vec::with_capacity(camera.data.len() * 4);
    // Only pixels the recovery touches are kept: a full second frame would
    // double a RAW's memory for the fraction of a percent that clips.
    let mut unrecovered = super::input::Unrecovered::default();
    for (index, p) in camera.data.iter().enumerate() {
        let sensor = DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
        let balanced = sensor * gains;
        // Use exactly the same matrix path as the historical recovery stage.
        // Keep this independent buffer so disabling recovery never attempts
        // to invert the destructive neutralisation operation.
        let clipped = smoothstep(0.95, 1.0, sensor.y);
        if clipped > 0. {
            let original = unbalance * balanced;
            let original = [original.x as f32, original.y as f32, original.z as f32];
            ensure!(
                original.iter().all(|v| v.is_finite()),
                "Non-finite unrecovered RAW pixel"
            );
            unrecovered.pixels.push((index as u32, original));
        }
        let balanced = if clipped > 0. {
            // A clipped sensor says only "at least this bright"; like
            // Resolve, read a fully clipped core as brighter than its
            // brightest channel, here up to twice.
            balanced.lerp(
                DVec3::splat(balanced.max_element() * (1. + clipped)),
                clipped,
            )
        } else {
            balanced
        };
        let rgb = unbalance * balanced;
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
    // Indices were taken before orientation; map them through the same
    // orientation by carrying a marker plane along.
    let unrecovered = unrecovered.oriented(camera.width as u32, camera.height as u32, orientation);
    check_cancel()?;
    Ok(DecodedFrame { pixels, unrecovered: Some(unrecovered), color: SourceColor {primaries: Primaries::Srgb,transfer: Transfer::Linear,reference: ReferenceDomain::Scene}, source_profile: None, rendered_origin: false, input_domain: None, provenance: InputProvenance {
        source_hash: String::new(), transform_decision: "camera_calibration_to_linear_srgb_scene".into(),
        decoder_revision: "v3-bayer-input-1-rawler-424cc109", interpretation: "calibrated_scene_linear_srgb".into(),
        profile_hash: None, calibration: Some(record),
        warnings: vec!["Experimental sensor calibration; clipped highlights are neutralised rather than reconstructed; no dual-illuminant interpolation yet.".into()],
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_is_explicit_and_original_pixels_are_retained() {
        let mut frame = develop(fixture(), Orientation::Normal, false, || Ok(())).unwrap();
        let historical = frame.pixels.clone();
        let mut original = historical.clone();
        frame.unrecovered.as_ref().unwrap().restore(&mut original);
        assert_ne!(historical, original);
        assert!(!frame.unrecovered.as_ref().unwrap().pixels.is_empty());
        Recovery::NeutralGreenV1.apply(&mut frame);
        assert_eq!(frame.pixels, historical);
        Recovery::Off.apply(&mut frame);
        assert_eq!(frame.pixels, original);
        let reopened: serde_json::Value =
            serde_json::from_str(r#"{"v3RawRecovery":"off"}"#).unwrap();
        assert_eq!(Recovery::from_edits(&reopened).unwrap(), Recovery::Off);
        assert!(Recovery::from_edits(&serde_json::json!({"v3RawRecovery":"future"})).is_err());
        assert_eq!(
            Recovery::from_edits(&serde_json::json!({})).unwrap(),
            Recovery::NeutralGreenV1
        );
        let again = develop(fixture(), Orientation::Normal, false, || Ok(())).unwrap();
        assert_eq!(again.pixels, historical);
    }
    #[test]
    fn unrecovered_pixels_follow_the_orientation() {
        // Restoring the originals on a rotated frame must equal rotating the
        // frame restored upright.
        let mut upright = develop(fixture(), Orientation::Normal, false, || Ok(())).unwrap();
        let mut rotated = develop(fixture(), Orientation::Rotate90, false, || Ok(())).unwrap();
        Recovery::Off.apply(&mut upright);
        Recovery::Off.apply(&mut rotated);
        let expected = crate::image_processing::apply_orientation(
            DynamicImage::ImageRgba32F(upright.pixels),
            Orientation::Rotate90,
        )
        .into_rgba32f();
        assert_eq!(rotated.pixels, expected);
    }

    fn fixture() -> RawImage {
        use rawler::{
            cfa::{CFA, PlaneColor},
            decoders::Camera,
            rawimage::{BlackLevel, CFAConfig, WhiteLevel},
        };
        let camera = Camera {
            cfa: CFA::new("RGGB"),
            plane_color: PlaneColor::new("RGB"),
            ..Camera::default()
        };
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
        let sites = |levels: [f32; 4]| SiteLevels {
            levels: levels.to_vec(),
            width: 2,
            height: 2,
        };
        normalize_sensor(
            &mut v,
            2,
            &sites([100., 100., 100., 200.]),
            &sites([1100., 1100., 1100., 1200.]),
        )
        .unwrap();
        assert_eq!(v, [0., 0.1, 0.5, 2.]);
        let flat = SiteLevels::uniform(100.);
        assert!(normalize_sensor(&mut v, 2, &flat, &flat).is_err());
    }
    /// A Fujifilm X-Trans mosaic develops: the sensor's 6x6 pattern, a black
    /// level for each of its 36 sites, flat grey light.
    #[test]
    fn xtrans_mosaics_develop() {
        use rawler::{
            cfa::{CFA, PlaneColor},
            decoders::Camera,
            rawimage::{BlackLevel, CFAConfig, WhiteLevel},
        };
        // The X-T3's layout, as rawler reads it from a RAF.
        let pattern = "GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG";
        let camera = Camera {
            cfa: CFA::new(pattern),
            plane_color: PlaneColor::new("RGB"),
            ..Camera::default()
        };
        let cfa = CFAConfig::new_from_camera(&camera);
        let (w, h) = (66, 42);
        // Each site's black differs a little; the light above it is the same.
        let blacks: Vec<u32> = (0..36).map(|i| 1000 + i).collect();
        let data = (0..w * h)
            .map(|i| (1000 + ((i / w) % 6) * 6 + (i % w) % 6) as f32 + 2000.0)
            .collect();
        let mut raw = RawImage::new_with_data(
            camera,
            RawImageData::Float(data),
            w,
            h,
            1,
            [1.0, 1.0, 1.0, f32::NAN],
            RawPhotometricInterpretation::Cfa(cfa),
            Some(BlackLevel::new(&blacks, 6, 6, 1)),
            Some(WhiteLevel::new(vec![16383])),
            false,
        );
        raw.color_matrix.insert(
            Illuminant::D65,
            vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        );
        let expected = 2000.0 / (16383.0 - 1000.0);
        for fast in [false, true] {
            let frame = develop(raw.clone(), Orientation::Normal, fast, || Ok(())).unwrap();
            let (fw, fh) = frame.pixels.dimensions();
            assert_eq!((fw, fh), (w as u32, h as u32));
            // Away from the edges every pixel is the same neutral grey.
            for y in fh / 4..fh * 3 / 4 {
                for x in fw / 4..fw * 3 / 4 {
                    let p = frame.pixels.get_pixel(x, y);
                    for c in 0..3 {
                        assert!(
                            (p[c] - expected).abs() < 0.01 * expected,
                            "fast {fast} at {x},{y}: {p:?}, expected {expected}"
                        );
                    }
                }
            }
        }
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
        use rawler::formats::tiff::{Rational, Value, writer::TiffWriter};
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
            directory.add_untyped_tag(271, "Darkroom Index Test");
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.dng");
        std::fs::write(&path, bytes.get_ref()).unwrap();
        let state = crate::AppState::default();
        let get = |edits| {
            super::super::application::source_with_edits(&state, path.to_str().unwrap(), &edits)
                .unwrap()
        };
        let on = get(serde_json::json!({}));
        let pipeline = super::super::identity::pin(&state, &serde_json::json!({})).unwrap();
        let off = get(serde_json::json!({"v3Pipeline":pipeline,"v3RawRecovery":"off"}));
        let restored = get(serde_json::json!({"v3RawRecovery":"neutral_green_v1"}));
        let mut expected_off = frame.pixels.clone();
        frame
            .unrecovered
            .as_ref()
            .unwrap()
            .restore(&mut expected_off);
        // The app develops a RAW with its starting look after recovery.
        super::super::raw_look::apply(&mut expected_off);
        assert_eq!(off.pixels, expected_off);
        assert_ne!(off.pixels, on.pixels);
        assert_eq!(restored.pixels, on.pixels);
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
