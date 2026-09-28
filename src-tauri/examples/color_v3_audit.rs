//! Before/after pixel and warm-render benchmark. No photo/sidecar writes.
use anyhow::{Context, Result};
use rapidraw_lib::{
    AppState,
    color_engine::{application, identity},
    image_processing::GpuContext,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let photo = args
        .first()
        .context("PHOTO TRANSFORM_DIRECTORY ASSET_DIRECTORY")?;
    let transforms = PathBuf::from(args.get(1).context("Transform directory")?);
    let state = AppState::default();
    *state.v3_asset_dir.lock().unwrap() =
        Some(PathBuf::from(args.get(2).context("Asset directory")?));
    for (slot, name) in [
        (&state.input_transform, "input-transform.cube"),
        (&state.output_transform, "output-transform.cube"),
        (&state.input_transform_p3, "input-transform-p3.cube"),
    ] {
        let path = transforms.join(name);
        if path.is_file() {
            *slot.lock().unwrap() = Some(path);
        }
    }
    let pipeline = identity::pin(&state, &serde_json::json!({}))?;
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
    for (name, mut edits) in [
        (
            "neutral",
            serde_json::json!({"processVersion":3,"toneMapper":"resolve","v3":{}}),
        ),
        (
            "tone",
            serde_json::json!({"processVersion":3,"toneMapper":"resolve","exposure":0.3,"shadows":30,"highlights":-35,"v3":{}}),
        ),
        (
            "detail",
            serde_json::json!({"processVersion":3,"toneMapper":"resolve","v3":{"detail":{"clarity":12,"texture":10}}}),
        ),
        (
            "mask",
            serde_json::json!({"processVersion":3,"toneMapper":"resolve","v3":{},"masks":[{"id":"m","name":"m","visible":true,"invert":false,"opacity":100,"adjustments":{"exposure":0.4},"subMasks":[{"id":"c","type":"color","visible":true,"mode":"additive","parameters":{"targetX":1200,"targetY":800,"tolerance":40}}]}]}),
        ),
    ] {
        edits["v3Pipeline"] = serde_json::to_value(&pipeline)?;
        let baseline = application::render_file(&context, &state, photo, &edits, Some(1600))?;
        let hash = blake3::hash(bytemuck::cast_slice(baseline.encoded_srgb.as_raw()))
            .to_hex()
            .to_string();
        let mut times = vec![];
        for _ in 0..7 {
            let start = std::time::Instant::now();
            let frame = application::render_file(&context, &state, photo, &edits, Some(1600))?;
            times.push(start.elapsed().as_secs_f64() * 1000.);
            anyhow::ensure!(
                frame.encoded_srgb == baseline.encoded_srgb,
                "Repeated render differs"
            );
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({"case":name,"float_hash":hash,"median_ms":times[3]})
        );
    }
    Ok(())
}
