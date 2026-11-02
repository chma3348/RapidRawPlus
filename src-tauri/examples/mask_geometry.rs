//! Colour, luminance, clipped and AI masks on a cropped, rotated and flipped
//! photo, rendered as the app renders them (installed transforms): the mask
//! must land on the same part of the picture as it does on the photo with no
//! geometry, in the preview, a zoomed tile and the full-size export. Also
//! times the range-mask sampling cache.
//!
//!   cargo run --example mask_geometry -- check [PHOTO]
//!   cargo run --example mask_geometry -- timing
use anyhow::{Result, ensure};
use base64::Engine as _;
use darkroom_index::color_engine::application::{render_for_output, render_region_for_output};
use darkroom_index::image_processing::GpuContext;
use image::{DynamicImage, GrayImage, Luma};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn context() -> Result<GpuContext> {
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

/// Chris's configuration: the installed Resolve input and output cubes.
fn app_state() -> Result<darkroom_index::AppState> {
    let state = darkroom_index::AppState::default();
    let support = std::path::PathBuf::from(std::env::var("HOME")?)
        .join("Library/Application Support/io.github.chma3348.DarkroomIndex");
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

fn photos() -> Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from(std::env::var("HOME")?).join("Desktop/Test Photos"))
}

fn mask(sub: Value) -> Value {
    json!([{
        "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100,
        "adjustments": {"exposure": -1.5},
        "subMasks": [sub],
    }])
}

fn geometry_of(geometry: &Value) -> Value {
    json!({
        "rotation": geometry["rotation"],
        "flipHorizontal": geometry["flipHorizontal"],
        "flipVertical": geometry["flipVertical"],
        "orientationSteps": geometry["orientationSteps"],
    })
}

/// Where the mask darkened the picture: |with − without|, summed over the
/// channels, above a fixed amount (the same for every render compared).
fn effect(with: &image::Rgba32FImage, without: &image::Rgba32FImage) -> GrayImage {
    GrayImage::from_fn(with.width(), with.height(), |x, y| {
        let (a, b) = (with.get_pixel(x, y), without.get_pixel(x, y));
        let d: f32 = (0..3).map(|c| (a[c] - b[c]).abs()).sum();
        Luma([if d > 0.06 { 255 } else { 0 }])
    })
}

/// How well two maps agree at a coarser scale, where a fine-grained
/// selection resampled to another size still compares: the share of each
/// `block`-pixel square selected, as a soft intersection over union.
fn agreement(a: &GrayImage, b: &GrayImage, block: u32) -> f64 {
    let (w, h) = a.dimensions();
    let (mut both, mut either) = (0f64, 0f64);
    for by in (0..h).step_by(block as usize) {
        for bx in (0..w).step_by(block as usize) {
            let (mut na, mut nb, mut n) = (0f64, 0f64, 0f64);
            for y in by..(by + block).min(h) {
                for x in bx..(bx + block).min(w) {
                    na += (a.get_pixel(x, y)[0] > 127) as u8 as f64;
                    nb += (b.get_pixel(x, y)[0] > 127) as u8 as f64;
                    n += 1.;
                }
            }
            both += (na / n).min(nb / n);
            either += (na / n).max(nb / n);
        }
    }
    both / either.max(1e-9)
}

/// Intersection over union of two binary maps, and the share of pixels each
/// covers.
fn iou(a: &GrayImage, b: &GrayImage) -> (f64, f64, f64) {
    let (mut both, mut either, mut na, mut nb) = (0u64, 0u64, 0u64, 0u64);
    for (p, q) in a.pixels().zip(b.pixels()) {
        let (x, y) = (p[0] > 127, q[0] > 127);
        both += (x && y) as u64;
        either += (x || y) as u64;
        na += x as u64;
        nb += y as u64;
    }
    let n = (a.width() * a.height()) as f64;
    (
        both as f64 / either.max(1) as f64,
        na as f64 / n,
        nb as f64 / n,
    )
}

/// A source-frame map carried through the photo's geometry, as the
/// pipeline carries the picture.
fn carry(map: &GrayImage, geometry: &Value) -> GrayImage {
    let (out, _) =
        darkroom_index::apply_all_transformations(DynamicImage::ImageLuma8(map.clone()), geometry);
    out.to_luma8()
}

/// Where a source pixel lands in the uncropped, oriented frame the editor
/// takes clicks in: found by carrying a marker through the pipeline.
fn clicked(source: (u32, u32), size: (u32, u32), geometry: &Value) -> (f64, f64) {
    let mut marker = GrayImage::new(size.0, size.1);
    for dy in 0..3 {
        for dx in 0..3 {
            marker.put_pixel(source.0 + dx - 1, source.1 + dy - 1, Luma([255]));
        }
    }
    let mut uncropped = geometry.clone();
    uncropped["crop"] = Value::Null;
    let moved = carry(&marker, &uncropped);
    let (mut sx, mut sy, mut sw) = (0f64, 0f64, 0f64);
    for (x, y, p) in moved.enumerate_pixels() {
        let w = p[0] as f64;
        sx += x as f64 * w;
        sy += y as f64 * w;
        sw += w;
    }
    (sx / sw, sy / sw)
}

fn check(path: &str) -> Result<()> {
    let context = context()?;
    let state = app_state()?;
    let base = json!({"processVersion": 3});
    let full = render_for_output(&context, &state, path, &base, None)?;
    let (w, h) = full.encoded_srgb.dimensions();
    println!("{path}: {w}x{h}");

    // A saturated, even patch near the middle to click on.
    let px = |x: u32, y: u32| full.encoded_srgb.get_pixel(x, y).0;
    let sat = |p: [f32; 4]| {
        let max = p[0].max(p[1]).max(p[2]);
        let min = p[0].min(p[1]).min(p[2]);
        if max > 0.05 { (max - min) / max } else { 0. }
    };
    let mut best = (0f32, (w / 2, h / 2));
    for y in (h * 3 / 10..h * 7 / 10).step_by(37) {
        for x in (w * 3 / 10..w * 7 / 10).step_by(37) {
            let c = px(x, y);
            let even = [(-6i32, 0i32), (6, 0), (0, -6), (0, 6)]
                .iter()
                .all(|(dx, dy)| {
                    let n = px((x as i32 + dx) as u32, (y as i32 + dy) as u32);
                    (0..3).all(|k| (n[k] - c[k]).abs() < 0.03)
                });
            if even && sat(c) > best.0 {
                best = (sat(c), (x, y));
            }
        }
    }
    let target = best.1;
    println!("target {target:?} saturation {:.2}", best.0);
    // A white threshold that selects about the brightest 4%.
    let mut mins: Vec<f32> = full
        .encoded_srgb
        .pixels()
        .step_by(97)
        .map(|p| p[0].min(p[1]).min(p[2]))
        .collect();
    mins.sort_by(f32::total_cmp);
    let white = (mins[mins.len() * 96 / 100] * 100.).clamp(55., 99.);
    // An AI matte (source frame): a rectangle off-centre.
    let mut matte = GrayImage::new(w, h);
    for y in h / 5..h / 2 {
        for x in w / 6..w / 2 {
            matte.put_pixel(x, y, Luma([255]));
        }
    }
    let mut png = Vec::new();
    DynamicImage::ImageLuma8(matte)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    let matte_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );

    let identity = json!({"rotation": 0, "flipHorizontal": false, "flipVertical": false, "orientationSteps": 0});
    // Uncropped dimensions after a quarter turn.
    let (uw, uh) = (h as f64, w as f64);
    let crop = json!({"x": (uw * 0.12).round(), "y": (uh * 0.09).round(),
        "width": (uw * 0.7).round(), "height": (uh * 0.75).round(), "unit": "px"});
    let geometries = [
        (
            "crop only",
            json!({"crop": {"x": (w as f64 * 0.2).round(), "y": (h as f64 * 0.1).round(),
            "width": (w as f64 * 0.7).round(), "height": (h as f64 * 0.8).round(), "unit": "px"},
            "rotation": 0, "flipHorizontal": false, "flipVertical": false, "orientationSteps": 0}),
        ),
        (
            "crop + 90° + flip H",
            json!({"crop": crop, "rotation": 0,
            "flipHorizontal": true, "flipVertical": false, "orientationSteps": 1}),
        ),
        (
            "crop + 90° + flip H + 4° straighten",
            json!({"crop": crop, "rotation": 4.0,
            "flipHorizontal": true, "flipVertical": false, "orientationSteps": 1}),
        ),
        (
            "crop + 270° + flip V",
            json!({"crop": crop, "rotation": 0,
            "flipHorizontal": false, "flipVertical": true, "orientationSteps": 3}),
        ),
    ];

    let with = |geometry: &Value, masks: Value| {
        let mut e = base.clone();
        for (k, v) in geometry.as_object().unwrap() {
            e[k] = v.clone();
        }
        e["masks"] = masks;
        e
    };
    let colour = |at: (f64, f64), recorded: &Value| {
        let mut p = json!({"targetX": at.0, "targetY": at.1, "samples": [{"x": at.0, "y": at.1}],
            "tolerance": 8, "feather": 0, "grow": 0});
        for (k, v) in geometry_of(recorded).as_object().unwrap() {
            p[k] = v.clone();
        }
        json!({"id": "c", "type": "color", "visible": true, "mode": "additive", "parameters": p})
    };
    let luminance = |at: (f64, f64), recorded: &Value| {
        let mut p =
            json!({"targetX": at.0, "targetY": at.1, "tolerance": 12, "feather": 0, "grow": 0});
        for (k, v) in geometry_of(recorded).as_object().unwrap() {
            p[k] = v.clone();
        }
        json!({"id": "l", "type": "luminance", "visible": true, "mode": "additive", "parameters": p})
    };
    let clipped = |recorded: &Value| {
        let mut p = json!({"whiteThreshold": white, "blackThreshold": 0, "feather": 0, "grow": 0, "clean": 0});
        for (k, v) in geometry_of(recorded).as_object().unwrap() {
            p[k] = v.clone();
        }
        json!({"id": "k", "type": "clipped", "visible": true, "mode": "additive", "parameters": p})
    };
    let ai = |recorded: &Value| {
        let mut p =
            json!({"startX": 0, "startY": 0, "endX": 1, "endY": 1, "maskDataBase64": matte_url});
        for (k, v) in geometry_of(recorded).as_object().unwrap() {
            p[k] = v.clone();
        }
        json!({"id": "a", "type": "ai-subject", "visible": true, "mode": "additive", "parameters": p})
    };

    // References: each mask on the photo with no geometry, in the source frame.
    let at_identity = (target.0 as f64, target.1 as f64);
    let plain = render_for_output(&context, &state, path, &with(&identity, json!([])), None)?;
    let mut references = Vec::new();
    for (name, sub) in [
        ("colour", colour(at_identity, &identity)),
        ("luminance", luminance(at_identity, &identity)),
        ("clipped", clipped(&identity)),
        ("ai-subject", ai(&identity)),
    ] {
        let f = render_for_output(&context, &state, path, &with(&identity, mask(sub)), None)?;
        let map = effect(&f.encoded_srgb, &plain.encoded_srgb);
        let (_, cover, _) = iou(&map, &map);
        println!("reference {name:10} covers {:.1}%", cover * 100.);
        references.push(map);
    }

    let mut failed = false;
    for (label, geometry) in &geometries {
        println!("-- {label}");
        let at = clicked(target, (w, h), geometry);
        let no_mask = with(geometry, json!([]));
        let full_plain = render_for_output(&context, &state, path, &no_mask, None)?;
        let preview_plain = render_for_output(&context, &state, path, &no_mask, Some(1600))?;
        let (fw, fh) = full_plain.encoded_srgb.dimensions();
        let roi = (0.25f32, 0.3f32, 0.4f32, 0.35f32);
        let tile_plain =
            render_region_for_output(&context, &state, path, &no_mask, Some(fw.max(fh)), roi)?;
        let cases = [
            ("colour", colour(at, geometry), 0),
            ("luminance", luminance(at, geometry), 1),
            ("clipped", clipped(geometry), 2),
            ("ai-subject", ai(geometry), 3),
            // Made before the photo was turned: recorded with no geometry.
            (
                "colour, made before turning",
                colour(at_identity, &identity),
                0,
            ),
            ("ai-subject, made before turning", ai(&identity), 3),
        ];
        for (name, sub, reference) in cases {
            let expected = carry(&references[reference], geometry);
            ensure!(expected.dimensions() == (fw, fh), "geometry mismatch");
            let edits = with(geometry, mask(sub));
            let f = render_for_output(&context, &state, path, &edits, None)?;
            let actual = effect(&f.encoded_srgb, &full_plain.encoded_srgb);
            let (_, got, want) = iou(&actual, &expected);
            let full_iou = agreement(&actual, &expected, 8);
            let p = render_for_output(&context, &state, path, &edits, Some(1600))?;
            let (pw, ph) = p.encoded_srgb.dimensions();
            let small =
                image::imageops::resize(&expected, pw, ph, image::imageops::FilterType::Triangle);
            let preview_iou = agreement(
                &effect(&p.encoded_srgb, &preview_plain.encoded_srgb),
                &small,
                4,
            );
            let t =
                render_region_for_output(&context, &state, path, &edits, Some(fw.max(fh)), roi)?;
            let ((rx, ry, rw, rh), _) = t.region.expect("a region render");
            let part = image::imageops::crop_imm(&expected, rx, ry, rw, rh).to_image();
            let tile = effect(&t.encoded_srgb, &tile_plain.encoded_srgb);
            let tile_iou = agreement(&tile, &part, 8);
            // The tile is exactly that part of the full-size render.
            let whole = image::imageops::crop_imm(&actual, rx, ry, rw, rh).to_image();
            let (same, _, _) = iou(&tile, &whole);
            println!(
                "{name:32} full {full_iou:.3}  preview {preview_iou:.3}  tile {tile_iou:.3} (tile = full-size render there: {same:.3})   covers {:.1}%, should cover {:.1}%",
                got * 100.,
                want * 100.
            );
            // A preview compares less closely whatever the geometry: a
            // fine-grained selection made at another size (the same on the
            // photo with no geometry). The tile must be the full-size render.
            failed |= full_iou < 0.9 || preview_iou < 0.75 || same < 0.99;
        }
    }
    ensure!(!failed, "a mask did not land where it should");
    println!("every mask landed where it should");
    Ok(())
}

fn timing() -> Result<()> {
    let context = context()?;
    let state = app_state()?;
    let dir = photos()?;
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    for (photo, target) in [
        ("DSC03520.ARW", (1200.0, 800.0)),
        ("DSC08212.JPG", (3000.0, 2000.0)),
    ] {
        let path = dir.join(photo).to_string_lossy().to_string();
        let side: Value = serde_json::from_str(&std::fs::read_to_string(format!("{path}.dri"))?)?;
        let mut edits = side["adjustments"].clone();
        edits["processVersion"] = json!(3);
        edits["masks"] = json!([{"id":"m","name":"m","visible":true,"invert":false,"opacity":100,
            "adjustments":{"exposure":0.4},
            "subMasks":[{"id":"c","type":"color","visible":true,"mode":"additive",
                "parameters":{"targetX": target.0, "targetY": target.1, "tolerance": 40}}]}]);
        println!("{photo}");
        let t = Instant::now();
        render_for_output(&context, &state, &path, &edits, Some(2048))?;
        println!("  cold with colour mask      {:>6.0} ms", ms(t));
        let mut tick = |label: &str, key: &str, value: Value| -> Result<()> {
            edits[key] = value;
            let t = Instant::now();
            render_for_output(&context, &state, &path, &edits, Some(2048))?;
            println!("  {label:26} {:>6.0} ms", ms(t));
            Ok(())
        };
        for i in 0..3 {
            tick("exposure tick", "exposure", json!(0.1 * (i + 1) as f64))?;
        }
        for i in 0..3 {
            tick("contrastPivot tick", "contrastPivot", json!(40 + i))?;
        }
        for v in [true, false] {
            tick(&format!("showClipping={v}"), "showClipping", json!(v))?;
        }
        for i in 0..2 {
            tick("lutIntensity tick", "lutIntensity", json!(80 + i))?;
        }
        for i in 0..2 {
            tick(
                "exposure tick after",
                "exposure",
                json!(0.5 + 0.1 * i as f64),
            )?;
        }
        // A mask with nothing set renders as no mask at all.
        let mut neutral = edits.clone();
        neutral["masks"][0]["adjustments"] = json!({});
        let mut none = edits.clone();
        none["masks"] = json!([]);
        render_for_output(&context, &state, &path, &neutral, Some(2048))?;
        let t = Instant::now();
        let a = render_for_output(&context, &state, &path, &neutral, Some(2048))?;
        let with_neutral = ms(t);
        render_for_output(&context, &state, &path, &none, Some(2048))?;
        let t = Instant::now();
        let b = render_for_output(&context, &state, &path, &none, Some(2048))?;
        let without = ms(t);
        let diff = a
            .encoded_srgb
            .as_raw()
            .iter()
            .zip(b.encoded_srgb.as_raw())
            .map(|(x, y)| (x - y).abs())
            .fold(0f32, f32::max);
        println!(
            "  neutral mask {with_neutral:.0} ms vs no mask {without:.0} ms, max difference {diff:.5}"
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("timing") => timing(),
        _ => {
            let default = photos()?.join("DSC08265.JPG").to_string_lossy().to_string();
            check(args.get(1).unwrap_or(&default))
        }
    }
}
