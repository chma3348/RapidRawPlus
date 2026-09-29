//! Reference intake, measurement and the search for each control's matching
//! value. Nothing here changes a slider; it says how far each one is from
//! Resolve and what value would have matched.
//!
//!   build ROOT                      package.json from ROOT/{Originals, No edits, ...}
//!   check PACKAGE.json              validate without a GPU
//!   render PACKAGE.json OUT [fit]   full-resolution renders and measurements
//!   sweep PACKAGE.json OUT CONTROL [SOURCE]
//!                                   the app value that best matches each export
//!   hash FILE | pin ASSETS IN OUT | self-test DIR | consistency PHOTO
use anyhow::{Context, Result, ensure};
use rapidraw_lib::{
    AppState,
    color_engine::{
        application, identity,
        reference::{self, Case, Package, Source, class},
    },
    image_processing::GpuContext,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    run(args)
}

fn gpu() -> Result<(GpuContext, String)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let info = format!("{:?}", adapter.get_info());
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))?;
    Ok((
        GpuContext {
            device: Arc::new(device),
            queue: Arc::new(queue),
            limits,
            display: Arc::new(Mutex::new(None)),
        },
        info,
    ))
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

fn load(manifest: &str) -> Result<(Package, PathBuf, Vec<u8>)> {
    let manifest = PathBuf::from(manifest).canonicalize()?;
    let root = manifest
        .parent()
        .context("Manifest has no directory")?
        .to_path_buf();
    let bytes = std::fs::read(&manifest)?;
    Ok((serde_json::from_slice(&bytes)?, root, bytes))
}

fn run(args: Vec<String>) -> Result<()> {
    let first = args.first().map(String::as_str);
    match first {
        Some("consistency") => {
            ensure!(args.len() == 2, "consistency PHOTO");
            return consistency(&args[1]);
        }
        Some("self-test") => {
            ensure!(args.len() == 2, "self-test NEW_DIRECTORY");
            return self_test(PathBuf::from(&args[1]));
        }
        Some("hash") => {
            ensure!(args.len() == 2, "hash FILE");
            println!("{}", blake3::hash(&std::fs::read(&args[1])?).to_hex());
            return Ok(());
        }
        Some("build") => {
            ensure!(args.len() == 2, "build ROOT");
            return build(PathBuf::from(&args[1]).canonicalize()?);
        }
        Some("pin") => {
            ensure!(
                args.len() == 4,
                "pin ASSET_DIRECTORY INPUT_CUBE_OR_none OUTPUT_CUBE_OR_none"
            );
            let state = AppState::default();
            *state.v3_asset_dir.lock().unwrap() = Some(PathBuf::from(&args[1]));
            let path = |s: &str| (s != "none").then(|| PathBuf::from(s));
            *state.input_transform.lock().unwrap() = path(&args[2]);
            *state.output_transform.lock().unwrap() = path(&args[3]);
            let pinned = identity::pin(&state, &json!({}))?;
            println!("{}", serde_json::to_string_pretty(&pinned)?);
            return Ok(());
        }
        Some("sweep") => {
            ensure!(
                args.len() == 4 || args.len() == 5,
                "sweep PACKAGE.json OUT_DIRECTORY CONTROL [SOURCE]"
            );
            let (package, root, _) = load(&args[1])?;
            package.validate(&root)?;
            return sweep(
                &package,
                &root,
                PathBuf::from(&args[2]),
                &args[3],
                args.get(4),
            );
        }
        _ => {}
    }
    ensure!(
        (args.len() == 2 && args[0] == "check")
            || ((args.len() == 3 || args.len() == 4) && args[0] == "render"),
        "Use: build ROOT | check PACKAGE.json | render PACKAGE.json NEW_OUTPUT_DIRECTORY [fit] | sweep PACKAGE.json OUT CONTROL [SOURCE] | hash FILE | pin ASSET_DIRECTORY INPUT_OR_none OUTPUT_OR_none"
    );
    let (package, root, bytes) = load(&args[1])?;
    let mut report = package.validate(&root)?;
    report["package_hash"] = json!(blake3::hash(&bytes).to_hex().to_string());
    if args[0] == "check" {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let fit_only = args.get(3).map(String::as_str) == Some("fit");
    ensure!(
        args.len() == 3 || fit_only,
        "The only render option is `fit`"
    );
    let output = PathBuf::from(&args[2]);
    std::fs::create_dir(&output)
        .context("Use a NEW output directory; existing results are never overwritten")?;
    // Preserve the exact input description, including Resolve settings and hashes.
    std::fs::write(output.join("package.json"), &bytes)?;
    let (context, info) = gpu()?;
    report["gpu"] = json!(info);
    let state = package.state(&root);
    let mut results = vec![];
    let mut groups: BTreeMap<(String, String, String), Vec<reference::Difference>> =
        BTreeMap::new();
    let mut offsets = vec![];
    for (index, case) in package.cases.iter().enumerate() {
        let source = source_of(&package, case)?;
        if fit_only && !source.fit {
            continue;
        }
        // Recheck at point of use, not just at package intake.
        source.file.read(&root)?;
        let path = root.join(&source.file.path);
        let edits = package.edits(case);
        let rendered = application::render_file(
            &context,
            &state,
            path.to_str().context("Non-UTF8 source")?,
            &edits,
            None,
        )?;
        let theirs = reference::reference_pixels(case, &root)?;
        let ours =
            reference::decode_output(&reference::match_size(&rendered.encoded_srgb, &theirs));
        let delta = reference::difference(&ours, &theirs)?;
        let filename = format!("{index:04}.png");
        rendered.write_srgb_png(std::fs::File::create(output.join(&filename))?, true)?;
        let mut entry = json!({"case":case.id,"source":source.id,"class":source.class,"fit":source.fit,
            "control":case.control,"resolve_value":case.resolve_value,
            "edits":edits,"render":filename,"difference":delta});
        if source.class == class::RAW && case.control == "neutral" {
            let offset = reference::exposure_offset(&ours, &theirs)?;
            offsets.push((source.id.clone(), offset.clone()));
            entry["exposure_offset"] = serde_json::to_value(offset)?;
        }
        println!(
            "{:44} {:>11} {:>6} mean {:5.2} p99 {:5.1} bias {:+5.1} {:+5.1} {:+5.1}",
            case.id,
            case.control,
            case.resolve_value,
            delta.mean_levels,
            delta.p99_levels,
            delta.bias_levels[0],
            delta.bias_levels[1],
            delta.bias_levels[2]
        );
        groups
            .entry((
                source.class.clone(),
                case.control.clone(),
                format!("{:+}", case.resolve_value),
            ))
            .or_default()
            .push(delta.clone());
        results.push(entry);
    }
    println!("\nBy class and control (8-bit levels, mean over cases):");
    println!(
        "{:12} {:11} {:>6} {:>3} {:>6} {:>6} {:>18}",
        "class", "control", "value", "n", "mean", "p99", "bias R G B"
    );
    let mut summary = vec![];
    for ((class, control, value), diffs) in &groups {
        let n = diffs.len() as f64;
        let mean = diffs.iter().map(|d| d.mean_levels).sum::<f64>() / n;
        let p99 = diffs.iter().map(|d| d.p99_levels).sum::<f64>() / n;
        let bias: Vec<f64> = (0..3)
            .map(|c| diffs.iter().map(|d| d.bias_levels[c]).sum::<f64>() / n)
            .collect();
        println!(
            "{class:12} {control:11} {value:>6} {:>3} {mean:6.2} {p99:6.1} {:+5.1} {:+5.1} {:+5.1}",
            diffs.len(),
            bias[0],
            bias[1],
            bias[2]
        );
        summary.push(
            json!({"class":class,"control":control,"resolve_value":value,"cases":diffs.len(),
            "mean_levels":mean,"p99_levels":p99,"bias_levels":bias}),
        );
    }
    if !offsets.is_empty() {
        println!(
            "\nRAW neutral development, Resolve relative to ours (stops, + = Resolve brighter):"
        );
        println!(
            "{:12} {:>8} {:>8} {:>10}",
            "source", "shadows", "midtones", "highlights"
        );
        for (id, o) in &offsets {
            println!(
                "{id:12} {:+8.2} {:+8.2} {:+10.2}",
                o.shadows_stops, o.midtones_stops, o.highlights_stops
            );
        }
    }
    report["measurements"] = serde_json::to_value(results)?;
    report["summary"] = json!(summary);
    report["fit_only"] = json!(fit_only);
    report["status"] = json!("measured_not_calibrated");
    serde_json::to_writer_pretty(std::fs::File::create(output.join("report.json"))?, &report)?;
    println!(
        "\nMeasured {} cases. No slider fitting performed. Results: {}",
        report["measurements"].as_array().unwrap().len(),
        output.display()
    );
    Ok(())
}

fn source_of<'a>(package: &'a Package, case: &Case) -> Result<&'a Source> {
    package
        .sources
        .iter()
        .find(|s| s.id == case.source)
        .context("Missing source")
}

/// A package from a folder of Resolve exports: `Originals/` holds the
/// sources, every other folder is one control's exports named as
/// `parse_export_name` reads them. Sources are classed by how the app
/// decodes them, with the classes whose neutral does not yet agree with
/// Resolve held out of fitting. Pins the installed transforms into
/// `ROOT/assets` and writes `ROOT/package.json`.
fn build(root: PathBuf) -> Result<()> {
    let state = installed_state()?;
    ensure!(
        state.input_transform.lock().unwrap().is_some()
            && state.output_transform.lock().unwrap().is_some(),
        "Both captured transforms must be installed to build a package as the app renders"
    );
    *state.v3_asset_dir.lock().unwrap() = Some(root.join("assets"));
    let pipeline = identity::pin(&state, &json!({}))?;
    let hash = |p: &Path| -> Result<reference::File> {
        Ok(reference::File {
            path: p.strip_prefix(&root)?.to_path_buf(),
            blake3: blake3::hash(&std::fs::read(p)?).to_hex().to_string(),
        })
    };
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join("Originals"))
        .context("ROOT/Originals is missing")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && !p.file_name().unwrap().to_string_lossy().starts_with('.'))
        .collect();
    entries.sort();
    let mut sources = vec![];
    for path in &entries {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let text = path.to_str().context("Non-UTF8 path")?;
        let report =
            application::input_report(&state, text, &reference::edits_for("neutral", 0.)?)?;
        let decision = report["provenance"]["transform_decision"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let (class, fit, note) = if rapidraw_lib::formats::is_raw_file(text) {
            (
                class::RAW,
                false,
                "Held out: Resolve's Camera RAW development differs from ours by 0.3-4.6 stops per file, not a slider matter. Fit once the decode is reconciled.",
            )
        } else if decision.contains("compressed_into_srgb") || decision.contains("p3") {
            (
                class::IPHONE_P3,
                false,
                "Held out: Display P3 photo that Resolve read as sRGB in this batch while the app honours the profile. Re-tag Display P3 in Resolve and install the P3 capture, then re-export.",
            )
        } else if stem.starts_with("chart") {
            (class::CHART, true, "")
        } else {
            (class::SRGB_JPEG, true, "")
        };
        println!("{stem:46} {class:10} fit={fit:5} {decision}");
        sources.push(Source {
            id: stem,
            file: hash(path)?,
            class: class.into(),
            fit,
            note: note.into(),
        });
    }
    ensure!(!sources.is_empty(), "No originals found");
    let mut cases = vec![];
    let mut unmatched = vec![];
    let mut folders: Vec<PathBuf> = std::fs::read_dir(&root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_dir()
                && !matches!(
                    p.file_name().unwrap().to_string_lossy().as_ref(),
                    "Originals" | "assets"
                )
                && !p.file_name().unwrap().to_string_lossy().starts_with('.')
        })
        .collect();
    folders.sort();
    for folder in &folders {
        let folder_name = folder.file_name().unwrap().to_string_lossy().to_string();
        let mut files: Vec<PathBuf> = std::fs::read_dir(folder)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|e| {
                    matches!(
                        e.to_string_lossy().to_ascii_lowercase().as_str(),
                        "tif" | "tiff" | "png"
                    )
                })
            })
            .collect();
        files.sort();
        for file in files {
            let stem = file.file_stem().unwrap().to_string_lossy().to_string();
            let Some((source, control, value)) = reference::parse_export_name(&folder_name, &stem)
            else {
                unmatched.push(file.clone());
                continue;
            };
            if !sources.iter().any(|s| s.id == source) {
                unmatched.push(file.clone());
                continue;
            }
            let id = if control == "neutral" {
                format!("{source}__neutral")
            } else {
                format!("{source}__{control}{value:+}")
            };
            cases.push(Case {
                id,
                source,
                reference: hash(&file)?,
                untagged_srgb: true,
                control: control.clone(),
                resolve_value: value,
                edits: reference::edits_for(&control, reference::app_value(&control, value))?,
            });
        }
    }
    let order = |c: &Case| {
        (
            sources.iter().position(|s| s.id == c.source).unwrap(),
            c.control != "neutral",
            c.control.clone(),
            (c.resolve_value * 1000.) as i64,
        )
    };
    cases.sort_by_key(order);
    let mut per_control: BTreeMap<String, usize> = BTreeMap::new();
    for c in &cases {
        *per_control.entry(c.control.clone()).or_default() += 1;
    }
    let package = Package {
        schema: 1,
        domain: "display_rgb".into(),
        resolve: reference::ResolveSettings {
            version: "DaVinci Resolve 21".into(),
            page_and_tool: "Photo page, Adjustments panel".into(),
            input_color_space: "sRGB for every photo (Display P3 photos were not tagged P3 in this batch)".into(),
            timeline_color_space: "DaVinci WG/Intermediate".into(),
            output_color_space: "sRGB".into(),
            data_levels: "full".into(),
            export_bit_depth: 16,
            notes: "Untagged 16-bit TIFF exports scaled to a 2160 px long edge. One control per export, reset between exports; the value in each file name is the slider reading in Resolve. RAW files developed with Resolve's Camera RAW defaults as set in the project (values to be recorded). Saturation entries are slider readings on the -100..100 scale as typed.".into(),
        },
        pipeline,
        asset_directory: "assets".into(),
        sources,
        cases,
        required_samples: Default::default(),
    };
    let manifest = root.join("package.json");
    if manifest.exists() {
        let previous = root.join("package.previous.json");
        std::fs::rename(&manifest, &previous)?;
        println!("Kept the previous manifest as {}", previous.display());
    }
    serde_json::to_writer_pretty(std::fs::File::create(&manifest)?, &package)?;
    println!(
        "\n{} sources ({} fitting), {} cases: {}",
        package.sources.len(),
        package.sources.iter().filter(|s| s.fit).count(),
        package.cases.len(),
        per_control
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for u in &unmatched {
        println!("Not matched to a source or control: {}", u.display());
    }
    println!("Wrote {}", manifest.display());
    package.validate(&root)?;
    println!("Package validates.");
    Ok(())
}

/// For every export of one control, the app value that best matches it,
/// found by a grid then golden-section search on the mean 8-bit difference.
/// Renders at the reference's size (the app's preview policy) for speed;
/// the full-resolution `render` is the authority once a fit is applied.
/// With the residual at the equal value and at neutral alongside, the table
/// says whether a control is a scale away from Resolve or a different shape.
fn sweep(
    package: &Package,
    root: &Path,
    output: PathBuf,
    control: &str,
    only: Option<&String>,
) -> Result<()> {
    std::fs::create_dir_all(&output)?;
    let (context, _) = gpu()?;
    let state = package.state(root);
    let cases: Vec<&Case> = package
        .cases
        .iter()
        .filter(|c| c.control == control)
        .filter(|c| {
            let s = source_of(package, c).unwrap();
            match only {
                Some(id) => &s.id == id,
                None => s.fit,
            }
        })
        .collect();
    ensure!(
        !cases.is_empty(),
        "No {control} cases for the selected sources"
    );
    println!(
        "{:46} {:>7} {:>8} {:>8} {:>8} {:>8}",
        "case", "resolve", "best app", "at best", "at equal", "neutral"
    );
    let mut rows = vec![];
    for case in cases {
        let source = source_of(package, case)?;
        let path = root.join(&source.file.path);
        let path = path.to_str().context("Non-UTF8 source")?;
        let theirs = reference::reference_pixels(case, root)?;
        let long_edge = theirs.width().max(theirs.height());
        let mut evaluations: BTreeMap<i64, f64> = BTreeMap::new();
        let mut error = |value: f64| -> Result<f64> {
            let key = (value * 100.).round() as i64;
            if let Some(e) = evaluations.get(&key) {
                return Ok(*e);
            }
            let mut edits = reference::edits_for(control, key as f64 / 100.)?;
            edits["v3Pipeline"] = serde_json::to_value(&package.pipeline)?;
            let rendered =
                application::render_file(&context, &state, path, &edits, Some(long_edge))?;
            let ours =
                reference::decode_output(&reference::match_size(&rendered.encoded_srgb, &theirs));
            let e = reference::difference(&ours, &theirs)?.mean_levels;
            evaluations.insert(key, e);
            Ok(e)
        };
        let (lo, hi) = reference::app_range(control);
        let grid: Vec<(f64, f64)> = (0..=8)
            .map(|k| {
                let v = lo + (hi - lo) * k as f64 / 8.;
                error(v).map(|e| (v, e))
            })
            .collect::<Result<_>>()?;
        let best = (0..grid.len())
            .min_by(|&a, &b| grid[a].1.total_cmp(&grid[b].1))
            .unwrap();
        let (mut a, mut b) = (
            grid[best.saturating_sub(1)].0,
            grid[(best + 1).min(grid.len() - 1)].0,
        );
        let phi = 0.5 * (5f64.sqrt() - 1.);
        let (mut c, mut d) = (b - phi * (b - a), a + phi * (b - a));
        let (mut fc, mut fd) = (error(c)?, error(d)?);
        for _ in 0..12 {
            if fc < fd {
                b = d;
                d = c;
                fd = fc;
                c = b - phi * (b - a);
                fc = error(c)?;
            } else {
                a = c;
                c = d;
                fc = fd;
                d = a + phi * (b - a);
                fd = error(d)?;
            }
        }
        let at_equal = error(reference::app_value(control, case.resolve_value).clamp(lo, hi))?;
        let at_neutral = error(0.)?;
        let (best_value, best_error) = evaluations
            .iter()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, e)| (*k as f64 / 100., *e))
            .unwrap();
        println!(
            "{:46} {:>7} {:>8.1} {:>8.2} {:>8.2} {:>8.2}",
            case.id, case.resolve_value, best_value, best_error, at_equal, at_neutral
        );
        rows.push(json!({"case":case.id,"source":source.id,"class":source.class,"control":control,
            "resolve_value":case.resolve_value,"best_app_value":best_value,"mean_levels_at_best":best_error,
            "mean_levels_at_equal":at_equal,"mean_levels_at_neutral":at_neutral,
            "evaluations":evaluations.iter().map(|(k,e)| json!([*k as f64/100.,e])).collect::<Vec<_>>()}));
    }
    let file = output.join(format!("sweep-{control}.json"));
    serde_json::to_writer_pretty(
        std::fs::File::create(&file)?,
        &json!({"control":control,"render":"reference size (preview policy)","rows":rows}),
    )?;
    println!("Wrote {}", file.display());
    Ok(())
}

fn self_test(root: PathBuf) -> Result<()> {
    std::fs::create_dir(&root).context("Self-test needs a new directory")?;
    let source = root.join("source.png");
    image::ImageBuffer::from_fn(64, 32, |x, y| {
        image::Rgba([(x * 900) as u16, (y * 1800) as u16, 24000u16, 65535u16])
    })
    .save(&source)?;
    let (context, _) = gpu()?;
    let state = AppState::default();
    let mut cases = vec![];
    for (control, value) in [("neutral", 0.), ("shadows", 50.)] {
        let edits = json!({"processVersion":3,"toneMapper":"resolve","shadows":value,"v3":{}});
        let frame =
            application::render_file(&context, &state, source.to_str().unwrap(), &edits, None)?;
        let path = root.join(format!("{control}.png"));
        frame.write_srgb_png(std::fs::File::create(&path)?, true)?;
        cases.push(json!({"id":control,"source":"synthetic","control":control,"resolve_value":value,
            "untagged_srgb":false,"edits":edits,"reference":{"path":path.file_name().unwrap().to_str().unwrap(),"blake3":blake3::hash(&std::fs::read(&path)?).to_hex().to_string()}}));
    }
    let package = json!({"schema":1,"domain":"display_rgb",
        "resolve":{"version":"synthetic","page_and_tool":"self-test","input_color_space":"sRGB","timeline_color_space":"DWG Intermediate","output_color_space":"sRGB","data_levels":"full","export_bit_depth":16,"notes":"Generated by RapidRAW, NOT a Resolve comparison"},
        "pipeline":identity::pin(&state,&json!({}))?,"asset_directory":"assets",
        "sources":[{"id":"synthetic","class":"chart","file":{"path":"source.png","blake3":blake3::hash(&std::fs::read(source)?).to_hex().to_string()}}],"cases":cases});
    let manifest = root.join("package.json");
    serde_json::to_writer_pretty(std::fs::File::create(&manifest)?, &package)?;
    run(vec![
        "render".into(),
        manifest.to_string_lossy().into_owned(),
        root.join("results").to_string_lossy().into_owned(),
    ])?;
    let report: Value = serde_json::from_slice(&std::fs::read(root.join("results/report.json"))?)?;
    for case in report["measurements"].as_array().unwrap() {
        ensure!(
            case["difference"]["max_linear_rgb"].as_f64().unwrap() < 0.0001,
            "16-bit reference roundtrip regression"
        );
    }
    // The search must find the value the reference was made with.
    run(vec![
        "sweep".into(),
        manifest.to_string_lossy().into_owned(),
        root.join("sweep").to_string_lossy().into_owned(),
        "shadows".into(),
    ])?;
    let sweep: Value =
        serde_json::from_slice(&std::fs::read(root.join("sweep/sweep-shadows.json"))?)?;
    let best = sweep["rows"][0]["best_app_value"].as_f64().unwrap();
    ensure!(
        (best - 50.).abs() < 1.,
        "The sweep found {best} for a reference made at 50"
    );
    println!(
        "Self-test passed: exact application path -> profiled 16-bit export -> validated package -> comparison -> sweep recovers the value."
    );
    Ok(())
}

/// Real-photo regression probe; never writes the source or its sidecar.
fn consistency(path: &str) -> Result<()> {
    let (context, _) = gpu()?;
    let edits = json!({"processVersion":3,"toneMapper":"resolve","shadows":30,"highlights":-25,
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
        json!({"source":path,"cache_exact":true,"fresh_exact":true,
        "preview_encoded_channel_mae":delta.mean_linear_rgb,"preview_encoded_channel_max":delta.max_linear_rgb})
    );
    ensure!(
        delta.mean_linear_rgb < 0.02,
        "Preview/export average error exceeded the real-photo regression tolerance"
    );
    Ok(())
}
