//! The application's sole color pipeline, shared by previews and exports.
//! Explicit source interpretation, float DWG controls and fixed SDR sRGB output.
//! Development-era edits adopt this engine; shared tone math is retained.
pub mod application;
pub mod config;
pub mod contract;
pub mod controls;
pub mod cube;
pub mod detail;
mod file_version;
pub mod identity;
pub mod input;
pub mod migration;
pub mod optics;
pub mod patches;
pub mod plan;
pub mod raw;
pub mod raw_look;
pub mod raw_look_table;
pub mod rcd;
pub mod reference;
mod renderer;
pub mod selection;
pub mod spaces;
pub mod tone_zones_table;

pub(crate) use renderer::tpdf as renderer_tpdf;
pub use renderer::{ColorEngine, RenderedFrame, StageCapture};

pub fn shader_source() -> String {
    [
        include_str!("../shaders/tone_v2.wgsl"),
        include_str!("../shaders/color_v3/spaces.wgsl"),
        include_str!("../shaders/color_v3/output.wgsl"),
        include_str!("../shaders/color_v3/primary.wgsl"),
        include_str!("../shaders/color_v3/main.wgsl"),
    ]
    .join("\n")
}
