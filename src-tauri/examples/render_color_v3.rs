//! Developer-only renderer; requires explicit interpretation of decoded pixels.
use anyhow::{ensure, Context, Result};
use rapidraw_lib::color_engine::{
    config::*, input::decode_profiled_photo, plan::RenderPlan, ColorEngine,
};
use rapidraw_lib::image_processing::GpuContext;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    ensure!(
        args.len() == 4,
        "Usage: render_color_v3 INPUT NEW_OUTPUT_DIRECTORY CONFIG_JSON_OR_auto_OR_raw_OR_raw-fast"
    );
    let input_path = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    let bytes = std::fs::read(&input_path)?;
    let (input, config, provenance) = if args[3] == "auto"
        || args[3] == "raw"
        || args[3] == "raw-fast"
    {
        let decoded = if args[3] == "auto" {
            decode_profiled_photo(&bytes)?
        } else {
            rapidraw_lib::color_engine::raw::decode_raw(&bytes, args[3] == "raw-fast", || Ok(()))?
        };
        let output_rendering = if decoded.color.reference == ReferenceDomain::Scene {
            OutputRendering::SceneShoulderV1
        } else {
            OutputRendering::DisplayPassthroughV1
        };
        let config = PipelineConfig {
            controls: Default::default(),
            process_version: 3,
            source: decoded.color,
            working_space: Primaries::DavinciWideGamut,
            output_rendering,
        };
        (decoded.pixels, config, Some(decoded.provenance))
    } else {
        // Explicit mode bypasses profile interpretation, for controlled fixtures.
        let config: PipelineConfig = serde_json::from_slice(&std::fs::read(&args[3])?)?;
        (image::load_from_memory(&bytes)?.to_rgba32f(), config, None)
    };
    let plan = RenderPlan::build(config)?;
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))?;
    let engine = ColorEngine::new(GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
    })?;
    let start = std::time::Instant::now();
    let frame = engine.render(&input, &plan, true)?;
    let render_ms = start.elapsed().as_secs_f64() * 1000.0;
    // Refuse existing directories so evaluation cannot overwrite previous artifacts.
    std::fs::create_dir(&output).context("Output directory must not already exist")?;
    frame.write_srgb_png(std::fs::File::create(output.join("preview.png"))?, false)?;
    frame.write_srgb_png(std::fs::File::create(output.join("export-16.png"))?, true)?;
    let stages = frame.stages.context("Missing requested captures")?;
    stages.working.save(output.join("working-linear-dwg.exr"))?;
    stages.graded.save(output.join("graded-linear-dwg.exr"))?;
    let manifest = serde_json::json!({
        "status": "experimental-exposure-only",
        "config": plan.config(),
        "source": input_path,
        "input_provenance": provenance,
        "fingerprint": plan.fingerprint(&format!("{}:{}", blake3::hash(&bytes), serde_json::to_string(&provenance)?)),
        "render_ms_including_readback": render_ms,
        "width": input.width(), "height": input.height(),
        "output_encoding": "sRGB/D65 with embedded ICC profile",
        "stage_encoding": "linear DaVinci Wide Gamut/D65; reference domain from config"
    });
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!(
        "V3 evaluation saved to {} ({render_ms:.1} ms)",
        output.display()
    );
    Ok(())
}
