//! Reference intake and full-resolution measurements, never slider fitting.
use anyhow::{Context, Result, ensure};
use rapidraw_lib::{
    AppState,
    color_engine::{
        application, identity,
        reference::{self, Package},
    },
    image_processing::GpuContext,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    run(args)
}

fn run(args: Vec<String>) -> Result<()> {
    if args.first().map(String::as_str) == Some("consistency") {
        ensure!(args.len() == 2, "consistency PHOTO");
        return consistency(&args[1]);
    }
    if args.first().map(String::as_str) == Some("self-test") {
        ensure!(args.len() == 2, "self-test NEW_DIRECTORY");
        return self_test(PathBuf::from(&args[1]));
    }
    if args.first().map(String::as_str) == Some("hash") {
        ensure!(args.len() == 2, "hash FILE");
        println!("{}", blake3::hash(&std::fs::read(&args[1])?).to_hex());
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("pin") {
        ensure!(
            args.len() == 4,
            "pin ASSET_DIRECTORY INPUT_CUBE_OR_none OUTPUT_CUBE_OR_none"
        );
        let state = AppState::default();
        *state.v3_asset_dir.lock().unwrap() = Some(PathBuf::from(&args[1]));
        let path = |s: &str| {
            if s == "none" {
                None
            } else {
                Some(PathBuf::from(s))
            }
        };
        *state.input_transform.lock().unwrap() = path(&args[2]);
        *state.output_transform.lock().unwrap() = path(&args[3]);
        let pinned = identity::pin(&state, &serde_json::json!({}))?;
        println!("{}", serde_json::to_string_pretty(&pinned)?);
        return Ok(());
    }
    ensure!(
        (args.len() == 2 && args[0] == "check") || (args.len() == 3 && args[0] == "render"),
        "Use: check PACKAGE.json | render PACKAGE.json NEW_OUTPUT_DIRECTORY | hash FILE | pin ASSET_DIRECTORY INPUT_OR_none OUTPUT_OR_none"
    );
    let manifest = PathBuf::from(&args[1]).canonicalize()?;
    let root = manifest.parent().context("Manifest has no directory")?;
    let bytes = std::fs::read(&manifest)?;
    let package: Package = serde_json::from_slice(&bytes)?;
    let mut report = package.validate(root)?;
    report["package_hash"] = serde_json::json!(blake3::hash(&bytes).to_hex().to_string());
    if args[0] == "check" {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir(&output)
        .context("Use a NEW output directory; existing results are never overwritten")?;
    // Preserve the exact input description, including Resolve settings and hashes.
    std::fs::write(output.join("package.json"), &bytes)?;
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    report["gpu"] = serde_json::to_value(format!("{:?}", adapter.get_info()))?;
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
    let state = package.state(root);
    let mut results = vec![];
    for (index, case) in package.cases.iter().enumerate() {
        let source = package
            .sources
            .iter()
            .find(|s| s.id == case.source)
            .context("Missing source")?;
        // Recheck at point of use, not just at package intake.
        source.file.read(root)?;
        let path = root.join(&source.file.path);
        let edits = package.edits(case);
        let rendered = application::render_file(
            &context,
            &state,
            path.to_str().context("Non-UTF8 source")?,
            &edits,
            None,
        )?;
        let theirs = reference::reference_pixels(case, root)?;
        let ours = reference::match_size(&rendered.encoded_srgb, &theirs);
        let delta = reference::difference(&reference::decode_output(&ours), &theirs)?;
        let filename = format!("{index:04}.png");
        rendered.write_srgb_png(std::fs::File::create(output.join(&filename))?, true)?;
        results.push(serde_json::json!({"case":case.id,"control":case.control,"resolve_value":case.resolve_value,
            "edits":edits,"render":filename,"difference":delta}));
    }
    report["measurements"] = serde_json::to_value(results)?;
    report["status"] = serde_json::json!("measured_not_calibrated");
    serde_json::to_writer_pretty(std::fs::File::create(output.join("report.json"))?, &report)?;
    println!(
        "Measured {} cases. No slider fitting performed. Results: {}",
        package.cases.len(),
        output.display()
    );
    Ok(())
}

fn self_test(root: PathBuf) -> Result<()> {
    std::fs::create_dir(&root).context("Self-test needs a new directory")?;
    let source = root.join("source.png");
    image::ImageBuffer::from_fn(64, 32, |x, y| {
        image::Rgba([(x * 900) as u16, (y * 1800) as u16, 24000u16, 65535u16])
    })
    .save(&source)?;
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
    let state = AppState::default();
    let mut cases = vec![];
    for (control, value) in [("neutral", 0.), ("shadows", 50.)] {
        let edits =
            serde_json::json!({"processVersion":3,"toneMapper":"resolve","shadows":value,"v3":{}});
        let frame =
            application::render_file(&context, &state, source.to_str().unwrap(), &edits, None)?;
        let path = root.join(format!("{control}.png"));
        frame.write_srgb_png(std::fs::File::create(&path)?, true)?;
        cases.push(serde_json::json!({"id":control,"source":"synthetic","control":control,"resolve_value":value,
            "untagged_srgb":false,"edits":edits,"reference":{"path":path.file_name().unwrap().to_str().unwrap(),"blake3":blake3::hash(&std::fs::read(&path)?).to_hex().to_string()}}));
    }
    let package = serde_json::json!({"schema":1,"domain":"display_rgb",
        "resolve":{"version":"synthetic","page_and_tool":"self-test","input_color_space":"sRGB","timeline_color_space":"DWG Intermediate","output_color_space":"sRGB","data_levels":"full","export_bit_depth":16,"notes":"Generated by RapidRAW, NOT a Resolve comparison"},
        "pipeline":identity::pin(&state,&serde_json::json!({}))?,"asset_directory":"assets",
        "sources":[{"id":"synthetic","file":{"path":"source.png","blake3":blake3::hash(&std::fs::read(source)?).to_hex().to_string()}}],"cases":cases});
    let manifest = root.join("package.json");
    serde_json::to_writer_pretty(std::fs::File::create(&manifest)?, &package)?;
    run(vec![
        "render".into(),
        manifest.to_string_lossy().into_owned(),
        root.join("results").to_string_lossy().into_owned(),
    ])?;
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("results/report.json"))?)?;
    for case in report["measurements"].as_array().unwrap() {
        ensure!(
            case["difference"]["max_linear_rgb"].as_f64().unwrap() < 0.0001,
            "16-bit reference roundtrip regression"
        );
    }
    println!(
        "Self-test passed: exact application path -> profiled 16-bit export -> validated package -> comparison."
    );
    Ok(())
}

/// Real-photo regression probe; never writes the source or its sidecar.
fn consistency(path: &str) -> Result<()> {
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
    let edits = serde_json::json!({"processVersion":3,"toneMapper":"resolve","shadows":30,"highlights":-25,
        "v3":{"detail":{"clarity":10,"texture":10}}});
    let state = AppState::default();
    let full = application::render_file(&context, &state, path, &edits, None)?;
    let cached = application::render_file(&context, &state, path, &edits, None)?;
    ensure!(
        full.encoded_srgb == cached.encoded_srgb,
        "Full-resolution cache changed pixels"
    );
    drop(cached);
    let preview = application::render_file(&context, &state, path, &edits, Some(1024))?;
    drop(state);
    let fresh = application::render_file(&context, &AppState::default(), path, &edits, None)?;
    ensure!(
        full.encoded_srgb == fresh.encoded_srgb,
        "Fresh full-resolution render changed pixels"
    );
    drop(fresh);
    let down = rapidraw_lib::image_processing::downscale_f32_image(
        &image::DynamicImage::ImageRgba32F(full.encoded_srgb),
        1024,
        1024,
    )
    .to_rgba32f();
    // Encoded-sRGB channel MAE is used only as a preview regression tolerance.
    // This is not a perceptual score or a Resolve-match claim.
    let delta = reference::difference(&preview.encoded_srgb, &down)?;
    println!(
        "{}",
        serde_json::json!({"source":path,"cache_exact":true,"fresh_exact":true,
        "preview_encoded_channel_mae":delta.mean_linear_rgb,"preview_encoded_channel_max":delta.max_linear_rgb})
    );
    ensure!(
        delta.mean_linear_rgb < 0.02,
        "Preview/export average error exceeded the real-photo regression tolerance"
    );
    Ok(())
}
