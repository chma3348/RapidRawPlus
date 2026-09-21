//! What v3's auto adjustment chooses for each photograph given, through the
//! captured Resolve transforms when they are installed, as the app renders.
//!
//!   cargo run --release --example v3_auto -- PHOTO...
use anyhow::Result;
use rapidraw_lib::color_engine::application::auto_controls;
use rapidraw_lib::image_processing::GpuContext;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
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
    for path in std::env::args().skip(1) {
        let edits = json!({"processVersion": 3, "v3": {}, "masks": []});
        let start = std::time::Instant::now();
        let auto = auto_controls(&context, &state, &path, &edits)?;
        if let Ok(dir) = std::env::var("V3_AUTO_SHEETS") {
            let before = rapidraw_lib::color_engine::application::render_file(
                &context,
                &state,
                &path,
                &edits,
                Some(600),
            )?
            .preview_rgba8();
            let mut adjusted = edits.clone();
            adjusted["v3"] = auto.clone();
            let after = rapidraw_lib::color_engine::application::render_file(
                &context,
                &state,
                &path,
                &adjusted,
                Some(600),
            )?
            .preview_rgba8();
            let mut sheet = image::RgbaImage::new(before.width() * 2, before.height());
            image::imageops::overlay(&mut sheet, &before, 0, 0);
            image::imageops::overlay(&mut sheet, &after, before.width() as i64, 0);
            let name = std::path::Path::new(&path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string();
            image::DynamicImage::ImageRgba8(sheet)
                .to_rgb8()
                .save(std::path::Path::new(&dir).join(format!("auto_{name}.jpg")))?;
        }
        println!(
            "{:48} {auto} ({:.0} ms)",
            std::path::Path::new(&path)
                .file_name()
                .unwrap()
                .to_string_lossy(),
            start.elapsed().as_secs_f64() * 1000.
        );
    }
    Ok(())
}
