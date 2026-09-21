//! Interactive cost of v3 through the application path, on a real photograph.
//!
//!   cargo run --release --example color_v3_timing -- PHOTO
use anyhow::Result;
use rapidraw_lib::color_engine::application::render_file;
use rapidraw_lib::image_processing::GpuContext;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("a photograph to time");
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
    let mask = |target: f64| {
        json!([{
            "id":"m","name":"m","visible":true,"invert":false,"opacity":100,
            "adjustments":{"v3":{"exposure":0.5}},
            "subMasks":[{"id":"c","type":"color","visible":true,"mode":"additive",
                "parameters":{"targetX": target, "targetY": 800, "tolerance": 40}}]
        }])
    };
    let time = |label: &str, edits: serde_json::Value, dim: Option<u32>| -> Result<()> {
        let start = std::time::Instant::now();
        render_file(&context, &state, &path, &edits, dim)?;
        println!("{label:44} {:>7.0} ms", start.elapsed().as_secs_f64() * 1000.0);
        Ok(())
    };
    let preview = Some(1600);
    time("decode + first preview", json!({"processVersion":3,"v3":{},"masks":[]}), preview)?;
    time("preview, warm", json!({"processVersion":3,"v3":{"exposure":0.3},"masks":[]}), preview)?;
    time("add a colour range mask (cold sampling)", json!({"processVersion":3,"v3":{},"masks":mask(1200.0)}), preview)?;
    time("move a slider with the mask (warm)", json!({"processVersion":3,"v3":{"exposure":0.4},"masks":mask(1200.0)}), preview)?;
    time("re-click the mask (warm sampling)", json!({"processVersion":3,"v3":{},"masks":mask(2400.0)}), preview)?;
    time("rotate 1 degree (sampling rebuilt)", json!({"processVersion":3,"v3":{},"rotation":1.0,"masks":mask(2400.0)}), preview)?;
    let linear = json!([{
        "id":"l","name":"l","visible":true,"invert":false,"opacity":100,
        "adjustments":{"v3":{"exposure":0.5}},
        "subMasks":[{"id":"g","type":"linear","visible":true,"mode":"additive",
            "parameters":{"startX":0,"startY":0,"endX":6000,"endY":4000,"range":1000}}]
    }]);
    time("gradient mask instead (no sampling)", json!({"processVersion":3,"v3":{},"rotation":1.0,"masks":linear.clone()}), preview)?;
    time("gradient mask, slider moved", json!({"processVersion":3,"v3":{"exposure":0.2},"rotation":1.0,"masks":linear}), preview)?;
    time("full-resolution export with the mask", json!({"processVersion":3,"v3":{},"rotation":1.0,"masks":mask(2400.0)}), None)?;
    Ok(())
}
