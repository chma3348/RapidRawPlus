use serde::{Deserialize, Serialize};

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
    Legacy,
    ExperimentalV3,
}

pub fn engine_for_version(version: u32) -> anyhow::Result<EngineVersion> {
    // 0 is the existing zero-initialized legacy GPU parameter block.
    match version {
        0..=2 => Ok(EngineVersion::Legacy),
        3 => Ok(EngineVersion::ExperimentalV3),
        _ => anyhow::bail!("Unsupported color engine version {version}"),
    }
}
