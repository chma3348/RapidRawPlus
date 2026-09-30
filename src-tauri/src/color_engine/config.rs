use serde::{Deserialize, Serialize};

/// The colour space finished pictures are encoded in: previews in the
/// editor and exported files, always together, so what the editor shows is
/// what the file holds. Display P3 keeps colours sRGB cannot (richer greens,
/// reds and cyans) when a Resolve P3 output capture is installed; without
/// one, a P3 picture is the sRGB rendering stored as P3, identical to look at.
/// Everything else (thumbnails, AI inputs, calibration) stays sRGB.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OutputSpace {
    #[default]
    Srgb,
    DisplayP3,
}

impl OutputSpace {
    /// The app setting: "srgb" chooses sRGB; anything else, including no
    /// setting at all, is the default Display P3.
    pub fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("srgb") => Self::Srgb,
            _ => Self::DisplayP3,
        }
    }

    /// The ICC profile a file in this space carries.
    pub fn icc_profile(self) -> anyhow::Result<Vec<u8>> {
        let profile = match self {
            Self::Srgb => moxcms::ColorProfile::new_srgb(),
            Self::DisplayP3 => moxcms::ColorProfile::new_display_p3(),
        };
        Ok(profile.encode()?)
    }

    /// EXIF's ColorSpace tag: 1 is sRGB; anything else is "uncalibrated",
    /// meaning the embedded profile decides (what Apple writes for P3).
    pub fn exif_color_space(self) -> u16 {
        match self {
            Self::Srgb => 1,
            Self::DisplayP3 => 0xFFFF,
        }
    }
}

/// All supported primaries use D65. Other white points/ICC profiles must be
/// resolved by an input adapter before using this experimental pipeline.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Primaries {
    Srgb,
    DavinciWideGamut,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    Linear,
    Srgb,
    DavinciIntermediate,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceDomain {
    Scene,
    Display,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceColor {
    pub primaries: Primaries,
    pub transfer: Transfer,
    pub reference: ReferenceDomain,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputRendering {
    /// Preserve an already-rendered image at neutral settings.
    DisplayPassthroughV1,
    /// Provisional maximum-channel shoulder, NOT Resolve's DRT.
    SceneShoulderV1,
    /// Luminance shoulder plus hue-preserving SDR gamut projection.
    SceneLuminanceV1,
    /// No second tone map for rendered photos; project out-of-gamut chroma.
    DisplayGamutV1,
    /// As `SceneLuminanceV1`, but compressing chroma toward the gamut instead
    /// of projecting onto it, so saturated regions keep their gradation.
    SceneLuminanceV2,
    /// As `DisplayGamutV1`, with the same soft chroma compression.
    DisplayGamutV2,
    /// A rendering transform captured from DaVinci Resolve on this machine,
    /// applied in DaVinci Intermediate. Requires `output_lut`.
    ResolveCubeV1,
    /// The previous engine's tone mappers, its own functions
    /// (tone_v2.wgsl), for the Basic panel's Tone Mapper switch.
    PreviousBasic,
    PreviousAgx,
    PreviousFilmic,
}

/// Input interpretation must be explicit. Creative controls default to neutral;
/// unknown fields are errors rather than silently dropped edits.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PipelineConfig {
    pub process_version: u32,
    pub source: SourceColor,
    pub working_space: Primaries,
    pub output_rendering: OutputRendering,
    /// The captured cube, for `ResolveCubeV1` only. Its contents, not its
    /// path, are what the render is keyed on.
    #[serde(default)]
    pub output_lut: Option<std::path::PathBuf>,
    #[serde(default)]
    pub controls: super::controls::Controls,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineVersion {
    V3,
}

pub fn engine_for_version(version: u32) -> anyhow::Result<EngineVersion> {
    // Development-era edit versions adopt the current engine.
    match version {
        0..=3 => Ok(EngineVersion::V3),
        _ => anyhow::bail!("Unsupported color engine version {version}"),
    }
}
