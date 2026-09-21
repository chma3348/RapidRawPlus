//! Profile-aware input for the opt-in engine. Never used by legacy edits.
use super::config::{Primaries, ReferenceDomain, SourceColor, Transfer};
use anyhow::{ensure, Context, Result};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Rgba32FImage};
use moxcms::{
    ColorProfile, DataColorSpace, Layout, ProfileClass, RenderingIntent, ToneReprCurve,
    TransformOptions,
};
use serde::Serialize;
use std::io::Cursor;

#[derive(Clone, Debug, Serialize)]
pub struct InputProvenance {
    pub decoder_revision: &'static str,
    pub interpretation: String,
    pub profile_hash: Option<String>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration: Option<serde_json::Value>,
}

pub struct DecodedFrame {
    pub pixels: Rgba32FImage,
    pub color: SourceColor,
    pub provenance: InputProvenance,
}

/// Returns straight-alpha, linear sRGB coordinates of a display-referred
/// image. Out-of-sRGB colors retain negative/above-one values until output.
/// The reference domain remains Display: a linear transfer is not scene data.
pub fn decode_profiled_photo(bytes: &[u8]) -> Result<DecodedFrame> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader.format().context("Unrecognized photo format")?;
    ensure!(matches!(format, ImageFormat::Png | ImageFormat::Jpeg),
        "Automatic v3 interpretation currently supports PNG/JPEG only; RAW, TIFF, HEIC and HDR need their dedicated adapters");
    let mut decoder = reader.into_decoder()?;
    let icc = decoder
        .icc_profile()
        .context("Cannot read embedded ICC profile")?;
    if format == ImageFormat::Png {
        let png = png::Decoder::new(Cursor::new(bytes)).read_info()?;
        let info = png.info();
        ensure!(
            info.coding_independent_code_points.is_none()
                && info.mastering_display_color_volume.is_none()
                && info.content_light_level.is_none(),
            "PNG CICP/HDR input requires an explicit supported input adapter"
        );
    }
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let mut warnings = vec![];
    let (profile, interpretation) = if let Some(ref data) = icc {
        (
            ColorProfile::new_from_slice(data)
                .context("Invalid ICC profile; refusing to assume sRGB")?,
            "embedded_rgb_icc",
        )
    } else {
        if format == ImageFormat::Png {
            let png = png::Decoder::new(Cursor::new(bytes)).read_info()?;
            let info = png.info();
            // Do not silently overwrite alternate gamma/chromaticity tags.
            ensure!(info.srgb.is_some() || (info.gama_chunk.is_none() && info.chrm_chunk.is_none()),
                "PNG declares gamma/chromaticities without an ICC or sRGB profile; cannot assume sRGB");
        }
        // EXIF Adobe RGB/uncalibrated cannot be safely treated as untagged sRGB.
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) {
            if let Some(value) = exif
                .get_field(exif::Tag::ColorSpace, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
            {
                ensure!(
                    value == 1,
                    "EXIF declares a non-sRGB or uncalibrated color space without an ICC profile"
                );
            }
        }
        warnings.push("No embedded ICC profile: using the documented sRGB fallback for standard PNG/JPEG photos.".into());
        (
            ColorProfile::new_from_slice(&ColorProfile::new_srgb().encode()?)?,
            "srgb_fallback",
        )
    };
    ensure!(profile.color_space == DataColorSpace::Rgb,
        "Only RGB ICC profiles are supported by this adapter; refusing to apply a gray/CMYK profile to decoded RGB pixels");
    ensure!(
        matches!(
            profile.profile_class,
            ProfileClass::InputDevice | ProfileClass::DisplayDevice | ProfileClass::ColorSpace
        ),
        "This ICC profile is not a supported source RGB characterization"
    );
    let pixels = convert_rgb_profile(&image.to_rgba32f(), &profile)?;
    Ok(DecodedFrame {
        pixels,
        color: SourceColor {
            primaries: Primaries::Srgb,
            transfer: Transfer::Linear,
            reference: ReferenceDomain::Display,
        },
        provenance: InputProvenance {
            decoder_revision: "v3-photo-input-1-image-0.25.10-moxcms-0.8.1",
            interpretation: interpretation.into(),
            profile_hash: icc.map(|p| blake3::hash(&p).to_hex().to_string()),
            warnings,
            calibration: None,
        },
    })
}

fn convert_rgb_profile(input: &Rgba32FImage, profile: &ColorProfile) -> Result<Rgba32FImage> {
    // Match the ICC actually embedded in our exports. ICC colorants are fixed-
    // point; comparing a parsed profile against unquantized built-in colorants
    // introduces a small matrix mismatch, amplified by encoding near black.
    let mut destination = ColorProfile::new_from_slice(&ColorProfile::new_srgb().encode()?)?;
    destination.cicp = None;
    destination.red_trc = Some(ToneReprCurve::Parametric(vec![1.0]));
    destination.green_trc = destination.red_trc.clone();
    destination.blue_trc = destination.red_trc.clone();
    let transform = profile.create_transform_f32(
        Layout::Rgba,
        &destination,
        Layout::Rgba,
        TransformOptions {
            rendering_intent: RenderingIntent::RelativeColorimetric,
            prefer_fixed_point: false,
            allow_extended_range_rgb_xyz: true,
            ..Default::default()
        },
    )?;
    let mut pixels = vec![0.0; input.as_raw().len()];
    transform.transform(input.as_raw(), &mut pixels)?;
    // Alpha is coverage, never a color coordinate.
    for (src, dst) in input
        .as_raw()
        .chunks_exact(4)
        .zip(pixels.chunks_exact_mut(4))
    {
        dst[3] = src[3];
    }
    ensure!(
        pixels.iter().all(|v| v.is_finite()),
        "ICC conversion produced non-finite pixels"
    );
    image::ImageBuffer::from_raw(input.width(), input.height(), pixels)
        .context("Invalid input dimensions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, ImageEncoder, Rgba};

    fn png_with_profile(profile: Option<Vec<u8>>) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        if let Some(profile) = profile {
            encoder.set_icc_profile(profile).unwrap();
        }
        encoder
            .write_image(&[128, 64, 192, 100], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        bytes
    }

    #[test]
    fn fallback_decodes_srgb_once_and_preserves_alpha() {
        let frame = decode_profiled_photo(&png_with_profile(None)).unwrap();
        let pixel = frame.pixels.get_pixel(0, 0);
        for (i, value) in [128.0, 64.0, 192.0].iter().enumerate() {
            let expected = super::super::spaces::decode(value / 255.0, Transfer::Srgb) as f32;
            assert!(
                (pixel[i] - expected).abs() < 2e-5,
                "{pixel:?} expected {expected}"
            );
        }
        assert_eq!(pixel[3], 100.0 / 255.0);
        assert_eq!(frame.color.reference, ReferenceDomain::Display);
        assert_eq!(frame.provenance.warnings.len(), 1);
    }

    #[test]
    fn tagged_p3_preserves_out_of_srgb_headroom() {
        let p3 = ColorProfile::new_display_p3();
        let input = ImageBuffer::from_pixel(1, 1, Rgba([1.0, 0.0, 0.0, 0.3]));
        let output = convert_rgb_profile(&input, &p3).unwrap();
        let p = output.get_pixel(0, 0);
        assert!(
            p[0] > 1.2 && p[1] < -0.03 && p[2] < -0.01,
            "P3 red clipped: {p:?}"
        );
        assert_eq!(p[3], 0.3);
        let frame = decode_profiled_photo(&png_with_profile(Some(p3.encode().unwrap()))).unwrap();
        assert!(frame.provenance.profile_hash.is_some());
        assert!(frame.provenance.warnings.is_empty());
    }

    #[test]
    fn corrupt_profile_and_ambiguous_gamma_fail_closed() {
        assert!(decode_profiled_photo(&png_with_profile(Some(vec![0; 128]))).is_err());
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_source_gamma(png::ScaledFloat::new(1.0));
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[128]).unwrap();
        }
        assert!(decode_profiled_photo(&bytes).is_err());
    }

    #[test]
    fn gray_profiles_are_not_applied_to_rgb_pixels() {
        let profile = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
        assert!(decode_profiled_photo(&png_with_profile(Some(profile))).is_err());
    }

    #[test]
    fn jpeg_fallback_remains_display_referred() {
        let image =
            DynamicImage::ImageRgb8(ImageBuffer::from_pixel(8, 8, image::Rgb([128, 128, 128])));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Jpeg).unwrap();
        let frame = decode_profiled_photo(bytes.get_ref()).unwrap();
        assert_eq!(frame.color.reference, ReferenceDomain::Display);
        assert_eq!(frame.color.transfer, Transfer::Linear);
        assert!((frame.pixels.get_pixel(0, 0)[0] - 0.216).abs() < 0.005);
    }

    #[test]
    fn embedded_orientation_is_applied_once() {
        let exif = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder.set_exif_metadata(exif.to_vec()).unwrap();
        encoder
            .write_image(
                &[255, 0, 0, 255, 0, 0, 255, 128],
                2,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let frame = decode_profiled_photo(&bytes).unwrap();
        assert_eq!(frame.pixels.dimensions(), (1, 2));
        assert!(frame.pixels.get_pixel(0, 0)[0] > 0.99);
        assert_eq!(frame.pixels.get_pixel(0, 1)[3], 128.0 / 255.0);
    }

    #[test]
    fn tagged_and_assumed_srgb_match() {
        let untagged = decode_profiled_photo(&png_with_profile(None)).unwrap();
        let tagged = decode_profiled_photo(&png_with_profile(Some(
            ColorProfile::new_srgb().encode().unwrap(),
        )))
        .unwrap();
        assert_eq!(untagged.pixels, tagged.pixels);
    }
}
