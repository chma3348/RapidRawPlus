//! Full-resolution neutral v3 renders of several photographs, as the app
//! renders them (installed transforms), written as 16-bit PNGs for
//! comparison against Resolve exports.
//!   cargo run --release --example render_neutral -- OUT_DIR PHOTO...
use anyhow::Result;
use rapidraw_lib::color_engine::application::render_aside;
use rapidraw_lib::image_processing::GpuContext;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = std::path::PathBuf::from(&args[0]);
    std::fs::create_dir_all(&out)?;
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))?;
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
    };
    let state = rapidraw_lib::AppState::default();
    let support = std::path::PathBuf::from(std::env::var("HOME")?)
        .join("Library/Application Support/io.github.CyberTimon.RapidRAW");
    for (slot, name) in [
        (&state.output_transform, "output-transform.cube"),
        (&state.input_transform, "input-transform.cube"),
        (&state.input_transform_p3, "input-transform-p3.cube"),
    ] {
        let path = support.join(name);
        if path.exists() {
            *slot.lock().unwrap() = Some(path);
        }
    }
    let edits: serde_json::Value = std::env::var("EDITS")
        .ok()
        .map(|e| serde_json::from_str(&e).unwrap())
        .unwrap_or_else(|| json!({"processVersion": 3, "toneMapper": "resolve", "v3": {}}));
    for path in &args[1..] {
        let start = std::time::Instant::now();
        let frame = render_aside(&context, &state, path, &edits, None)?;
        let stem = std::path::Path::new(path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        frame
            .export_rgba16()
            .to_rgb16()
            .save(out.join(format!("{stem}.png")))?;
        println!(
            "{stem}: {}x{} {:.0} ms",
            frame.encoded_srgb.width(),
            frame.encoded_srgb.height(),
            start.elapsed().as_secs_f64() * 1000.
        );
    }
    Ok(())
}
