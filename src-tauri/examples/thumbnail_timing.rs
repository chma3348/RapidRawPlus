//! Cost of a library thumbnail, and proof that making one leaves the
//! editor's caches alone.
//!   cargo run --release --example thumbnail_timing -- EDITOR_PHOTO THUMB_PHOTO...
use anyhow::Result;
use rapidraw_lib::color_engine::application::{render_file, render_thumbnail};
use rapidraw_lib::image_processing::GpuContext;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    let edits = json!({"processVersion": 3, "toneMapper": "resolve", "v3": {}});
    // The editor has a photo open.
    render_file(&context, &state, &args[0], &edits, Some(1600))?;
    let held = state
        .v3
        .source
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| Arc::as_ptr(&c.frame));
    for path in &args[1..] {
        let start = std::time::Instant::now();
        let thumb = render_thumbnail(&context, &state, path, &json!({}), 720)?;
        println!(
            "thumbnail {:44} {}x{} {:>5.0} ms",
            std::path::Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy(),
            thumb.encoded_srgb.width(),
            thumb.encoded_srgb.height(),
            start.elapsed().as_secs_f64() * 1000.
        );
    }
    let still = state
        .v3
        .source
        .lock()
        .unwrap()
        .as_ref()
        .map(|c| Arc::as_ptr(&c.frame));
    anyhow::ensure!(held == still, "thumbnails evicted the editor's source");
    let start = std::time::Instant::now();
    render_file(
        &context,
        &state,
        &args[0],
        &json!({"processVersion":3,"toneMapper":"resolve","exposure":0.3,"v3":{}}),
        Some(1600),
    )?;
    println!(
        "editor slider after thumbnails {:>5.0} ms (warm: caches kept)",
        start.elapsed().as_secs_f64() * 1000.
    );
    Ok(())
}
