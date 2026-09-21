//! Developer tool: a contact sheet of v3's lens and film effects on one
//! photograph, and what they cost at full resolution.
//!
//! `cargo run --release --example optics_sheet -- PHOTO OUT.png [EFFECTS_JSON...]`
//!
//! Each EFFECTS_JSON is a v3 `effects` object (e.g. `{"glow_amount":60}`);
//! the first panel is always the photograph untouched. Rendered as the app
//! renders: through the captured Resolve transforms when they are installed.
use anyhow::{Result, ensure};
use rapidraw_lib::color_engine::{
    ColorEngine, config::*, controls::Controls, cube::CubeLut, optics, plan::RenderPlan,
};
use rapidraw_lib::image_processing::GpuContext;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() >= 2,
        "Usage: optics_sheet PHOTO OUT.png [EFFECTS_JSON...]"
    );
    let bytes = std::fs::read(&args[0])?;
    let mut frame = if rapidraw_lib::formats::is_raw_file(&args[0]) {
        rapidraw_lib::color_engine::raw::decode_raw(&bytes, false, || Ok(()))?
    } else {
        rapidraw_lib::color_engine::input::decode_profiled_photo(&bytes)?
    };
    let support = std::path::PathBuf::from(std::env::var("HOME")?)
        .join("Library/Application Support/io.github.CyberTimon.RapidRAW");
    let output_cube = Some(support.join("output-transform.cube")).filter(|p| p.exists());
    let input_cube = support.join("input-transform.cube");
    if frame.color.reference == ReferenceDomain::Display && input_cube.exists() {
        rapidraw_lib::color_engine::cube::apply_input_transform(
            &CubeLut::load(&input_cube)?,
            &mut frame.pixels,
        );
        frame.color = SourceColor {
            primaries: Primaries::DavinciWideGamut,
            transfer: Transfer::Linear,
            reference: ReferenceDomain::Scene,
        };
    }
    let full = image::DynamicImage::ImageRgba32F(frame.pixels.clone());

    let settings: Vec<serde_json::Value> = std::iter::once(serde_json::json!({}))
        .chain(args[2..].iter().map(|s| serde_json::from_str(s).unwrap()))
        .collect();

    // Full-resolution cost of each setting.
    for s in &settings[1..] {
        let e: rapidraw_lib::color_engine::controls::Effects = serde_json::from_value(s.clone())?;
        let mut pixels = frame.pixels.clone();
        let start = std::time::Instant::now();
        optics::correct_chromatic_aberration(&mut pixels, &e);
        optics::add_light(&mut pixels, &e, 0., frame.color.primaries);
        eprintln!(
            "{}x{} {s}: {:.0} ms",
            pixels.width(),
            pixels.height(),
            start.elapsed().as_secs_f64() * 1000.
        );
    }

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
    let small = full.resize(900, 900, image::imageops::FilterType::Triangle);
    let (w, h) = (small.width(), small.height());
    let columns = settings.len().min(3) as u32;
    let rows = (settings.len() as u32).div_ceil(columns);
    let mut sheet = image::RgbaImage::new(w * columns, h * rows);
    for (i, s) in settings.iter().enumerate() {
        let controls = Controls {
            effects: serde_json::from_value(s.clone())?,
            ..Default::default()
        };
        let mut pixels = small.to_rgba32f();
        optics::correct_chromatic_aberration(&mut pixels, &controls.effects);
        optics::add_light(&mut pixels, &controls.effects, 0., frame.color.primaries);
        let output_rendering = match (&output_cube, frame.color.reference) {
            (Some(_), ReferenceDomain::Scene) => OutputRendering::ResolveCubeV1,
            (_, ReferenceDomain::Scene) => OutputRendering::SceneLuminanceV2,
            (_, ReferenceDomain::Display) => OutputRendering::DisplayGamutV2,
        };
        let plan = RenderPlan::build(PipelineConfig {
            process_version: 3,
            source: frame.color.clone(),
            working_space: Primaries::DavinciWideGamut,
            output_lut: (output_rendering == OutputRendering::ResolveCubeV1)
                .then(|| output_cube.clone())
                .flatten(),
            output_rendering,
            controls: Controls::default(),
        })?;
        let rendered = engine.render(&pixels, &plan, false)?.preview_rgba8();
        let (x, y) = ((i as u32 % columns) * w, (i as u32 / columns) * h);
        image::imageops::overlay(&mut sheet, &rendered, x as i64, y as i64);
    }
    sheet.save(&args[1])?;
    Ok(())
}
