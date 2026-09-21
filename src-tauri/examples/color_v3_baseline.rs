//! Phase 0 baseline: render a fixed fixture set through v3 and measure it.
//!
//! This is the objective half of the roadmap's Phase 0 gate. It does not know
//! what a good photograph looks like and makes no claim about matching
//! Resolve. What it does is make the engine's behaviour *comparable*: the same
//! fixtures and the same control cases, measured the same way, before and
//! after a change, so an argument about whether something improved can be
//! settled with numbers instead of impressions.
//!
//! The measurements are chosen from the roadmap's own acceptance rules:
//! clipping, neutral-axis drift, hue drift weighted by chroma, banding, and
//! the lightness/chroma distribution. A regression in any of them is a
//! question to answer, not automatically a fault — the point is that it gets
//! noticed.
//!
//!   cargo run --release --manifest-path src-tauri/Cargo.toml \
//!     --example color_v3_baseline -- MANIFEST.json NEW_OUTPUT_DIRECTORY
//!
//! The manifest lists fixtures (path plus why it is in the set) and cases
//! (named v3 control settings). Every fixture is rendered through every case.

use anyhow::{Context, Result, ensure};
use rapidraw_lib::color_engine::{
    ColorEngine, config::*, controls::Controls, input::decode_profiled_photo, plan::RenderPlan,
    spaces,
};
use rapidraw_lib::image_processing::GpuContext;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Deserialize)]
struct Manifest {
    /// Longest edge the fixtures are rendered at. Colour operators are
    /// pointwise, so this changes measurement cost, not measured behaviour.
    #[serde(default = "default_size")]
    max_dimension: u32,
    /// A transform captured from Resolve. When present every case renders
    /// through it instead of the built-in rendering, so the two runs differ
    /// in exactly one thing.
    #[serde(default)]
    output_transform: Option<PathBuf>,
    /// The matching input transform, which undoes the rendering a photograph
    /// already carries so the output one is not a second rendering.
    #[serde(default)]
    input_transform: Option<PathBuf>,
    fixtures: Vec<Fixture>,
    cases: Vec<Case>,
}

fn default_size() -> u32 {
    1024
}

#[derive(Deserialize)]
struct Fixture {
    name: String,
    path: PathBuf,
    /// What this photograph is in the set to exercise.
    why: String,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    #[serde(default)]
    controls: Controls,
}

#[derive(Serialize)]
struct Measurement {
    fixture: String,
    why: String,
    case: String,
    pixels: u32,
    render_ms: f64,
    /// Fraction of pixels with any channel at the floor or the ceiling.
    clipped_low: f64,
    clipped_high: f64,
    /// Largest departure from neutral among pixels the source had as neutral.
    /// A grade that claims not to touch neutrals is measured here.
    neutral_drift: f64,
    /// Mean Oklab hue rotation, weighted by chroma so noise in near-grey
    /// pixels — where hue means little — cannot dominate the number.
    hue_drift_degrees: f64,
    mean_lightness: f64,
    mean_chroma: f64,
    /// Longest run of one 8-bit level along a row: a banding proxy.
    longest_flat_run: u32,
}

/// The input adapter hands back linear sRGB-primaries pixels, whatever the
/// file was tagged as; the rendered frame is sRGB-encoded. Measuring one as
/// though it were the other invents a difference that is not there.
fn oklab_linear(rgb: [f32; 3]) -> glam::DVec3 {
    spaces::oklab_from_rgb(
        Primaries::Srgb,
        glam::DVec3::new(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64),
    )
}

fn oklab_encoded(rgb: [f32; 3]) -> glam::DVec3 {
    let decode = |v: f32| spaces::decode(v as f64, Transfer::Srgb);
    spaces::oklab_from_rgb(
        Primaries::Srgb,
        glam::DVec3::new(decode(rgb[0]), decode(rgb[1]), decode(rgb[2])),
    )
}

fn measure(
    fixture: &Fixture,
    case: &str,
    source: &image::Rgba32FImage,
    rendered: &image::RgbaImage,
    render_ms: f64,
) -> Measurement {
    let (mut low, mut high) = (0u64, 0u64);
    let (mut drift, mut hue_sum, mut hue_weight) = (0.0f64, 0.0f64, 0.0f64);
    let (mut lightness, mut chroma) = (0.0f64, 0.0f64);
    for (a, b) in source.pixels().zip(rendered.pixels()) {
        let out = [
            b[0] as f32 / 255.0,
            b[1] as f32 / 255.0,
            b[2] as f32 / 255.0,
        ];
        if b[0].min(b[1]).min(b[2]) == 0 {
            low += 1;
        }
        if b[0].max(b[1]).max(b[2]) == 255 {
            high += 1;
        }
        let (before, after) = (oklab_linear([a[0], a[1], a[2]]), oklab_encoded(out));
        lightness += after.x;
        let hypot = |v: glam::DVec3| (v.y * v.y + v.z * v.z).sqrt();
        let (c0, c1) = (hypot(before), hypot(after));
        chroma += c1;
        // Neutral in, neutral out: only meaningful where the source was grey.
        if c0 < 0.002 {
            drift = drift.max(c1);
        }
        // Hue is only meaningful where there is chroma to carry it. At eight
        // bits a barely-coloured pixel can swing its hue angle a long way on
        // one level of rounding, so the floor is set well above that.
        if c0 > 0.05 && c1 > 0.05 {
            let turn = (after.z.atan2(after.y) - before.z.atan2(before.y))
                .sin()
                .asin()
                .to_degrees()
                .abs();
            hue_sum += turn * c0;
            hue_weight += c0;
        }
    }
    let count = source.pixels().len() as f64;
    let mut longest = 1u32;
    for y in 0..rendered.height() {
        let mut run = 1u32;
        for x in 1..rendered.width() {
            run = if rendered.get_pixel(x, y)[0] == rendered.get_pixel(x - 1, y)[0] {
                run + 1
            } else {
                1
            };
            longest = longest.max(run);
        }
    }
    Measurement {
        fixture: fixture.name.clone(),
        why: fixture.why.clone(),
        case: case.to_string(),
        pixels: source.pixels().len() as u32,
        render_ms,
        clipped_low: low as f64 / count,
        clipped_high: high as f64 / count,
        neutral_drift: drift,
        hue_drift_degrees: if hue_weight > 0.0 {
            hue_sum / hue_weight
        } else {
            0.0
        },
        mean_lightness: lightness / count,
        mean_chroma: chroma / count,
        longest_flat_run: longest,
    }
}

fn load(
    path: &Path,
    max_dimension: u32,
    input_transform: Option<&rapidraw_lib::color_engine::cube::CubeLut>,
) -> Result<(image::Rgba32FImage, SourceColor)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let decoded = decode_profiled_photo(&bytes)
        .with_context(|| format!("interpreting {}", path.display()))?;
    let (w, h) = decoded.pixels.dimensions();
    let pixels = if w.max(h) > max_dimension {
        image::DynamicImage::ImageRgba32F(decoded.pixels)
            .resize(max_dimension, max_dimension, image::imageops::CatmullRom)
            .to_rgba32f()
    } else {
        decoded.pixels
    };
    let mut pixels = pixels;
    let mut color = decoded.color;
    if let Some(cube) = input_transform {
        if color.reference == ReferenceDomain::Display {
            rapidraw_lib::color_engine::cube::apply_input_transform(cube, &mut pixels);
            color = SourceColor {
                primaries: Primaries::DavinciWideGamut,
                transfer: Transfer::Linear,
                reference: ReferenceDomain::Scene,
            };
        }
    }
    Ok((pixels, color))
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    ensure!(
        args.len() == 3,
        "Usage: color_v3_baseline MANIFEST_JSON NEW_OUTPUT_DIRECTORY"
    );
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    ensure!(
        !manifest.fixtures.is_empty() && !manifest.cases.is_empty(),
        "A baseline needs at least one fixture and one case"
    );
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir(&output).context("Output directory must not already exist")?;

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

    let input_transform = manifest
        .input_transform
        .as_ref()
        .map(|p| rapidraw_lib::color_engine::cube::CubeLut::load(p))
        .transpose()?;
    let mut measurements = Vec::new();
    for fixture in &manifest.fixtures {
        let (pixels, color) = load(
            &fixture.path,
            manifest.max_dimension,
            input_transform.as_ref(),
        )?;
        let rendering = match (&manifest.output_transform, color.reference) {
            (Some(_), _) => OutputRendering::ResolveCubeV1,
            (None, ReferenceDomain::Scene) => OutputRendering::SceneLuminanceV2,
            (None, ReferenceDomain::Display) => OutputRendering::DisplayGamutV2,
        };
        for case in &manifest.cases {
            let plan = RenderPlan::build(PipelineConfig {
                process_version: 3,
                source: color.clone(),
                working_space: Primaries::DavinciWideGamut,
                output_lut: manifest.output_transform.clone(),
                output_rendering: rendering,
                controls: case.controls.clone(),
            })?;
            let start = std::time::Instant::now();
            let frame = engine.render(&pixels, &plan, false)?;
            let render_ms = start.elapsed().as_secs_f64() * 1000.0;
            // Measured undithered: the dither is a display choice, and the
            // metrics are here to describe the engine.
            let encoded = frame.preview_rgba8();
            let directory = output.join(&case.name);
            std::fs::create_dir_all(&directory)?;
            frame.write_display_png(std::fs::File::create(
                directory.join(format!("{}.png", fixture.name)),
            )?)?;
            measurements.push(measure(fixture, &case.name, &pixels, &encoded, render_ms));
            println!("{} / {}: {render_ms:.1}ms", fixture.name, case.name);
        }
    }
    serde_json::to_writer_pretty(
        std::fs::File::create(output.join("measurements.json"))?,
        &measurements,
    )?;
    println!(
        "\n{} renders over {} fixtures written to {}",
        measurements.len(),
        manifest.fixtures.len(),
        output.display()
    );
    Ok(())
}
