//! Renders for calibrating the lighting sliders against Lightroom: each source
//! photo, unedited and across each slider's range, exactly as the app renders
//! it (installed transforms, v3), saved as raw pixels for
//! tools/adobe_lighting.py to measure. Nothing here changes a slider.
//!
//!   adobe_sweep SOURCE_DIR OUT_DIR MAX_DIMENSION CONTROL:V1,V2,... [CONTROL:...]
//!
//! Writes OUT_DIR/<stem>__<control>_<value>.f32 (little-endian RGB float32,
//! display-encoded sRGB) and a matching .json with the size. Existing files
//! are kept, so an interrupted sweep resumes.
use anyhow::{Context, Result, ensure};
use rapidraw_lib::{
    AppState, color_engine::application, color_engine::reference, image_processing::GpuContext,
};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

fn gpu() -> Result<GpuContext> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))?;
    Ok(GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
    })
}

/// The transforms the app has installed, as it finds them at startup.
fn installed_state() -> Result<AppState> {
    let state = AppState::default();
    let support = PathBuf::from(std::env::var("HOME")?)
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
    Ok(state)
}

/// The edits for one control at one value: the lighting and colour
/// controls as the reference package defines them, and the detail and
/// effect controls Lightroom exports were made for (Clarity, Texture,
/// Dehaze; Grain at Lightroom's default size 25 and roughness 50, which are
/// RapidRAW's defaults too).
fn edits_for(control: &str, value: f64) -> Result<serde_json::Value> {
    match control {
        "clarity" | "texture" | "dehaze" => {
            let mut edits = reference::edits_for("neutral", 0.0)?;
            edits["v3"]["detail"][control] = serde_json::json!(value);
            Ok(edits)
        }
        "grain" => {
            let mut edits = reference::edits_for("neutral", 0.0)?;
            edits["v3"]["effects"]["grain_amount"] = serde_json::json!(value);
            Ok(edits)
        }
        _ => reference::edits_for(control, value),
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() >= 4,
        "adobe_sweep SOURCE_DIR OUT_DIR MAX_DIMENSION CONTROL:V1,V2,... [CONTROL:...]"
    );
    let source_dir = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    let max: u32 = args[2].parse()?;
    std::fs::create_dir_all(&out)?;
    let mut cases = vec![("neutral".to_string(), 0.0f64)];
    for spec in &args[3..] {
        let (control, values) = spec.split_once(':').context("CONTROL:V1,V2,...")?;
        for v in values.split(',') {
            cases.push((control.to_string(), v.parse()?));
        }
    }
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&source_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && !p.file_name().unwrap().to_string_lossy().starts_with('.'))
        .collect();
    sources.sort();
    let context = gpu()?;
    let state = installed_state()?;
    for source in &sources {
        let stem = source.file_stem().unwrap().to_string_lossy().into_owned();
        let path = source.to_str().context("Non-UTF8 path")?;
        for (control, value) in &cases {
            let name = format!("{stem}__{control}_{value}");
            let bin = out.join(format!("{name}.f32"));
            if bin.exists() {
                continue;
            }
            let started = Instant::now();
            let edits = edits_for(control, *value)?;
            let frame = application::render_file(&context, &state, path, &edits, Some(max))?;
            let image = &frame.encoded_srgb;
            let mut bytes =
                Vec::with_capacity(image.width() as usize * image.height() as usize * 12);
            for p in image.pixels() {
                for c in 0..3 {
                    bytes.extend_from_slice(&p[c].to_le_bytes());
                }
            }
            let tmp = out.join(format!("{name}.f32.part"));
            std::fs::File::create(&tmp)?.write_all(&bytes)?;
            std::fs::write(
                out.join(format!("{name}.json")),
                serde_json::json!({
                    "width": image.width(), "height": image.height(),
                    "space": format!("{:?}", frame.space),
                    "tones": frame.tones.map(|t| serde_json::json!({"median": t.median, "brightest": t.brightest})),
                    "edits": edits,
                })
                .to_string(),
            )?;
            std::fs::rename(&tmp, &bin)?;
            println!(
                "{name} {}x{} {:.1}s",
                image.width(),
                image.height(),
                started.elapsed().as_secs_f32()
            );
        }
    }
    Ok(())
}
