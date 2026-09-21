//! Opt-in, versioned color pipeline shared by application previews and exports.
//! Explicit source interpretation, float DWG controls and fixed SDR sRGB output.
//! Application defaults and legacy edit rendering remain unchanged.
pub mod config;
pub mod application;
pub mod controls;
pub mod cube;
pub mod selection;
pub mod input;
pub mod raw;
pub mod plan;
mod renderer;
pub mod spaces;

pub use renderer::{ColorEngine, RenderedFrame, StageCapture};

pub fn shader_source() -> String {
    [
        include_str!("../shaders/color_v3/spaces.wgsl"),
        include_str!("../shaders/color_v3/output.wgsl"),
        include_str!("../shaders/color_v3/primary.wgsl"),
        include_str!("../shaders/color_v3/main.wgsl"),
    ]
    .join("\n")
}
