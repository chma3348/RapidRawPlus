//! A contact sheet of v3 renders of one photograph, through the app's own
//! path and the captured Resolve transforms when installed.
//!
//!   cargo run --release --example v3_sheet -- PHOTO OUT.jpg EDITS_JSON...
//!
//! Each EDITS_JSON is merged over `{"processVersion":3,"toneMapper":"resolve"}`;
//! the first panel is always that base.
use anyhow::Result;
use rapidraw_lib::color_engine::application::render_file;
use rapidraw_lib::image_processing::GpuContext;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 2,
        "Usage: v3_sheet PHOTO OUT.jpg EDITS_JSON..."
    );
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
    ] {
        let path = support.join(name);
        if path.exists() {
            *slot.lock().unwrap() = Some(path);
        }
    }
    let base = json!({"processVersion": 3, "toneMapper": "resolve", "v3": {}, "masks": []});
    let mut panels = Vec::new();
    for extra in std::iter::once("{}".to_string()).chain(args[2..].iter().cloned()) {
        let mut edits = base.clone();
        if let Value::Object(map) = serde_json::from_str::<Value>(&extra)? {
            for (k, v) in map {
                edits[k] = v;
            }
        }
        panels.push(render_file(&context, &state, &args[0], &edits, Some(700))?.preview_rgba8());
    }
    let (w, h) = panels[0].dimensions();
    let columns = panels.len().min(3) as u32;
    let rows = (panels.len() as u32).div_ceil(columns);
    let mut sheet = image::RgbaImage::new(w * columns, h * rows);
    for (i, p) in panels.iter().enumerate() {
        let (x, y) = ((i as u32 % columns) * w, (i as u32 / columns) * h);
        image::imageops::overlay(&mut sheet, p, x as i64, y as i64);
    }
    image::DynamicImage::ImageRgba8(sheet)
        .to_rgb8()
        .save(&args[1])?;
    Ok(())
}
