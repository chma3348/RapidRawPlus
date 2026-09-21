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
    // HEIC, AVIF and Photoshop documents: macOS decodes them to PNG with the
    // source's ICC profile embedded, so they go through exactly the same
    // interpretation as a PNG. The PNG has no orientation, so the original
    // file's is applied afterwards.
    if let Some(extension) = crate::image_loader::system_codec_format(bytes) {
        let png = crate::image_loader::system_codec_png(bytes, extension)?;
        let mut frame = decode_profiled_photo(&png)?;
        if let Some(orientation) = exif_orientation(bytes) {
            let mut image = DynamicImage::ImageRgba32F(frame.pixels);
            image.apply_orientation(orientation);
            frame.pixels = image.to_rgba32f();
        }
        frame.provenance.interpretation =
            format!("{} (decoded by macOS, {extension})", frame.provenance.interpretation);
        return Ok(frame);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    // The default allocation limit is sized by pixel count, and on a small
    // TIFF it is smaller than the embedded ICC profile, which then silently
    // reads as absent — and a wide-gamut file as sRGB.
    reader.no_limits();
    let format = reader.format().context("Unrecognized photo format")?;
    ensure!(
        matches!(
            format,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Tiff | ImageFormat::WebP
        ),
        "Automatic v3 interpretation supports PNG, JPEG, TIFF, WebP, HEIC, AVIF and PSD; RAW and HDR formats need their dedicated adapters"
    );
    let mut decoder = reader.into_decoder()?;
    let icc = decoder
        .icc_profile()
        .context("Cannot read embedded ICC profile")?;
    // The image crate reads a TIFF's ICC tag only when it is typed BYTE; the
    // TIFF specification types it UNDEFINED, and then it silently reports no
    // profile at all — which would misread every wide-gamut TIFF as sRGB.
    let icc = match icc {
        None if format == ImageFormat::Tiff => tiff_icc_profile(bytes),
        found => found,
    };
    if format == ImageFormat::Png {
        let png = png::Decoder::new(Cursor::new(bytes)).read_info()?;
        let info = png.info();
        ensure!(
            info.mastering_display_color_volume.is_none() && info.content_light_level.is_none(),
            "PNG HDR mastering metadata requires an explicit supported input adapter"
        );
        // A CICP tag is accepted only where it cannot mean anything the ICC
        // profile does not: alongside one, describing an SDR curve in
        // full-range RGB. macOS writes exactly that when it decodes HEIC,
        // AVIF and Photoshop files. PQ, HLG, video matrices and narrow range
        // stay refused — those are HDR or video data, not a photograph.
        if let Some(cicp) = &info.coding_independent_code_points {
            let sdr_curve = matches!(cicp.transfer_function, 1 | 4 | 6 | 8 | 13 | 14 | 15);
            ensure!(
                icc.is_some() && sdr_curve && cicp.matrix_coefficients == 0 && cicp.is_video_full_range_image,
                "PNG CICP declares HDR or video data (transfer {}, matrix {}); that needs an explicit supported input adapter",
                cicp.transfer_function,
                cicp.matrix_coefficients
            );
        }
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

/// The ICC profile of a classic (not Big) TIFF, read from its first IFD:
/// tag 34675, typed BYTE or UNDEFINED, inline or at an offset.
fn tiff_icc_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    let little = match bytes.get(..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    for i in 0..entries {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry)? != 34675 {
            continue;
        }
        if !matches!(u16_at(entry + 2)?, 1 | 7) {
            return None;
        }
        let count = u32_at(entry + 4)? as usize;
        let start = if count <= 4 { entry + 8 } else { u32_at(entry + 8)? as usize };
        return bytes.get(start..start.checked_add(count)?).map(<[u8]>::to_vec);
    }
    None
}

/// The EXIF orientation of the original file, for formats whose pixels are
/// decoded without it.
fn exif_orientation(bytes: &[u8]) -> Option<image::metadata::Orientation> {
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()?;
    let value = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)?;
    image::metadata::Orientation::from_exif(value as u8)
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
    // One call over the whole image runs on one core; the transform is
    // per-pixel, so blocks of it can run on all of them.
    use rayon::prelude::*;
    let mut pixels = vec![0.0; input.as_raw().len()];
    const BLOCK: usize = 16384 * 4;
    pixels
        .par_chunks_mut(BLOCK)
        .zip(input.as_raw().par_chunks(BLOCK))
        .try_for_each(|(dst, src)| {
            transform.transform(src, dst)?;
            // Alpha is coverage, never a color coordinate.
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                d[3] = s[3];
            }
            anyhow::ensure!(
                dst.iter().all(|v| v.is_finite()),
                "ICC conversion produced non-finite pixels"
            );
            Ok::<(), anyhow::Error>(())
        })?;
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

    /// Real files of every system-decoded format, converted from a Display
    /// P3 JPEG, when the scratch fixtures exist on this machine.
    #[test]
    #[cfg(target_os = "macos")]
    fn system_codec_formats_keep_their_profile() {
        let dir = std::env::temp_dir().join("rapidraw_codec_fixture");
        let _ = std::fs::create_dir_all(&dir);
        // A small wide-gamut source: saturated P3 red, which sRGB cannot hold.
        let source = dir.join("p3.png");
        image::RgbImage::from_pixel(16, 8, image::Rgb([255, 0, 0])).save(&source).unwrap();
        let tagged = dir.join("p3-tagged.png");
        let profile = "/System/Library/ColorSync/Profiles/Display P3.icc";
        if !std::path::Path::new(profile).exists() {
            return;
        }
        let ok = std::process::Command::new("sips")
            .args(["--embedProfile", profile])
            .arg(&source)
            .arg("--out")
            .arg(&tagged)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return;
        }
        let reference = decode_profiled_photo(&std::fs::read(&tagged).unwrap()).unwrap();
        let red = reference.pixels.get_pixel(0, 0).0;
        assert!(red[0] > 1.0 || red[1] < 0.0, "P3 red should fall outside sRGB: {red:?}");
        for format in ["heic", "avif", "psd", "tiff"] {
            let out = dir.join(format!("p3.{format}"));
            let converted = std::process::Command::new("sips")
                .args(["-s", "format", format])
                .arg(&tagged)
                .arg("--out")
                .arg(&out)
                .output()
                .is_ok_and(|o| o.status.success());
            if !converted {
                continue;
            }
            let frame = decode_profiled_photo(&std::fs::read(&out).unwrap())
                .unwrap_or_else(|e| panic!("{format}: {e}"));
            let got = frame.pixels.get_pixel(0, 0).0;
            // Lossy formats move the value a little; the profile must not be
            // lost, which would move it a lot.
            for c in 0..3 {
                assert!(
                    (got[c] - red[c]).abs() < 0.06,
                    "{format} lost its colour profile: {got:?} vs {red:?}"
                );
            }
            assert_eq!(frame.pixels.dimensions(), (16, 8), "{format} changed size");
        }
    }

    /// Decode cost on a real 33-megapixel JPEG, when it is on this machine.
    /// `cargo test --release --lib decode_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decode_cost() {
        let path = std::path::Path::new(&std::env::var("HOME").unwrap())
            .join("Desktop/Test Photos/NYC/DSC08270.JPG");
        let Ok(bytes) = std::fs::read(&path) else { return };
        let start = std::time::Instant::now();
        let frame = decode_profiled_photo(&bytes).unwrap();
        println!(
            "decode + profile, {}x{}: {:.0} ms",
            frame.pixels.width(),
            frame.pixels.height(),
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
