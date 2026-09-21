//! Compositing heal, clone and generative patches into the working space.
//!
//! A patch is pixels somebody else produced — a heal, a clone, a generated
//! fill — stored beside the edit as an 8-bit image and a mask. The previous
//! engine composites them onto whatever the decoded base happened to be,
//! which works there because nothing declares a colour space, so nothing can
//! disagree. This engine does declare one, which is why v3 refused patches
//! until now: nothing said what the stored pixels *were*.
//!
//! What they are depends on where they came from, and the patch records it:
//!
//! - `encoding: "gamma"` marks a patch lifted from float or RAW data, stored
//!   through a 1/2.4 curve so deep shadows survive eight bits. Undo the curve
//!   and the values are linear in the source's own primaries.
//! - Anything else came from rendered, display-referred pixels, and is sRGB.
//!
//! Patches join the picture at the decoded-source stage, the same place the
//! previous engine puts them and before any geometry, so their coordinates
//! line up with the mask stored with them. When an input transform is
//! installed the source has already become scene data, so a display-referred
//! patch goes through that same transform — otherwise it would be the one
//! part of the frame that never had its rendering undone.

use crate::color_engine::config::{Primaries, ReferenceDomain, SourceColor, Transfer};
use crate::color_engine::cube::{CubeLut, apply_input_transform};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, Rgba32FImage, imageops};
use rayon::prelude::*;
use serde_json::Value;

/// The curve generative fills are stored through. Mirrors `LAMA_GAMMA`.
const STORED_GAMMA: f32 = 2.4;

pub fn visible(edits: &Value) -> Vec<&Value> {
    edits["aiPatches"]
        .as_array()
        .map(|patches| {
            patches
                .iter()
                .filter(|p| {
                    p["visible"].as_bool() != Some(false)
                        && p["patchData"]["color"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Composite every visible patch onto `base`, which is in `color`.
pub fn composite(
    base: &mut Rgba32FImage,
    edits: &Value,
    color: &SourceColor,
    input_transform: Option<&CubeLut>,
) -> Result<()> {
    let patches = visible(edits);
    if patches.is_empty() {
        return Ok(());
    }
    let (width, height) = base.dimensions();
    for patch in patches {
        let data = &patch["patchData"];
        let feather = patch["feather"].as_f64().unwrap_or(0.0) as f32;
        let opacity = (patch["opacity"].as_f64().unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0);

        let mut colour = decode_layer(
            data["color"].as_str().context("Patch has no colour data")?,
            width,
            height,
        )?;
        let stored_gamma = data["encoding"].as_str() == Some("gamma");
        to_source_space(&mut colour, color, stored_gamma, input_transform);

        let mut mask = mask_for(patch, data, width, height)?;
        if feather > 0.0 {
            mask = crate::ai_processing::feather_mask_inward(&mask, feather);
        }

        base.par_chunks_mut((width * 4) as usize)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..width as usize {
                    let coverage = mask.get_pixel(x as u32, y as u32)[0];
                    if coverage == 0 {
                        continue;
                    }
                    let alpha = coverage as f32 / 255.0 * opacity;
                    let patched = colour.get_pixel(x as u32, y as u32);
                    for c in 0..3 {
                        row[x * 4 + c] = patched[c] * alpha + row[x * 4 + c] * (1.0 - alpha);
                    }
                }
            });
    }
    Ok(())
}

fn decode_layer(encoded: &str, width: u32, height: u32) -> Result<image::Rgb32FImage> {
    let bytes = general_purpose::STANDARD.decode(encoded)?;
    let image = image::load_from_memory(&bytes)?.to_rgb8();
    let image = if image.dimensions() == (width, height) {
        image
    } else {
        imageops::resize(&image, width, height, imageops::FilterType::Lanczos3)
    };
    Ok(DynamicImage::ImageRgb8(image).to_rgb32f())
}

fn mask_for(patch: &Value, data: &Value, width: u32, height: u32) -> Result<image::GrayImage> {
    if let Some(encoded) = data["mask"].as_str().filter(|s| !s.is_empty()) {
        let bytes = general_purpose::STANDARD.decode(encoded)?;
        let mask = image::load_from_memory(&bytes)?.to_luma8();
        return Ok(if mask.dimensions() == (width, height) {
            mask
        } else {
            imageops::resize(&mask, width, height, imageops::FilterType::Lanczos3)
        });
    }
    // Older patches carry their shapes instead of a rendered mask.
    let info: crate::image_loader::PatchMaskInfo =
        serde_json::from_value(patch.clone()).context("Patch has neither a mask nor shapes")?;
    let definition = crate::mask_generation::MaskDefinition {
        id: info.id,
        name: info.name,
        visible: true,
        invert: info.invert,
        opacity: 100.0,
        grow: 0.0,
        feather: 0.0,
        adjustments: Value::Null,
        sub_masks: info.sub_masks,
    };
    crate::mask_generation::generate_mask_bitmap(&definition, width, height, 1.0, (0.0, 0.0), None)
        .context("Could not build a patch mask from its shapes")
}

/// Bring stored patch pixels into the space the base image is in.
fn to_source_space(
    colour: &mut image::Rgb32FImage,
    color: &SourceColor,
    stored_gamma: bool,
    input_transform: Option<&CubeLut>,
) {
    if stored_gamma {
        // Linear already, once the storage curve is undone, in the primaries
        // the source was decoded to.
        for pixel in colour.pixels_mut() {
            for c in 0..3 {
                pixel[c] = pixel[c].clamp(0.0, 1.0).powf(STORED_GAMMA);
            }
        }
        return;
    }
    match (color.reference, input_transform) {
        // The source has been through the input transform, so the patch must
        // be too, or it keeps a rendering the rest of the frame has had
        // removed.
        (ReferenceDomain::Scene, Some(cube)) => {
            let mut rgba = image::ImageBuffer::from_fn(colour.width(), colour.height(), |x, y| {
                let p = colour.get_pixel(x, y);
                // `apply_input_transform` re-encodes what it is given, so hand
                // it linear values.
                image::Rgba([srgb_decode(p[0]), srgb_decode(p[1]), srgb_decode(p[2]), 1.0])
            });
            apply_input_transform(cube, &mut rgba);
            for (out, src) in colour.pixels_mut().zip(rgba.pixels()) {
                for c in 0..3 {
                    out[c] = src[c];
                }
            }
        }
        _ => {
            for pixel in colour.pixels_mut() {
                for c in 0..3 {
                    pixel[c] = srgb_decode(pixel[c]);
                }
            }
            if color.primaries != Primaries::Srgb {
                let matrix =
                    crate::color_engine::spaces::conversion(Primaries::Srgb, color.primaries);
                for pixel in colour.pixels_mut() {
                    let v = glam::DVec3::new(pixel[0] as f64, pixel[1] as f64, pixel[2] as f64);
                    let converted = matrix * v;
                    pixel[0] = converted.x as f32;
                    pixel[1] = converted.y as f32;
                    pixel[2] = converted.z as f32;
                }
            }
            debug_assert_eq!(
                color.transfer,
                Transfer::Linear,
                "the input adapter is expected to hand back linear pixels"
            );
        }
    }
}

fn srgb_decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_json(colour: image::RgbImage, mask: image::GrayImage, encoding: &str) -> Value {
        let encode = |image: DynamicImage| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
            general_purpose::STANDARD.encode(bytes.into_inner())
        };
        serde_json::json!({"aiPatches": [{
            "visible": true,
            "opacity": 100.0,
            "feather": 0.0,
            "patchData": {
                "color": encode(DynamicImage::ImageRgb8(colour)),
                "mask": encode(DynamicImage::ImageLuma8(mask)),
                "encoding": encoding,
            }
        }]})
    }

    fn display_source() -> SourceColor {
        SourceColor {
            primaries: Primaries::Srgb,
            transfer: Transfer::Linear,
            reference: ReferenceDomain::Display,
        }
    }

    /// A patch stored as display pixels must arrive as the same colour the
    /// source would have been decoded to — not as its raw stored numbers.
    #[test]
    fn a_display_patch_is_decoded_into_the_working_space() {
        let colour = image::ImageBuffer::from_pixel(4, 4, image::Rgb([128u8, 64, 200]));
        let mask = image::ImageBuffer::from_pixel(4, 4, image::Luma([255u8]));
        let mut base = image::ImageBuffer::from_pixel(4, 4, image::Rgba([0.0f32, 0.0, 0.0, 1.0]));
        composite(
            &mut base,
            &patch_json(colour, mask, "srgb"),
            &display_source(),
            None,
        )
        .unwrap();
        let got = base.get_pixel(0, 0);
        for (c, stored) in [128.0f32, 64.0, 200.0].iter().enumerate() {
            let want = srgb_decode(stored / 255.0);
            assert!(
                (got[c] - want).abs() < 1e-4,
                "channel {c}: {got:?} wanted {want}"
            );
        }
    }

    #[test]
    fn a_gamma_patch_undoes_its_storage_curve() {
        let colour = image::ImageBuffer::from_pixel(2, 2, image::Rgb([128u8, 128, 128]));
        let mask = image::ImageBuffer::from_pixel(2, 2, image::Luma([255u8]));
        let mut base = image::ImageBuffer::from_pixel(2, 2, image::Rgba([0.0f32, 0.0, 0.0, 1.0]));
        composite(
            &mut base,
            &patch_json(colour, mask, "gamma"),
            &display_source(),
            None,
        )
        .unwrap();
        let want = (128.0f32 / 255.0).powf(STORED_GAMMA);
        assert!((base.get_pixel(0, 0)[0] - want).abs() < 1e-4);
    }

    /// The mask is coverage, and zero coverage must leave the base exactly.
    #[test]
    fn the_mask_decides_where_a_patch_lands() {
        let colour = image::ImageBuffer::from_pixel(4, 1, image::Rgb([255u8, 255, 255]));
        let mask =
            image::ImageBuffer::from_fn(4, 1, |x, _| image::Luma([if x < 2 { 255u8 } else { 0 }]));
        let mut base =
            image::ImageBuffer::from_pixel(4, 1, image::Rgba([0.25f32, 0.25, 0.25, 1.0]));
        composite(
            &mut base,
            &patch_json(colour, mask, "srgb"),
            &display_source(),
            None,
        )
        .unwrap();
        assert!(
            base.get_pixel(0, 0)[0] > 0.9,
            "covered pixel was not patched"
        );
        assert_eq!(
            base.get_pixel(3, 0)[0],
            0.25,
            "uncovered pixel must be untouched"
        );
    }

    #[test]
    fn a_hidden_patch_does_nothing() {
        let mut edits = patch_json(
            image::ImageBuffer::from_pixel(2, 2, image::Rgb([255u8, 0, 0])),
            image::ImageBuffer::from_pixel(2, 2, image::Luma([255u8])),
            "srgb",
        );
        edits["aiPatches"][0]["visible"] = Value::Bool(false);
        let mut base = image::ImageBuffer::from_pixel(2, 2, image::Rgba([0.1f32, 0.1, 0.1, 1.0]));
        let before = base.clone();
        composite(&mut base, &edits, &display_source(), None).unwrap();
        assert_eq!(base, before);
    }
}
