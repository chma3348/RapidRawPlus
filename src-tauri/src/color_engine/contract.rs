//! The application's stage order, named for diagnostics and calibration
//! reports. Change the revision and baselines before reordering the stages in
//! `application::render_file_with_capture`, which follows this order.
use serde::Serialize;

pub const REVISION: &str = "v3-application-stages-5";
/// Diagnostic fingerprint in calibration reports. This is not a substitute
/// for renderer-version compatibility, nor a claim to hash the entire binary.
pub fn implementation_digest() -> &'static str {
    static DIGEST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut hash = blake3::Hasher::new();
        for source in [
            include_str!("application.rs"),
            include_str!("migration.rs"),
            include_str!("identity.rs"),
            include_str!("input.rs"),
            include_str!("raw.rs"),
            include_str!("controls.rs"),
            include_str!("plan.rs"),
            include_str!("renderer.rs"),
            include_str!("spaces.rs"),
            include_str!("cube.rs"),
            include_str!("detail.rs"),
            include_str!("optics.rs"),
            include_str!("patches.rs"),
            include_str!("../../Cargo.lock"),
            include_str!("../shaders/tone_v2.wgsl"),
            include_str!("../shaders/color_v3/spaces.wgsl"),
            include_str!("../shaders/color_v3/output.wgsl"),
            include_str!("../shaders/color_v3/primary.wgsl"),
            include_str!("../shaders/color_v3/main.wgsl"),
        ] {
            hash.update(&(source.len() as u64).to_le_bytes());
            hash.update(source.as_bytes());
        }
        hash.finalize().to_hex().to_string()
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    SourceInterpretationAndRecovery,
    PatchesFlatFieldGeometryAndSampling,
    GlobalSpatialProcessing,
    WorkingConversionAndGlobalGrade,
    OrderedLocalGradesAndBlends,
    CreativeLookOutputAndGrain,
}
pub const ORDER: [Stage; 6] = [
    Stage::SourceInterpretationAndRecovery,
    Stage::PatchesFlatFieldGeometryAndSampling,
    Stage::GlobalSpatialProcessing,
    Stage::WorkingConversionAndGlobalGrade,
    Stage::OrderedLocalGradesAndBlends,
    Stage::CreativeLookOutputAndGrain,
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn order_is_the_documented_one() {
        assert_eq!(ORDER[0], Stage::SourceInterpretationAndRecovery);
        assert_eq!(ORDER[5], Stage::CreativeLookOutputAndGrain);
        assert_eq!(implementation_digest().len(), 64);
    }
}
