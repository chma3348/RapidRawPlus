//! Reading Lightroom and Camera Raw edits, for people moving over from Adobe.
//!
//! Lightroom Classic, Lightroom, Camera Raw and Bridge keep a photo's develop
//! settings as `crs:` properties (the Camera Raw Settings namespace) in XMP:
//! in an `IMG_1234.xmp` sidecar beside a RAW, or embedded in a DNG, JPEG or
//! TIFF. This reads them as plain values and point curves; turning them into
//! this app's edits happens in the editor (`src/utils/adobeImport.ts`), where
//! the edit format lives. Nothing here writes to Adobe's files.

use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;
use serde::Serialize;

use crate::xmp::NS_RDF;

pub const NS_CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const NS_TIFF: &str = "http://ns.adobe.com/tiff/1.0/";

/// A photo's size as the editor sees it (full resolution, upright) and the
/// orientation stored in the file, for placing Adobe's crop.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    /// EXIF orientation, 1–8.
    pub file_orientation: u16,
}

/// One photo's Adobe develop settings.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AdobeDevelop {
    /// Simple settings by their `crs:` name: "Exposure2012" → "+0.35".
    pub values: HashMap<String, String>,
    /// Point curves by name ("ToneCurvePV2012", "ToneCurvePV2012Red"…), as
    /// (input, output) pairs on Adobe's 0–255 scale.
    pub curves: HashMap<String, Vec<(f64, f64)>>,
    /// How many local corrections (masks, brushes, gradients) and retouch
    /// spots it has; these don't come across.
    pub local_corrections: usize,
    pub retouch_spots: usize,
    /// "sidecar" or "embedded".
    pub source: String,
    /// `tiff:Orientation` in Adobe's XMP: differs from the file's when the
    /// photo was rotated in Lightroom.
    pub xmp_orientation: Option<u16>,
    pub geometry: Option<Geometry>,
    /// For a RAW whose white balance was changed: the camera's as-shot white
    /// point as CIE xy, so Adobe's Kelvin can become a shift from as shot.
    pub as_shot_xy: Option<[f64; 2]>,
}

/// Lists whose items are counted rather than read.
const LOCAL_LISTS: [&str; 4] = [
    "MaskGroupBasedCorrections",
    "PaintBasedCorrections",
    "GradientBasedCorrections",
    "CircularGradientBasedCorrections",
];

fn parse_curve(items: &[String]) -> Vec<(f64, f64)> {
    items
        .iter()
        .filter_map(|item| {
            let mut parts = item.split(',').map(|p| p.trim().parse::<f64>());
            Some((parts.next()?.ok()?, parts.next()?.ok()?))
        })
        .collect()
}

/// The `crs:` settings in an XMP document, in attribute or element form.
/// `None` when it has none.
pub fn read(content: &str) -> Option<AdobeDevelop> {
    let doc = roxmltree::Document::parse(content).ok()?;
    let mut d = AdobeDevelop::default();
    for desc in doc
        .descendants()
        .filter(|n| n.has_tag_name((NS_RDF, "Description")))
    {
        for a in desc.attributes() {
            if a.namespace() == Some(NS_CRS) {
                d.values
                    .insert(a.name().to_string(), a.value().trim().to_string());
            } else if a.namespace() == Some(NS_TIFF) && a.name() == "Orientation" {
                d.xmp_orientation = a.value().trim().parse().ok();
            }
        }
        if let Some(o) = desc
            .children()
            .find(|n| n.has_tag_name((NS_TIFF, "Orientation")))
            .and_then(|n| n.text())
        {
            d.xmp_orientation = o.trim().parse().ok();
        }
        for child in desc
            .children()
            .filter(|n| n.is_element() && n.tag_name().namespace() == Some(NS_CRS))
        {
            let name = child.tag_name().name();
            // Only the list's own items: local corrections nest lists of their own.
            let items: Vec<String> = child
                .children()
                .filter(|n| n.is_element())
                .flat_map(|container| {
                    container
                        .children()
                        .filter(|n| n.has_tag_name((NS_RDF, "li")))
                })
                .map(|li| li.text().unwrap_or("").trim().to_string())
                .collect();
            if name.starts_with("ToneCurve")
                && !name.ends_with("Name")
                && !name.ends_with("Name2012")
            {
                let curve = parse_curve(&items);
                if !curve.is_empty() {
                    d.curves.insert(name.to_string(), curve);
                }
            } else if LOCAL_LISTS.contains(&name) {
                d.local_corrections += items.len();
            } else if name == "RetouchInfo" || name == "RetouchAreas" {
                d.retouch_spots += items.len();
            } else if let Some(text) = child.text().map(str::trim).filter(|t| !t.is_empty()) {
                d.values.insert(name.to_string(), text.to_string());
            } else if let Some(first) = items.into_iter().next() {
                // An rdf:Alt such as a look's localised name: keep the first.
                d.values.insert(name.to_string(), first);
            }
        }
    }
    (!d.values.is_empty() || !d.curves.is_empty()).then_some(d)
}

/// The first XMP packet inside a file's bytes (JPEG APP1, the TIFF/DNG XMP
/// tag), found by its text: Adobe writes it uncompressed.
fn embedded_packet(bytes: &[u8]) -> Option<&str> {
    let start = memchr::memmem::find(bytes, b"<x:xmpmeta")?;
    let end_tag = b"</x:xmpmeta>";
    let end = start + memchr::memmem::find(&bytes[start..], end_tag)? + end_tag.len();
    std::str::from_utf8(&bytes[start..end]).ok()
}

/// The CIE xy of the camera's as-shot white: the sensor response its
/// white-balance multipliers neutralise, through its D65 matrix, the same
/// calibration the v3 RAW path uses.
fn as_shot_xy(xyz_to_camera: &[f32], wb: &[f32; 4]) -> Option<[f64; 2]> {
    if xyz_to_camera.len() != 9 || wb[..3].iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return None;
    }
    let m =
        glam::DMat3::from_cols_array(&std::array::from_fn(|i| xyz_to_camera[i] as f64)).transpose();
    if m.determinant().abs() < 1e-12 {
        return None;
    }
    let neutral = glam::DVec3::new(1.0 / wb[0] as f64, 1.0 / wb[1] as f64, 1.0 / wb[2] as f64);
    let xyz = m.inverse() * neutral;
    let sum = xyz.x + xyz.y + xyz.z;
    (sum.is_finite() && sum > 0.0).then(|| [xyz.x / sum, xyz.y / sum])
}

fn upright(width: u32, height: u32, orientation: u16) -> (u32, u32) {
    if (5..=8).contains(&orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

/// Size and orientation of a RAW from its metadata alone (no pixels decoded):
/// the default crop within the active sensor area, as the RAW developer
/// crops it, plus the as-shot white point.
fn raw_facts(bytes: &[u8]) -> Option<(Geometry, Option<[f64; 2]>)> {
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::{Dim2, Point, Rect, xyz::Illuminant};
    let source = rawler::rawsource::RawSource::new_from_slice(bytes);
    let decoder = rawler::get_decoder(&source).ok()?;
    let params = RawDecodeParams::default();
    let raw = decoder.raw_image(&source, &params, true).ok()?;
    let orientation = decoder
        .raw_metadata(&source, &params)
        .ok()
        .and_then(|m| m.exif.orientation)
        .unwrap_or(raw.orientation.to_u16());
    let full = Rect::new(Point::zero(), Dim2::new(raw.width, raw.height));
    let active = raw.active_area.unwrap_or(full);
    let size = match raw.crop_area {
        Some(crop) => {
            let c = crop.intersection(&active);
            if c.d.w == 0 || c.d.h == 0 {
                active.d
            } else {
                c.d
            }
        }
        None => active.d,
    };
    let (width, height) = upright(size.w as u32, size.h as u32, orientation);
    let white = raw
        .color_matrix
        .get(&Illuminant::D65)
        .and_then(|m| as_shot_xy(m, &raw.wb_coeffs));
    Some((
        Geometry {
            width,
            height,
            file_orientation: orientation,
        },
        white,
    ))
}

/// Size and orientation of an ordinary image file.
fn image_facts(path: &Path, bytes: &[u8]) -> Option<Geometry> {
    let (w, h) = image::image_dimensions(path).ok()?;
    let orientation = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(bytes))
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1) as u16;
    let (width, height) = upright(w, h, orientation);
    Some(Geometry {
        width,
        height,
        file_orientation: orientation,
    })
}

/// A photo's Adobe settings: from its sidecar if it has one with any, else
/// from XMP embedded in the file. With them, what's needed to place them:
/// the photo's size and orientation, and for a RAW whose white balance was
/// changed, its as-shot white point.
pub fn for_photo(path: &Path) -> Option<AdobeDevelop> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: read-only mapping of a photo that is only read here.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let mut d = match crate::xmp::sidecar_for(path)
        .filter(|(_, exists)| *exists)
        .and_then(|(sidecar, _)| std::fs::read_to_string(sidecar).ok())
        .and_then(|text| read(&text))
    {
        Some(mut d) => {
            d.source = "sidecar".into();
            d
        }
        None => {
            let mut d = read(embedded_packet(&map)?)?;
            d.source = "embedded".into();
            d
        }
    };
    if crate::formats::is_raw_file(path) {
        if let Some((geometry, white)) = raw_facts(&map) {
            d.geometry = Some(geometry);
            let changed = d
                .values
                .get("WhiteBalance")
                .is_some_and(|wb| wb != "As Shot");
            d.as_shot_xy = if changed { white } else { None };
        }
    } else {
        d.geometry = image_facts(path, &map);
    }
    Some(d)
}

/// Adobe settings for each photo that has any, read in parallel. Virtual
/// copies are skipped: their original is the one Adobe edited.
#[tauri::command]
pub async fn read_adobe_develop(
    paths: Vec<String>,
) -> Result<HashMap<String, AdobeDevelop>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        paths
            .par_iter()
            .filter(|p| !p.contains("?vc="))
            .filter_map(|p| for_photo(Path::new(p)).map(|d| (p.clone(), d)))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIDECAR: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:ProcessVersion="15.4"
    crs:WhiteBalance="Custom"
    crs:Temperature="5600"
    crs:Tint="+8"
    crs:Exposure2012="+0.65"
    crs:Highlights2012="-42"
    crs:HasCrop="True"
    crs:CropTop="0.05">
   <crs:ToneCurveName2012>Custom</crs:ToneCurveName2012>
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>64, 54</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
   <crs:MaskGroupBasedCorrections>
    <rdf:Seq>
     <rdf:li><rdf:Description crs:What="Correction"><crs:CorrectionMasks><rdf:Seq><rdf:li/></rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li>
     <rdf:li><rdf:Description crs:What="Correction"/></rdf:li>
    </rdf:Seq>
   </crs:MaskGroupBasedCorrections>
   <crs:Look><rdf:Description crs:Name="Adobe Color"/></crs:Look>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn reads_values_curves_and_counts_local_corrections() {
        let d = read(SIDECAR).unwrap();
        assert_eq!(d.values["Exposure2012"], "+0.65");
        assert_eq!(d.values["Temperature"], "5600");
        assert_eq!(d.values["ToneCurveName2012"], "Custom");
        assert_eq!(
            d.curves["ToneCurvePV2012"],
            vec![(0.0, 0.0), (64.0, 54.0), (255.0, 255.0)]
        );
        assert_eq!(d.local_corrections, 2);
    }

    #[test]
    fn finds_xmp_embedded_in_a_file() {
        let mut bytes = b"\xFF\xD8\xFF\xE1junk http://ns.adobe.com/xap/1.0/\0".to_vec();
        bytes.extend_from_slice(SIDECAR.as_bytes());
        bytes.extend_from_slice(b"\xFF\xDBmore image data");
        let d = read(embedded_packet(&bytes).unwrap()).unwrap();
        assert_eq!(d.values["Highlights2012"], "-42");
    }

    /// The size worked out from metadata matches what the RAW developer
    /// produces. Run with ADOBE_TEST_RAWS=path1:path2 cargo test -- --ignored.
    #[test]
    #[ignore]
    fn raw_size_from_metadata_matches_decode() {
        let Ok(list) = std::env::var("ADOBE_TEST_RAWS") else {
            return;
        };
        for path in list.split(':') {
            let bytes = std::fs::read(path).unwrap();
            let (geometry, white) = raw_facts(&bytes).unwrap();
            let frame = crate::color_engine::raw::decode_raw(&bytes, false, || Ok(())).unwrap();
            println!(
                "{path}: metadata {}x{} (orientation {}), decoded {}x{}, as-shot xy {:?}",
                geometry.width,
                geometry.height,
                geometry.file_orientation,
                frame.pixels.width(),
                frame.pixels.height(),
                white
            );
            assert_eq!(
                (geometry.width, geometry.height),
                (frame.pixels.width(), frame.pixels.height())
            );
        }
    }

    /// An edit made by the importer (src/utils/adobeImport.ts) passes the
    /// engine's own checks and counts as edited. Run with
    /// ADOBE_CONVERTED=path/to/converted.json cargo test -- --ignored.
    #[test]
    #[ignore]
    fn converted_edit_is_valid_for_v3() {
        let Ok(path) = std::env::var("ADOBE_CONVERTED") else {
            return;
        };
        let adj: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let controls: crate::color_engine::controls::Controls =
            serde_json::from_value(adj["v3"].clone()).expect("v3 settings parse");
        controls.validate().expect("v3 settings in range");
        assert!(crate::image_processing::is_image_edited(&adj, true, None));
        crate::color_engine::migration::normalize(&adj).expect("migration accepts it");
    }

    #[test]
    fn no_settings_is_none() {
        let plain = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="3"/></rdf:RDF></x:xmpmeta>"#;
        assert!(read(plain).is_none());
    }
}
