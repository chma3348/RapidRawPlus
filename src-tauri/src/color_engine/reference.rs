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
/// How a source reaches the engine, which decides whether its references can
/// fit a slider today. Sliders are the same code for every class, so a fit
/// on the classes that already agree at neutral carries to the others; the
/// classes held out have a baseline difference of their own to settle first.
pub mod class {
    /// sRGB JPEG or untagged photo: neutral agrees with Resolve to a level.
    pub const SRGB_JPEG: &str = "srgb_jpeg";
    /// The synthetic charts: flat patches, no resampling error.
    pub const CHART: &str = "chart";
    /// Display P3 photo. Resolve read it as sRGB in this batch and we honour
    /// the profile, so its references describe a different picture.
    pub const IPHONE_P3: &str = "iphone_p3";
    /// RAW: Resolve's Camera RAW development is not ours yet (0.3-4.6 stops
    /// apart per file); a slider fit would absorb that decode difference.
    pub const RAW: &str = "raw";
}

fn yes() -> bool {
    true
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub file: File,
    /// One of `class::*`; empty in packages made before classes existed.
    #[serde(default)]
    pub class: String,
    /// Whether this source's references take part in slider fitting. Every
    /// source is still measured; this only decides what a fit sees.
    #[serde(default = "yes")]
    pub fit: bool,
    /// Why a source is held out, for the person reading the package.
    #[serde(default)]
    pub note: String,
}

/// The Resolve exports in a folder named after their control (`No edits`,
/// `Hilights`, `Shadows`, `Saturation`, `Contrast`, `Pivot`, `Temp`, `Tint`,
/// `Hue`, `Exposure`), each named `<source> (<control> <value>)` or
/// `<source> <value>`, spelling and brackets as typed: the value is the last
/// signed number in the name and the source is what comes before the
/// bracket or before that number. Returns (source stem, control, Resolve
/// value as read off the slider).
pub fn parse_export_name(folder: &str, stem: &str) -> Option<(String, String, f64)> {
    let folder = folder.to_ascii_lowercase();
    let control = if folder.contains("no edit") || folder.contains("neutral") {
        "neutral"
    } else if folder.contains("light") {
        "highlights"
    } else if folder.contains("shad") {
        "shadows"
    } else if folder.contains("satur") {
        "saturation"
    } else if folder.contains("pivot") {
        "pivot"
    } else if folder.contains("contrast") {
        "contrast"
    } else if folder.contains("temp") {
        "temperature"
    } else if folder.contains("tint") {
        "tint"
    } else if folder.contains("hue") {
        "hue"
    } else if folder.contains("expos") {
        "exposure"
    } else {
        return None;
    };
    let stem = stem.trim();
    if control == "neutral" {
        let source = stem
            .strip_suffix(" plain")
            .or_else(|| stem.strip_suffix(" neutral"))
            .or_else(|| stem.strip_suffix(" (plain)"))
            .unwrap_or(stem);
        return Some((source.trim().to_string(), control.into(), 0.));
    }
    let (source, tail) = match stem.find(" (") {
        Some(i) => (&stem[..i], &stem[i..]),
        None => {
            let i = stem.rfind(' ')?;
            (&stem[..i], &stem[i..])
        }
    };
    // The last signed number in the tail, in case the control's spelling
    // carried a digit.
    let value = tail
        .split(|c: char| !(c.is_ascii_digit() || c == '-' || c == '+' || c == '.'))
        .rfind(|s| s.chars().any(|c| c.is_ascii_digit()))?
        .parse::<f64>()
        .ok()?;
    Some((source.trim().to_string(), control.into(), value))
}

/// The app's value that stands for a Resolve reading before any fitting: the
/// same number where the scales look alike, a unit change where Resolve's
/// scale is plainly different (Contrast 0..2 about 1, Pivot 0..1, Temp in
/// hundreds). Equal numbers do not imply equal response; the sweep finds
/// the value that actually matches.
pub fn app_value(control: &str, resolve_value: f64) -> f64 {
    match control {
        "contrast" => (resolve_value - 1.) * 100.,
        "pivot" => resolve_value * 100.,
        "temperature" => resolve_value / 10.,
        _ => resolve_value,
    }
}

/// The app's range for a control, the domain a sweep searches.
pub fn app_range(control: &str) -> (f64, f64) {
    match control {
        "pivot" => (0., 100.),
        "hue" => (-180., 180.),
        "exposure" => (-5., 5.),
        _ => (-100., 100.),
    }
}

/// The application edits that set one control to an app value and hold
/// everything else constant. Shared Basic controls live at the top level in
/// the previous engine's units; Saturation, Temp, Tint and Hue are v3's own.
/// Pivot is exported with Contrast 1.5 in Resolve, so it carries the app's
/// starting guess for that contrast until Contrast itself is fitted.
pub fn edits_for(control: &str, value: f64) -> Result<Value> {
    let mut edits = serde_json::json!({
        "processVersion": 3, "toneMapper": "resolve",
        "v3RawRecovery": "neutral_green_v1", "v3": {}
    });
    match control {
        "neutral" => {}
        "highlights" | "shadows" | "whites" | "blacks" | "contrast" | "exposure" => {
            edits[control] = serde_json::json!(value);
        }
        "pivot" => {
            edits["contrast"] = serde_json::json!(app_value("contrast", 1.5));
            edits["contrastPivot"] = serde_json::json!(value);
        }
        "saturation" | "temperature" | "tint" | "vibrance" | "hue" => {
            edits["v3"][control] = serde_json::json!(value);
        }
        other => anyhow::bail!("No application edit is defined for control {other}"),
    }
    Ok(edits)
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
            // Resolve's photo export scales to a chosen long edge (2160 here),
            // so references may be smaller than the source; the aspect must
            // still match, or the export was cropped or rotated differently.
            let (rw, rh) = (reference.width() as f64, reference.height() as f64);
            let (sw, sh) = (
                input["width"].as_f64().unwrap_or(0.),
                input["height"].as_f64().unwrap_or(0.),
            );
            ensure!(
                rw <= sw && rh <= sh && ((rw / rh) - (sw / sh)).abs() < 0.01,
                "Case {} was exported at a different shape ({}x{} vs source {}x{}): export without crop or rotation",
                case.id,
                rw,
                rh,
                sw,
                sh
            );
            reports.push(serde_json::json!({"case":case.id,"input":input,"reference_hash":case.reference.blake3}));
        }
        Ok(
            serde_json::json!({"schema":1,"status":"validated_not_calibrated","cases":reports,
            "resolve":self.resolve,"stage_revision":super::contract::REVISION}),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Difference {
    pub mean_linear_rgb: f64,
    pub p99_linear_rgb: f64,
    pub max_linear_rgb: f64,
    pub mean_oklab_distance: f64,
    pub channel_bias_linear: [f64; 3],
    /// The same comparison in encoded sRGB, in 8-bit levels: what a
    /// difference looks like on screen. Mean and 99th percentile of the
    /// per-channel absolute difference, and the signed mean per channel.
    pub mean_levels: f64,
    pub p99_levels: f64,
    pub bias_levels: [f64; 3],
}

/// How far apart two neutral developments of the same RAW are, in stops,
/// by the reference's own brightness: a decode difference shows here as a
/// uniform offset, while an exposure-dependent one (a highlight rolloff, a
/// shadow lift) shows as different numbers per band. Positive means the
/// reference is brighter. Inputs are linear sRGB.
#[derive(Debug, Clone, Serialize)]
pub struct ExposureOffset {
    pub shadows_stops: f64,
    pub midtones_stops: f64,
    pub highlights_stops: f64,
}
pub fn exposure_offset(ours: &Rgba32FImage, theirs: &Rgba32FImage) -> Result<ExposureOffset> {
    ensure!(
        ours.dimensions() == theirs.dimensions(),
        "Offset dimensions differ"
    );
    let luma =
        |p: &image::Rgba<f32>| 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
    let mut sums = [[0f64; 2]; 3];
    for (a, b) in ours.pixels().zip(theirs.pixels()) {
        let (ya, yb) = (luma(a), luma(b));
        let band = match yb {
            y if (0.004..0.04).contains(&y) => 0,
            y if (0.04..0.25).contains(&y) => 1,
            y if (0.25..0.85).contains(&y) => 2,
            _ => continue,
        };
        sums[band][0] += ya;
        sums[band][1] += yb;
    }
    let stops = |s: [f64; 2]| {
        if s[0] > 0. && s[1] > 0. {
            (s[1] / s[0]).log2()
        } else {
            f64::NAN
        }
    };
    Ok(ExposureOffset {
        shadows_stops: stops(sums[0]),
        midtones_stops: stops(sums[1]),
        highlights_stops: stops(sums[2]),
    })
}
/// Bring a full-resolution render to a smaller reference's size by area
/// averaging, the way an export scaler does. Resampling differences show at
/// edges, so per-pixel maxima are less telling on scaled references than the
/// mean and the 99th percentile; the chart's flat patches are unaffected.
pub fn match_size(render: &Rgba32FImage, reference: &Rgba32FImage) -> Rgba32FImage {
    if render.dimensions() == reference.dimensions() {
        return render.clone();
    }
    area_resample(render, reference.width(), reference.height())
}

/// Exact box-filter resampling to a given size: every destination pixel is
/// the area-weighted mean of the source it covers, whatever the ratio, so a
/// render lands on the reference's own dimensions rather than one pixel off
/// when the aspect ratios differ in the third decimal.
pub fn area_resample(src: &Rgba32FImage, width: u32, height: u32) -> Rgba32FImage {
    let (sw, sh) = src.dimensions();
    let weights = |n_src: u32, n_dst: u32| -> Vec<Vec<(u32, f32)>> {
        let scale = n_src as f64 / n_dst as f64;
        (0..n_dst)
            .map(|i| {
                let (a, b) = (i as f64 * scale, (i as f64 + 1.) * scale);
                let mut taps = Vec::new();
                let mut x = a.floor() as u32;
                while (x as f64) < b && x < n_src {
                    let (lo, hi) = (a.max(x as f64), b.min(x as f64 + 1.));
                    if hi > lo {
                        taps.push((x, ((hi - lo) / scale) as f32));
                    }
                    x += 1;
                }
                taps
            })
            .collect()
    };
    let (wx, wy) = (weights(sw, width), weights(sh, height));
    let mut rows = vec![0f32; (width * sh * 4) as usize];
    for y in 0..sh {
        for (i, taps) in wx.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(x, w) in taps {
                let p = src.get_pixel(x, y);
                for c in 0..4 {
                    acc[c] += p[c] * w;
                }
            }
            let o = (y as usize * width as usize + i) * 4;
            rows[o..o + 4].copy_from_slice(&acc);
        }
    }
    let mut out = Rgba32FImage::new(width, height);
    for (j, taps) in wy.iter().enumerate() {
        for x in 0..width {
            let mut acc = [0f32; 4];
            for &(y, w) in taps {
                let o = (y as usize * width as usize + x as usize) * 4;
                for c in 0..4 {
                    acc[c] += rows[o + c] * w;
                }
            }
            out.put_pixel(x, j as u32, image::Rgba(acc));
        }
    }
    out
}

/// Both inputs are linear sRGB coordinates (not necessarily sRGB gamut).
pub fn difference(a: &Rgba32FImage, b: &Rgba32FImage) -> Result<Difference> {
    ensure!(
        a.dimensions() == b.dimensions() && !a.is_empty(),
        "Comparison dimensions differ or are empty"
    );
    let mut errors = Vec::with_capacity(a.as_raw().len());
    let mut levels = Vec::with_capacity(a.as_raw().len());
    let mut bias = [0.; 3];
    let mut level_bias = [0.; 3];
    let mut lab = 0.;
    let encode = |v: f64| {
        let v = v.clamp(0., 1.);
        255. * if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    };
    for (a, b) in a.pixels().zip(b.pixels()) {
        ensure!(
            a.0.iter().chain(b.0.iter()).all(|v| v.is_finite()),
            "Non-finite comparison pixel"
        );
        for c in 0..3 {
            let d = a[c] as f64 - b[c] as f64;
            errors.push(d.abs());
            bias[c] += d;
            let l = encode(a[c] as f64) - encode(b[c] as f64);
            levels.push(l.abs());
            level_bias[c] += l;
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
    levels.sort_unstable_by(f64::total_cmp);
    let n = a.pixels().len() as f64;
    let p99 = ((errors.len() - 1) as f64 * 0.99).ceil() as usize;
    Ok(Difference {
        mean_linear_rgb: errors.iter().sum::<f64>() / errors.len() as f64,
        p99_linear_rgb: errors[p99],
        max_linear_rgb: *errors.last().unwrap(),
        mean_oklab_distance: lab / n,
        channel_bias_linear: bias.map(|v| v / n),
        mean_levels: levels.iter().sum::<f64>() / levels.len() as f64,
        p99_levels: levels[p99],
        bias_levels: level_bias.map(|v| v / n),
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
                class: class::CHART.into(),
                fit: true,
                note: String::new(),
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
    fn export_names_are_read_as_typed() {
        let parse = |folder, stem| parse_export_name(folder, stem).unwrap();
        assert_eq!(
            parse("Hilights", "DSC03453 (hilights -100)"),
            ("DSC03453".into(), "highlights".into(), -100.)
        );
        assert_eq!(
            parse("Hilights", "DSC08030 (hilights -100"),
            ("DSC08030".into(), "highlights".into(), -100.)
        );
        assert_eq!(
            parse("Hilights", "DSC08197 (hilighst -50)"),
            ("DSC08197".into(), "highlights".into(), -50.)
        );
        assert_eq!(
            parse("Hilights", "chart-all (higlights -100)"),
            ("chart-all".into(), "highlights".into(), -100.)
        );
        assert_eq!(
            parse(
                "Shadows",
                "8693D82C-6F6D-41AD-BC7A-D89C8DDF644D_1_105_c (showdows 50)"
            ),
            (
                "8693D82C-6F6D-41AD-BC7A-D89C8DDF644D_1_105_c".into(),
                "shadows".into(),
                50.
            )
        );
        assert_eq!(
            parse(
                "Saturation",
                "0423A308-94B7-48D6-BDD9-93303D1EBE3D_1_105_c -100"
            ),
            (
                "0423A308-94B7-48D6-BDD9-93303D1EBE3D_1_105_c".into(),
                "saturation".into(),
                -100.
            )
        );
        assert_eq!(
            parse("Saturation", "chart-all 50"),
            ("chart-all".into(), "saturation".into(), 50.)
        );
        assert_eq!(
            parse("No edits", "_AAF8641 plain"),
            ("_AAF8641".into(), "neutral".into(), 0.)
        );
        assert!(parse_export_name("Originals", "DSC03453").is_none());
        assert!(parse_export_name("Shadows", "DSC03453").is_none());
        assert_eq!(
            parse("Contrast", "chart-all (contrast 1.5)"),
            ("chart-all".into(), "contrast".into(), 1.5)
        );
        assert_eq!(
            parse("Temp", "chart-all (temp -1000)"),
            ("chart-all".into(), "temperature".into(), -1000.)
        );
        assert_eq!(
            parse("Pivot", "chart-all (pivot 0.3)"),
            ("chart-all".into(), "pivot".into(), 0.3)
        );
        assert_eq!(app_value("contrast", 1.5), 50.);
        assert!((app_value("pivot", 0.3) - 30.).abs() < 1e-9);
        assert_eq!(app_value("temperature", -1000.), -100.);
        assert_eq!(app_value("highlights", -50.), -50.);
    }
    #[test]
    fn edits_hold_everything_but_the_control() {
        let e = edits_for("highlights", -50.).unwrap();
        assert_eq!(e["highlights"], -50.);
        assert_eq!(e["toneMapper"], "resolve");
        assert!(e["v3"]["saturation"].is_null());
        let e = edits_for("saturation", 50.).unwrap();
        assert_eq!(e["v3"]["saturation"], 50.);
        assert!(e.get("saturation").is_none());
        assert!(edits_for("midtone_detail", 1.).is_err());
        let e = edits_for("pivot", 30.).unwrap();
        assert_eq!(e["contrastPivot"], 30.);
        assert_eq!(e["contrast"], 50.);
        let e = edits_for("exposure", 1.).unwrap();
        assert_eq!(e["exposure"], 1.);
        let source: Source =
            serde_json::from_str(r#"{"id":"a","file":{"path":"a","blake3":""}}"#).unwrap();
        assert!(source.fit && source.class.is_empty());
    }
    #[test]
    fn area_resample_lands_on_the_reference_size_and_keeps_means() {
        let src = Rgba32FImage::from_fn(3264, 5, |x, _| {
            let v = (x % 7) as f32 / 7.;
            image::Rgba([v, 1. - v, 0.5, 1.])
        });
        let out = area_resample(&src, 2159, 5);
        assert_eq!(out.dimensions(), (2159, 5));
        let mean = |i: &Rgba32FImage| {
            i.pixels().map(|p| p[0] as f64).sum::<f64>() / i.pixels().len() as f64
        };
        assert!((mean(&src) - mean(&out)).abs() < 1e-4);
        assert!(
            out.pixels()
                .all(|p| (p[3] - 1.).abs() < 1e-5 && (p[2] - 0.5).abs() < 1e-5)
        );
        // Equal sizes pass through untouched.
        assert_eq!(match_size(&src, &src), src);
    }
    #[test]
    fn exposure_offset_reads_a_uniform_stop() {
        let ours = Rgba32FImage::from_fn(64, 1, |x, _| {
            let v = 0.005 + x as f32 * 0.012;
            image::Rgba([v, v, v, 1.])
        });
        let theirs = Rgba32FImage::from_fn(64, 1, |x, _| {
            let v = 2. * (0.005 + x as f32 * 0.012);
            image::Rgba([v, v, v, 1.])
        });
        let offset = exposure_offset(&ours, &theirs).unwrap();
        for stops in [
            offset.shadows_stops,
            offset.midtones_stops,
            offset.highlights_stops,
        ] {
            assert!((stops - 1.).abs() < 1e-5, "{stops}");
        }
    }
    #[test]
    fn measurement_is_precise_and_rejects_bad_inputs() {
        let a = Rgba32FImage::from_pixel(3, 2, image::Rgba([0.2, 0.3, 0.4, 1.]));
        assert_eq!(difference(&a, &a).unwrap().max_linear_rgb, 0.);
        let mut b = a.clone();
        b.get_pixel_mut(0, 0)[0] += 0.0001;
        assert!(difference(&a, &b).unwrap().max_linear_rgb > 0.00009);
        let mut c = a.clone();
        for p in c.pixels_mut() {
            p[1] += 0.01;
        }
        let d = difference(&c, &a).unwrap();
        // 0.30 -> 0.31 linear is 2.2 levels at this brightness.
        assert!((d.bias_levels[1] - 2.2).abs() < 0.15, "{:?}", d.bias_levels);
        assert!(d.bias_levels[0] == 0. && (d.mean_levels - d.bias_levels[1] / 3.).abs() < 1e-9);
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
