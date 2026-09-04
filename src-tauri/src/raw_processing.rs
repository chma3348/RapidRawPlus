use crate::image_processing::apply_orientation;
use anyhow::{Result, anyhow};
use image::{DynamicImage, ImageBuffer, Rgba};
use rawler::{
    decoders::{Orientation, RawDecodeParams},
    imgop::develop::{DemosaicAlgorithm, Intermediate, ProcessingStep, RawDevelop},
    rawimage::{RawImage, RawPhotometricInterpretation},
    rawsource::RawSource,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub fn develop_raw_image(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<DynamicImage> {
    let (developed_image, orientation) = develop_internal(
        file_bytes,
        fast_demosaic,
        highlight_compression,
        linear_mode,
        cancel_token,
    )?;
    Ok(apply_orientation(developed_image, orientation))
}

fn is_linear_raw_format(raw_image: &RawImage) -> bool {
    matches!(
        raw_image.photometric,
        RawPhotometricInterpretation::LinearRaw
    )
}

#[inline]
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        // sRGB EOTF exponent is 2.4; this read 3.0, which darkened and
        // desaturated every LinearRaw file (DNG linear and similar).
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn develop_internal(
    file_bytes: &[u8],
    fast_demosaic: bool,
    highlight_compression: f32,
    linear_mode: String,
    cancel_token: Option<(Arc<AtomicUsize>, usize)>,
) -> Result<(DynamicImage, Orientation)> {
    let check_cancel = || -> Result<()> {
        if let Some((tracker, generation)) = &cancel_token
            && tracker.load(Ordering::SeqCst) != *generation
        {
            return Err(anyhow!("Load cancelled"));
        }
        Ok(())
    };

    check_cancel()?;

    let source = RawSource::new_from_slice(file_bytes);
    let decoder = rawler::get_decoder(&source)?;

    check_cancel()?;
    let mut raw_image: RawImage = decoder.raw_image(&source, &RawDecodeParams::default(), false)?;

    let metadata = decoder.raw_metadata(&source, &RawDecodeParams::default())?;
    let orientation = metadata
        .exif
        .orientation
        .map(Orientation::from_u16)
        .unwrap_or(Orientation::Normal);

    let is_linear_format = is_linear_raw_format(&raw_image);

    let (apply_ungamma, apply_calibration) = match linear_mode.as_str() {
        "gamma" => (true, true),
        "skip_calib" => (false, false),
        "gamma_skip_calib" => (true, false),
        _ => (false, true),
    };

    let original_white_level = raw_image
        .whitelevel
        .0
        .first()
        .cloned()
        .unwrap_or(u16::MAX as u32) as f32;
    let original_black_level = raw_image
        .blacklevel
        .levels
        .first()
        .map(|r| r.as_f32())
        .unwrap_or(0.0);

    for level in raw_image.whitelevel.0.iter_mut() {
        *level = u32::MAX;
    }

    let mut developer = RawDevelop::default();

    if is_linear_format {
        developer.steps.retain(|&step| {
            step != ProcessingStep::SRgb
                && step != ProcessingStep::Demosaic
                && (apply_calibration || step != ProcessingStep::Calibrate)
        });
    } else if fast_demosaic {
        developer.demosaic_algorithm = DemosaicAlgorithm::Speed;
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    } else {
        developer.steps.retain(|&step| step != ProcessingStep::SRgb);
    }

    check_cancel()?;
    let mut developed_intermediate = developer.develop_intermediate(&raw_image)?;

    drop(raw_image);

    let denominator = (original_white_level - original_black_level).max(1.0);
    let rescale_factor = (u32::MAX as f32 - original_black_level) / denominator;

    let safe_highlight_compression = highlight_compression.max(1.01);

    let clamp_limit = if fast_demosaic {
        1.0
    } else {
        safe_highlight_compression
    };

    check_cancel()?;

    match &mut developed_intermediate {
        Intermediate::Monochrome(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                let mut linear_val = *p * rescale_factor;
                if is_linear_format && apply_ungamma {
                    linear_val = srgb_to_linear(linear_val.clamp(0.0, 1.0));
                }
                *p = linear_val.clamp(0.0, clamp_limit);
            });
        }
        Intermediate::ThreeColor(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                let mut r = (p[0] * rescale_factor).max(0.0);
                let mut g = (p[1] * rescale_factor).max(0.0);
                let mut b = (p[2] * rescale_factor).max(0.0);

                if is_linear_format && apply_ungamma {
                    r = srgb_to_linear(r.clamp(0.0, 1.0));
                    g = srgb_to_linear(g.clamp(0.0, 1.0));
                    b = srgb_to_linear(b.clamp(0.0, 1.0));
                }

                let max_c = r.max(g).max(b);

                let (final_r, final_g, final_b) = if max_c > 1.0 {
                    let min_c = r.min(g).min(b);
                    // Chroma rolls off asymptotically. The old ramp was linear
                    // and reached exactly ZERO at the limit, so any highlight
                    // further over than `safe_highlight_compression` lost all
                    // colour and became a flat grey disc. Measured by decoding
                    // a sunset frame (DSC03520.ARW, sun peaking at 3.1 against
                    // the 2.5 default), the brightest 0.1% of pixels came back
                    // with saturation 0.0000 -- a grey sun, which is exactly
                    // the "white cast" this looked like next to Resolve, whose
                    // own render keeps a white core inside a golden surround.
                    //
                    // Desaturating clipped highlights is still right: once a
                    // channel saturates its true colour is unknown, and holding
                    // it produces magenta suns. But it should approach neutral,
                    // not snap to it. 1/(1+x^2) matches the old curve closely
                    // where it mattered (0.5 at the limit against the old 0.0)
                    // and never quite reaches zero.
                    let over = ((max_c - 1.0) / (safe_highlight_compression - 1.0)).max(0.0);
                    let compression_factor = 1.0 / (1.0 + over * over);
                    let compressed_r = min_c + (r - min_c) * compression_factor;
                    let compressed_g = min_c + (g - min_c) * compression_factor;
                    let compressed_b = min_c + (b - min_c) * compression_factor;
                    let compressed_max = compressed_r.max(compressed_g).max(compressed_b);

                    if compressed_max > 1e-6 {
                        let rescale = max_c / compressed_max;
                        (
                            compressed_r * rescale,
                            compressed_g * rescale,
                            compressed_b * rescale,
                        )
                    } else {
                        (max_c, max_c, max_c)
                    }
                } else {
                    (r, g, b)
                };

                p[0] = final_r.clamp(0.0, clamp_limit);
                p[1] = final_g.clamp(0.0, clamp_limit);
                p[2] = final_b.clamp(0.0, clamp_limit);
            });
        }
        Intermediate::FourColor(pixels) => {
            pixels.data.iter_mut().for_each(|p| {
                p.iter_mut().for_each(|c| {
                    let mut linear_val = *c * rescale_factor;
                    if is_linear_format && apply_ungamma {
                        linear_val = srgb_to_linear(linear_val.clamp(0.0, 1.0));
                    }
                    *c = linear_val.clamp(0.0, clamp_limit);
                });
            });
        }
    }

    let (width, height) = {
        let dim = developed_intermediate.dim();
        (dim.w as u32, dim.h as u32)
    };

    check_cancel()?;

    let dynamic_image = match developed_intermediate {
        Intermediate::ThreeColor(pixels) => {
            let buffer = ImageBuffer::<Rgba<f32>, _>::from_fn(width, height, |x, y| {
                let p = pixels.data[(y * width + x) as usize];
                Rgba([p[0], p[1], p[2], 1.0])
            });
            DynamicImage::ImageRgba32F(buffer)
        }
        Intermediate::Monochrome(pixels) => {
            let buffer = ImageBuffer::<Rgba<f32>, _>::from_fn(width, height, |x, y| {
                let p = pixels.data[(y * width + x) as usize];
                Rgba([p, p, p, 1.0])
            });
            DynamicImage::ImageRgba32F(buffer)
        }
        _ => {
            return Err(anyhow!("Unsupported intermediate format for conversion"));
        }
    };

    Ok((dynamic_image, orientation))
}

pub fn get_fast_demosaic_scale_factor(
    file_bytes: &[u8],
    decoded_width: u32,
    decoded_height: u32,
) -> f32 {
    let source = RawSource::new_from_slice(file_bytes);
    if let Ok(decoder) = rawler::get_decoder(&source)
        && let Ok(raw_img) = decoder.raw_image(&source, &RawDecodeParams::default(), true)
    {
        let max_orig = (raw_img.width as f32).max(raw_img.height as f32);
        let max_comp = (decoded_width as f32).max(decoded_height as f32);
        if max_orig > 0.0 {
            let ratio = max_comp / max_orig;
            if ratio > 0.1 && ratio < 0.35 {
                return 0.25;
            } else if (0.35..0.75).contains(&ratio) {
                return 0.5;
            }
        }
    }
    1.0
}

#[cfg(test)]
mod decode_probe {
    /// Ad-hoc probe: decode a RAW at several Highlight Recovery settings and
    /// report what happens to the brightest pixels. Needs a real file, so it
    /// is ignored by default:
    ///   RAPIDRAW_TEST_RAW=/path/to.ARW cargo test --lib decode_probe -- --ignored --nocapture
    /// Where does the brightness go? Report the max at each stage of loading.
    #[test]
    #[ignore]
    fn brightness_bisect() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_RAW") else { return };
        let bytes = std::fs::read(&path).expect("read raw");
        let stats = |label: &str, img: &image::DynamicImage| {
            let rgb = img.to_rgba32f();
            let mut mx: Vec<f32> = rgb.pixels().map(|p| p.0[0].max(p.0[1]).max(p.0[2])).collect();
            mx.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let q = |f: f64| mx[((mx.len() - 1) as f64 * f) as usize];
            println!(
                "  {label:28} max {:.3}  p99.9 {:.3}  p99 {:.3}  p50 {:.3}  above 1.0: {:.3}%",
                mx[mx.len() - 1], q(0.999), q(0.99), q(0.50),
                100.0 * mx.iter().filter(|v| **v > 1.0).count() as f64 / mx.len() as f64
            );
        };
        let developed = super::develop_raw_image(&bytes, false, 2.5, "auto".to_string(), None)
            .expect("develop");
        stats("after develop_raw_image", &developed);
        let settings = crate::AppSettings::default();
        let loaded = crate::image_loader::load_base_image_from_bytes(
            &bytes, &path, false, &settings, None,
        )
        .expect("load");
        stats("after load_base_image", &loaded);
    }

    #[test]
    #[ignore]
    fn highlight_recovery_sweep() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_RAW") else {
            eprintln!("set RAPIDRAW_TEST_RAW");
            return;
        };
        let bytes = std::fs::read(&path).expect("read raw");
        for hc in [1.5f32, 2.5, 5.0, 8.0] {
            let img = super::develop_raw_image(&bytes, false, hc, "auto".to_string(), None)
                .expect("develop");
            let rgb = img.to_rgba32f();
            let px: Vec<[f32; 3]> = rgb.pixels().map(|p| [p.0[0], p.0[1], p.0[2]]).collect();
            let mut luma: Vec<f32> = px
                .iter()
                .map(|p| p[0] * 0.2126 + p[1] * 0.7152 + p[2] * 0.0722)
                .collect();
            luma.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let thr = luma[(luma.len() as f64 * 0.999) as usize];
            let bright: Vec<&[f32; 3]> = px
                .iter()
                .filter(|p| p[0] * 0.2126 + p[1] * 0.7152 + p[2] * 0.0722 >= thr)
                .collect();
            let sat = |p: &[f32; 3]| {
                let mx = p[0].max(p[1]).max(p[2]);
                let mn = p[0].min(p[1]).min(p[2]);
                if mx > 1e-6 { (mx - mn) / mx } else { 0.0 }
            };
            let mean_sat: f32 = bright.iter().map(|p| sat(p)).sum::<f32>() / bright.len() as f32;
            let mean_max: f32 =
                bright.iter().map(|p| p[0].max(p[1]).max(p[2])).sum::<f32>() / bright.len() as f32;
            let above_one = px
                .iter()
                .filter(|p| p[0].max(p[1]).max(p[2]) > 1.0)
                .count() as f64
                / px.len() as f64;
            println!(
                "  hc {hc:>4.1}: top-0.1% mean sat {mean_sat:.4}  mean max-channel {mean_max:.3}  \
                 pixels above 1.0: {:.3}%",
                above_one * 100.0
            );
        }
    }
}
