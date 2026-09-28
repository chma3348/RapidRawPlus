//! Strict, profile-aware reference intake. No fitting or slider changes.
use super::{
    application,
    config::{Primaries, Transfer},
    identity, input, spaces,
};
use anyhow::{Context, Result, ensure};
use image::{ColorType, ImageReader, Rgba32FImage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    io::Cursor,
    path::{Path, PathBuf},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub path: PathBuf,
    pub blake3: String,
}
impl File {
    pub fn read(&self, root: &Path) -> Result<Vec<u8>> {
        let bytes = std::fs::read(root.join(&self.path))
            .with_context(|| format!("Reading {}", self.path.display()))?;
        ensure!(
            blake3::hash(&bytes).to_hex().as_str() == self.blake3,
            "Content changed or wrong hash: {}",
            self.path.display()
        );
        Ok(bytes)
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveSettings {
    pub version: String,
    pub page_and_tool: String,
    pub input_color_space: String,
    pub timeline_color_space: String,
    pub output_color_space: String,
    pub data_levels: String,
    pub export_bit_depth: u8,
    pub notes: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub file: File,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub source: String,
    pub reference: File,
    /// Explicit permission for an untagged export to be interpreted as sRGB.
    pub untagged_srgb: bool,
    pub control: String,
    pub resolve_value: f64,
    pub edits: Value,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub schema: u32,
    /// Display-rendered photographs only. Intermediate lattice captures use
    /// resolve_fit.py and must never be compared with this display adapter.
    pub domain: String,
    pub resolve: ResolveSettings,
    pub pipeline: identity::Identity,
    pub asset_directory: PathBuf,
    pub sources: Vec<Source>,
    pub cases: Vec<Case>,
    /// Declare planned strengths to reject incomplete calibration batches.
    #[serde(default)]
    pub required_samples: std::collections::BTreeMap<String, Vec<f64>>,
}

pub fn reference_pixels(case: &Case, root: &Path) -> Result<Rgba32FImage> {
    let bytes = case.reference.read(root)?;
    let mut reader = ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?;
    reader.no_limits();
    let image = reader.decode()?;
    ensure!(
        matches!(image.color(), ColorType::Rgb16 | ColorType::Rgba16),
        "Reference {} must be 16-bit RGB/RGBA, not 8-bit, grayscale or float",
        case.id
    );
    let frame = input::decode_profiled_photo(&bytes)?;
    ensure!(
        frame.provenance.profile_hash.is_some() || case.untagged_srgb,
        "Reference {} is untagged: supply its ICC profile or explicitly attest sRGB",
        case.id
    );
    ensure!(
        frame.pixels.pixels().all(|p| p[3] == 1.0),
        "Reference {} contains transparency; export against the same opaque background",
        case.id
    );
    Ok(frame.pixels)
}

impl Package {
    pub fn edits(&self, case: &Case) -> Value {
        let mut edits = case.edits.clone();
        edits["v3Pipeline"] = serde_json::to_value(&self.pipeline).unwrap();
        edits
    }
    pub fn state(&self, root: &Path) -> crate::AppState {
        let state = crate::AppState::default();
        *state.v3_asset_dir.lock().unwrap() = Some(root.join(&self.asset_directory));
        state
    }
    pub fn validate(&self, root: &Path) -> Result<Value> {
        ensure!(
            self.schema == 1 && self.domain == "display_rgb",
            "Unsupported reference schema/domain; Intermediate captures need their dedicated tool"
        );
        ensure!(
            self.resolve.data_levels == "full" && self.resolve.export_bit_depth == 16,
            "Reference package requires full-range 16-bit display exports"
        );
        for text in [
            &self.resolve.version,
            &self.resolve.page_and_tool,
            &self.resolve.input_color_space,
            &self.resolve.timeline_color_space,
            &self.resolve.output_color_space,
        ] {
            ensure!(
                !text.trim().is_empty() && !text.contains("REPLACE"),
                "Record the actual Resolve settings before intake"
            );
        }
        ensure!(
            !self.sources.is_empty() && !self.cases.is_empty(),
            "Empty reference package"
        );
        let state = self.state(root);
        identity::resolve(&state, &serde_json::json!({"v3Pipeline":self.pipeline}))?;
        let mut ids = HashSet::new();
        for source in &self.sources {
            ensure!(
                !source.id.is_empty() && ids.insert(&source.id),
                "Duplicate/empty source ID"
            );
            source.file.read(root)?;
            for (control, values) in &self.required_samples {
                ensure!(
                    !values.is_empty() && values.iter().all(|v| v.is_finite()),
                    "Invalid required strengths"
                );
                for value in values {
                    ensure!(
                        self.cases.iter().any(|c| c.source == source.id
                            && c.control == *control
                            && c.resolve_value == *value),
                        "Source {} is missing {} at {}",
                        source.id,
                        control,
                        value
                    );
                }
            }
            ensure!(
                self.cases
                    .iter()
                    .any(|c| c.source == source.id && c.control == "neutral"),
                "Source {} needs a neutral Resolve reference",
                source.id
            );
        }
        let mut cases = HashSet::new();
        let mut settings = HashSet::new();
        let mut reports = vec![];
        for case in &self.cases {
            ensure!(
                !case.untagged_srgb || self.resolve.output_color_space.eq_ignore_ascii_case("srgb"),
                "An untagged sRGB assumption requires the recorded output color space to be sRGB"
            );
            ensure!(
                !case.id.is_empty() && cases.insert(&case.id),
                "Duplicate/empty case ID"
            );
            ensure!(
                !case.control.is_empty() && case.resolve_value.is_finite(),
                "Invalid control setting"
            );
            ensure!(
                settings.insert((
                    case.source.clone(),
                    case.control.clone(),
                    case.resolve_value.to_string()
                )),
                "Duplicate control strength for source"
            );
            let source = self
                .sources
                .iter()
                .find(|s| s.id == case.source)
                .context("Unknown case source")?;
            ensure!(
                case.edits.is_object() && case.edits["processVersion"].as_u64() == Some(3),
                "Case {} needs exact processVersion 3 application edits",
                case.id
            );
            ensure!(
                case.edits.get("v3Pipeline").is_none(),
                "Use the package pipeline, not per-case overrides"
            );
            ensure!(
                case.edits["lutPath"].is_null() && case.edits["flatFieldProfile"].is_null(),
                "Calibration cases must not have external creative LUTs or flat fields"
            );
            ensure!(
                case.edits
                    .get("masks")
                    .is_none_or(|v| v.as_array().is_some_and(|a| a.is_empty())),
                "Calibrate global controls without masks first"
            );
            let controls = application::controls(&case.edits)?;
            controls.validate()?;
            if case.control == "neutral" {
                ensure!(
                    controls.is_neutral()
                        && controls.detail.is_neutral()
                        && super::optics::is_neutral(&controls.effects),
                    "Neutral reference has non-neutral app controls"
                );
            }
            let edits = self.edits(case);
            let path = root.join(&source.file.path);
            let input = application::input_report(
                &state,
                path.to_str().context("Non-UTF8 source path")?,
                &edits,
            )?;
            let reference = reference_pixels(case, root)?;
            ensure!(
                Some(reference.width() as u64) == input["width"].as_u64()
                    && Some(reference.height() as u64) == input["height"].as_u64(),
                "Case {} dimensions differ: export native size without crop/resize",
                case.id
            );
            reports.push(serde_json::json!({"case":case.id,"input":input,"reference_hash":case.reference.blake3}));
        }
        Ok(
            serde_json::json!({"schema":1,"status":"validated_not_calibrated","cases":reports,
            "resolve":self.resolve,"stage_revision":super::contract::REVISION}),
        )
    }
}

#[derive(Debug, Serialize)]
pub struct Difference {
    pub mean_linear_rgb: f64,
    pub p99_linear_rgb: f64,
    pub max_linear_rgb: f64,
    pub mean_oklab_distance: f64,
    pub channel_bias_linear: [f64; 3],
}
/// Both inputs are linear sRGB coordinates (not necessarily sRGB gamut).
pub fn difference(a: &Rgba32FImage, b: &Rgba32FImage) -> Result<Difference> {
    ensure!(
        a.dimensions() == b.dimensions() && !a.is_empty(),
        "Comparison dimensions differ or are empty"
    );
    let mut errors = Vec::with_capacity(a.as_raw().len());
    let mut bias = [0.; 3];
    let mut lab = 0.;
    for (a, b) in a.pixels().zip(b.pixels()) {
        ensure!(
            a.0.iter().chain(b.0.iter()).all(|v| v.is_finite()),
            "Non-finite comparison pixel"
        );
        for c in 0..3 {
            let d = a[c] as f64 - b[c] as f64;
            errors.push(d.abs());
            bias[c] += d;
        }
        let lab_of = |p: &image::Rgba<f32>| {
            spaces::oklab_from_rgb(
                Primaries::Srgb,
                glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64),
            )
        };
        lab += (lab_of(a) - lab_of(b)).length();
    }
    errors.sort_unstable_by(f64::total_cmp);
    let n = a.pixels().len() as f64;
    Ok(Difference {
        mean_linear_rgb: errors.iter().sum::<f64>() / errors.len() as f64,
        p99_linear_rgb: errors[((errors.len() - 1) as f64 * 0.99).ceil() as usize],
        max_linear_rgb: *errors.last().unwrap(),
        mean_oklab_distance: lab / n,
        channel_bias_linear: bias.map(|v| v / n),
    })
}
pub fn decode_output(image: &Rgba32FImage) -> Rgba32FImage {
    let mut linear = image.clone();
    for p in linear.pixels_mut() {
        for c in 0..3 {
            p[c] = spaces::decode(p[c] as f64, Transfer::Srgb) as f32;
        }
    }
    linear
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_requires_neutral_and_explicit_consistent_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        image::ImageBuffer::from_pixel(8, 8, image::Rgba([32000u16, 32000, 32000, 65535]))
            .save(&path)
            .unwrap();
        let file = File {
            path,
            blake3: blake3::hash(&std::fs::read(dir.path().join("photo.png")).unwrap())
                .to_hex()
                .to_string(),
        };
        let mut package = Package {
            required_samples: Default::default(),
            schema: 1,
            domain: "display_rgb".into(),
            resolve: ResolveSettings {
                version: "test".into(),
                page_and_tool: "test".into(),
                input_color_space: "sRGB".into(),
                timeline_color_space: "DWG Intermediate".into(),
                output_color_space: "sRGB".into(),
                data_levels: "full".into(),
                export_bit_depth: 16,
                notes: "synthetic intake fixture, not a Resolve capture".into(),
            },
            pipeline: identity::pin(&crate::AppState::default(), &serde_json::json!({})).unwrap(),
            asset_directory: "assets".into(),
            sources: vec![Source {
                id: "source".into(),
                file: file.clone(),
            }],
            cases: vec![Case {
                id: "neutral".into(),
                source: "source".into(),
                reference: file,
                untagged_srgb: true,
                control: "neutral".into(),
                resolve_value: 0.,
                edits: serde_json::json!({"processVersion":3}),
            }],
        };
        assert!(package.validate(dir.path()).is_ok());
        package.cases[0].control = "shadows".into();
        assert!(package.validate(dir.path()).is_err());
        package.cases[0].control = "neutral".into();
        package.cases[0].edits["exposure"] = serde_json::json!(1.);
        assert!(package.validate(dir.path()).is_err());
        package.cases[0].edits["exposure"] = serde_json::json!(0.);
        package.domain = "davinci_intermediate".into();
        assert!(package.validate(dir.path()).is_err());
    }
    #[test]
    fn measurement_is_precise_and_rejects_bad_inputs() {
        let a = Rgba32FImage::from_pixel(3, 2, image::Rgba([0.2, 0.3, 0.4, 1.]));
        assert_eq!(difference(&a, &a).unwrap().max_linear_rgb, 0.);
        let mut b = a.clone();
        b.get_pixel_mut(0, 0)[0] += 0.0001;
        assert!(difference(&a, &b).unwrap().max_linear_rgb > 0.00009);
        b.get_pixel_mut(0, 0)[0] = f32::NAN;
        assert!(difference(&a, &b).is_err());
        assert!(difference(&a, &Rgba32FImage::new(2, 2)).is_err());
    }
    #[test]
    fn intake_rejects_changed_files_eight_bit_and_unattested_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ref.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([128, 128, 128, 255]))
            .save(&path)
            .unwrap();
        let file = || File {
            path: path.clone(),
            blake3: blake3::hash(&std::fs::read(&path).unwrap())
                .to_hex()
                .to_string(),
        };
        let mut case = Case {
            id: "test".into(),
            source: "source".into(),
            reference: file(),
            untagged_srgb: true,
            control: "neutral".into(),
            resolve_value: 0.,
            edits: serde_json::json!({"processVersion":3}),
        };
        assert!(reference_pixels(&case, dir.path()).is_err());
        image::ImageBuffer::from_pixel(8, 8, image::Rgba([32000u16, 32000, 32000, 65535]))
            .save(&path)
            .unwrap();
        assert!(case.reference.read(dir.path()).is_err());
        case.reference = file();
        case.untagged_srgb = false;
        assert!(reference_pixels(&case, dir.path()).is_err());
        case.untagged_srgb = true;
        assert!(reference_pixels(&case, dir.path()).is_ok());
    }
}
