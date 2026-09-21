use image::{ImageBuffer, Rgba};
use rapidraw_lib::color_engine::{ColorEngine, config::*, plan::RenderPlan, spaces};
use rapidraw_lib::image_processing::GpuContext;
use std::sync::{Arc, Mutex};

fn config() -> PipelineConfig {
    PipelineConfig {
        controls: Default::default(),
        process_version: 3,
        source: SourceColor {
            primaries: Primaries::Srgb,
            transfer: Transfer::Srgb,
            reference: ReferenceDomain::Display,
        },
        working_space: Primaries::DavinciWideGamut,
        output_lut: None,
        output_rendering: OutputRendering::DisplayPassthroughV1,
    }
}

#[test]
fn configuration_is_explicit_and_versioned() {
    for v in 0..=2 {
        assert_eq!(engine_for_version(v).unwrap(), EngineVersion::Legacy);
    }
    assert_eq!(
        engine_for_version(3).unwrap(),
        EngineVersion::ExperimentalV3
    );
    assert!(engine_for_version(4).is_err());
    let c = config();
    let mut json = serde_json::to_value(&c).unwrap();
    assert_eq!(
        serde_json::from_value::<PipelineConfig>(json.clone()).unwrap(),
        c
    );
    json["highlights"] = 30.into();
    assert!(serde_json::from_value::<PipelineConfig>(json).is_err());
    for invalid in [f32::NAN, f32::INFINITY, 21.0] {
        let mut c = config();
        c.controls.exposure = invalid;
        assert!(RenderPlan::build(c).is_err());
    }
    let mut c = config();
    c.output_rendering = OutputRendering::SceneShoulderV1;
    assert!(RenderPlan::build(c).is_err());
    let p = RenderPlan::build(config()).unwrap();
    assert_eq!(
        p.fingerprint("content-A"),
        RenderPlan::build(config())
            .unwrap()
            .fingerprint("content-A")
    );
    assert_ne!(p.fingerprint("content-A"), p.fingerprint("content-B"));
    let mut c = config();
    c.controls.exposure = 1.0;
    assert_ne!(
        p.fingerprint("content-A"),
        RenderPlan::build(c).unwrap().fingerprint("content-A")
    );
}

#[test]
fn published_space_and_transfer_contracts() {
    let xyz = spaces::rgb_to_xyz(Primaries::DavinciWideGamut) * glam::DVec3::X;
    assert!((xyz - glam::DVec3::new(0.70062239, 0.27411851, -0.09896291)).length() < 1e-12);
    let forward = spaces::conversion(Primaries::Srgb, Primaries::DavinciWideGamut);
    let inverse = spaces::conversion(Primaries::DavinciWideGamut, Primaries::Srgb);
    assert!((forward * glam::DVec3::ONE - glam::DVec3::ONE).length() < 1e-7);
    let signed_hdr = glam::DVec3::new(-0.2, 0.18, 12.0);
    assert!((inverse * forward * signed_hdr - signed_hdr).length() < 1e-12);
    for (linear, encoded) in [
        (-0.01, -0.104443),
        (0.0, 0.0),
        (0.18, 0.336043),
        (1.0, 0.513837),
        (10.0, 0.756599),
        (100.0, 1.0),
    ] {
        assert!((spaces::encode_intermediate(linear) - encoded).abs() < 1e-6);
        assert!(
            (spaces::decode(
                spaces::encode_intermediate(linear),
                Transfer::DavinciIntermediate
            ) - linear)
                .abs()
                < 1e-9
        );
    }
}

/// Oklab is defined on XYZ, so composing it with the working primaries and
/// routing through sRGB first are the same transform. Pinning that is what
/// lets the shader drop the detour without changing a pixel.
#[test]
fn spaces_compose_to_the_same_oklab() {
    let direct = spaces::lms_from_rgb(Primaries::DavinciWideGamut);
    let detour = spaces::lms_from_rgb(Primaries::Srgb)
        * spaces::conversion(Primaries::DavinciWideGamut, Primaries::Srgb);
    for c in 0..3 {
        assert!(
            (direct.col(c) - detour.col(c)).length() < 1e-12,
            "composed Oklab differs from the sRGB route: {direct} vs {detour}"
        );
    }
    // Oklab's own published constants are not self-consistent: the XYZ route
    // and the sRGB route differ in the fourth decimal. Record the size of that
    // gap so a future change to either constant is a deliberate one.
    let via_xyz = spaces::lms_from_rgb_via_xyz(Primaries::Srgb);
    let gap = (via_xyz - spaces::lms_from_rgb(Primaries::Srgb))
        .to_cols_array()
        .iter()
        .fold(0.0f64, |m, v| m.max(v.abs()));
    assert!(
        (1e-5..1e-3).contains(&gap),
        "published Oklab constants now disagree by {gap}"
    );
    assert!(
        (spaces::rgb_from_lms(Primaries::DavinciWideGamut) * direct - glam::DMat3::IDENTITY)
            .to_cols_array()
            .iter()
            .all(|v| v.abs() < 1e-9)
    );
}

#[test]
fn creative_controls_are_strict_and_part_of_saved_identity() {
    use rapidraw_lib::color_engine::{application::controls, controls::Controls};
    use serde_json::json;
    assert_eq!(controls(&json!({"v3":{}})).unwrap(), Controls::default());
    for invalid in [
        json!({"revision":2}),
        json!({"exposure":11}),
        json!({"saturation":101}),
        json!({"typo":1}),
        json!({"bands":[[0,0,0]]}),
    ] {
        assert!(controls(&json!({"v3":invalid})).is_err());
    }
    let mut edited = config();
    edited.controls.bands[0] = [30., -20., 10.];
    edited.controls.grading[2] = [240., 25., 0.];
    let serialized = serde_json::to_string(&edited).unwrap();
    let reopened: PipelineConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(edited, reopened);
    assert_eq!(
        RenderPlan::build(edited).unwrap().fingerprint("source"),
        RenderPlan::build(reopened).unwrap().fingerprint("source")
    );
    let mut c = config();
    c.controls.hue = f32::NAN;
    assert!(RenderPlan::build(c).is_err());
}

#[test]
fn gpu_color_pipeline_contracts() {
    // Deliberately fail rather than silently skip on hosts without a GPU.
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
        .expect("GPU required for v3 contracts");
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .unwrap();
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
    };
    let engine = ColorEngine::new(context.clone()).unwrap();
    let plan = RenderPlan::build(config()).unwrap();
    // Cross the 65536-pixel chunk boundary, with distinct RGB and straight alpha.
    let input = ImageBuffer::from_fn(257, 257, |x, y| {
        Rgba([
            x as f32 / 256.0,
            y as f32 / 256.0,
            ((x + y) % 257) as f32 / 256.0,
            0.64,
        ])
    });
    let result = engine.render(&input, &plan, true).unwrap();
    for (a, b) in input.as_raw().iter().zip(result.encoded_srgb.as_raw()) {
        assert!((a - b).abs() < 3e-6, "neutral changed {a} to {b}");
    }
    let uncaptured = engine.render(&input, &plan, false).unwrap();
    assert!(uncaptured.stages.is_none());
    assert_eq!(uncaptured.encoded_srgb, result.encoded_srgb);
    // Chunk boundaries must not change a pixel. The production chunk holds a
    // whole preview, so force boundaries — including awkward, non-row-aligned
    // ones — with engines that use small chunks, in every readback mode.
    let mut graded_plan_config = config();
    graded_plan_config.controls.exposure = 0.7;
    graded_plan_config.controls.saturation = 30.0;
    // Vignette and grain depend on where a pixel is, so they are what proves
    // each chunk knows its true position in the frame.
    graded_plan_config.controls.effects.vignette_amount = -60.0;
    graded_plan_config.controls.effects.grain_amount = 60.0;
    let mut graded_plan = RenderPlan::build(graded_plan_config).unwrap();
    graded_plan.set_render_scale(0.5);
    let reference = engine.render(&input, &graded_plan, true).unwrap();
    let reference_graded = engine.render_graded(&input, &graded_plan).unwrap();
    for pixels in [1000, 4096, 65536] {
        let small = ColorEngine::new(context.clone())
            .unwrap()
            .with_chunk_pixels(pixels);
        let chunked = small.render(&input, &graded_plan, true).unwrap();
        assert_eq!(
            chunked.encoded_srgb, reference.encoded_srgb,
            "{pixels}-pixel chunks changed output"
        );
        assert_eq!(
            chunked.stages.as_ref().unwrap().graded,
            reference.stages.as_ref().unwrap().graded,
            "{pixels}-pixel chunks changed the graded stage"
        );
        assert_eq!(
            small.render_graded(&input, &graded_plan).unwrap(),
            reference_graded,
            "{pixels}-pixel chunks changed graded-only readback"
        );
    }
    assert_eq!(
        reference_graded,
        reference.stages.unwrap().graded,
        "graded-only readback disagrees with the full capture"
    );
    let preview = result.preview_rgba8();
    let export = result.export_rgba16().into_rgba16();
    for (a, b) in preview.as_raw().iter().zip(export.as_raw()) {
        assert!((*a as f32 - *b as f32 / 257.0).abs() <= 0.501);
    }
    // Eight-bit display encoding: dither must break the plateaus that plain
    // rounding leaves in a slow ramp, without moving the average off the
    // value it is encoding.
    {
        let mut ramp = config();
        ramp.source.transfer = Transfer::Linear;
        ramp.output_rendering = OutputRendering::DisplayGamutV2;
        let shallow = ImageBuffer::from_fn(256, 8, |x, _| {
            let v = 0.2 + x as f32 * (0.01 / 256.0);
            Rgba([v, v, v, 1.0])
        });
        let frame = engine
            .render(&shallow, &RenderPlan::build(ramp).unwrap(), false)
            .unwrap();
        let run = |image: &image::RgbaImage| {
            let mut longest = 1;
            let mut current = 1;
            let row: Vec<u8> = (0..image.width())
                .map(|x| image.get_pixel(x, 0)[0])
                .collect();
            for pair in row.windows(2) {
                current = if pair[0] == pair[1] { current + 1 } else { 1 };
                longest = longest.max(current);
            }
            longest
        };
        let plain = frame.preview_rgba8();
        let dithered = frame.display_rgba8();
        assert!(
            run(&dithered) * 4 < run(&plain),
            "dither left the banding in place: {} vs {}",
            run(&dithered),
            run(&plain)
        );
        let mean = |image: &image::RgbaImage| {
            image.pixels().map(|p| p[0] as f64).sum::<f64>() / image.pixels().len() as f64
        };
        assert!(
            (mean(&dithered) - mean(&plain)).abs() < 0.25,
            "dither shifted the average: {} vs {}",
            mean(&dithered),
            mean(&plain)
        );
        for (a, d) in plain.as_raw().iter().zip(dithered.as_raw()) {
            assert!(
                a.abs_diff(*d) <= 1,
                "dither moved a pixel more than one level: {a} -> {d}"
            );
        }
        assert_eq!(
            dithered,
            frame.display_rgba8(),
            "display dither must be deterministic"
        );
    }
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba16(export.clone())
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let decoded = image::load_from_memory(png.get_ref()).unwrap();
    assert_eq!(decoded.color(), image::ColorType::Rgba16);
    assert_eq!(decoded.into_rgba16(), export);
    let mut tagged = Vec::new();
    result.write_srgb_png(&mut tagged, true).unwrap();
    let roundtrip = rapidraw_lib::color_engine::input::decode_profiled_photo(&tagged).unwrap();
    assert!(roundtrip.provenance.profile_hash.is_some());
    let mut roundtrip_config = config();
    roundtrip_config.source = roundtrip.color;
    let rerendered = engine
        .render(
            &roundtrip.pixels,
            &RenderPlan::build(roundtrip_config).unwrap(),
            false,
        )
        .unwrap();
    for (a, b) in result
        .encoded_srgb
        .as_raw()
        .iter()
        .zip(rerendered.encoded_srgb.as_raw())
    {
        assert!((a - b).abs() < 5e-5, "profile roundtrip mismatch {a} {b}");
    }

    let mut scene = config();
    scene.source.transfer = Transfer::Linear;
    scene.source.reference = ReferenceDomain::Scene;
    scene.output_rendering = OutputRendering::SceneShoulderV1;
    scene.controls.exposure = 1.0;
    let ramp = ImageBuffer::from_fn(1025, 1, |x, _| {
        let v = x as f32 / 128.0;
        Rgba([v, v, v, 1.0])
    });
    let rendered = engine
        .render(&ramp, &RenderPlan::build(scene.clone()).unwrap(), true)
        .unwrap();
    let stages = rendered.stages.as_ref().unwrap();
    for (working, graded) in stages.working.pixels().zip(stages.graded.pixels()) {
        for c in 0..3 {
            assert!(
                (graded[c] - working[c] * 2.0).abs() < 1e-6,
                "exposure not exact: {} -> {} (want {})",
                working[c],
                graded[c],
                working[c] * 2.0
            );
        }
    }
    assert!(stages.graded.get_pixel(1024, 0)[0] > 15.9);
    let mut previous = 0.0;
    for p in rendered.encoded_srgb.pixels() {
        assert!(p[0] + 1e-6 >= previous && (0.0..=1.0).contains(&p[0]));
        previous = p[0];
    }
    // Equivalent scene colors, encoded as DWG/Intermediate versus linear sRGB.
    let samples = ImageBuffer::from_fn(32, 1, |x, _| Rgba([x as f32 / 8.0 - 0.02, 0.18, 0.9, 0.5]));
    let matrix = spaces::conversion(Primaries::Srgb, Primaries::DavinciWideGamut);
    let encoded = ImageBuffer::from_fn(32, 1, |x, y| {
        let p = samples.get_pixel(x, y);
        let v = matrix * glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
        Rgba([
            spaces::encode_intermediate(v.x) as f32,
            spaces::encode_intermediate(v.y) as f32,
            spaces::encode_intermediate(v.z) as f32,
            p[3],
        ])
    });
    let linear = engine
        .render(&samples, &RenderPlan::build(scene.clone()).unwrap(), true)
        .unwrap();
    scene.source.primaries = Primaries::DavinciWideGamut;
    scene.source.transfer = Transfer::DavinciIntermediate;
    let log = engine
        .render(&encoded, &RenderPlan::build(scene).unwrap(), true)
        .unwrap();
    for (a, b) in linear
        .encoded_srgb
        .as_raw()
        .iter()
        .zip(log.encoded_srgb.as_raw())
    {
        assert!((a - b).abs() < 2e-5, "source mismatch {a} {b}");
    }
    let negative = ImageBuffer::from_pixel(1, 1, Rgba([-0.1, -0.1, -0.1, 1.0]));
    // Exercise the application's output transforms, not only the original
    // architecture-test transform. Every allowed tonal extreme must preserve
    // ordering, neutral balance, alpha and finite output.
    for scene_referred in [false, true] {
        for amount in [-100.0, 0.0, 100.0] {
            let mut c = config();
            c.source.transfer = Transfer::Linear;
            c.source.reference = if scene_referred {
                ReferenceDomain::Scene
            } else {
                ReferenceDomain::Display
            };
            c.output_rendering = if scene_referred {
                OutputRendering::SceneLuminanceV1
            } else {
                OutputRendering::DisplayGamutV1
            };
            c.controls.contrast = amount;
            c.controls.shadows = amount;
            c.controls.highlights = -amount;
            c.controls.blacks = amount;
            c.controls.whites = -amount;
            let ramp = ImageBuffer::from_fn(4097, 1, |x, _| {
                let v = x as f32 / 512.0;
                Rgba([v, v, v, 0.375])
            });
            let frame = engine
                .render(&ramp, &RenderPlan::build(c).unwrap(), true)
                .unwrap();
            let mut previous = 0.0;
            for p in frame.encoded_srgb.pixels() {
                assert!(p.0.iter().all(|v| v.is_finite()), "nonfinite tonal output");
                assert!(
                    p[0] + 3e-6 >= previous,
                    "tonal reversal at {amount}: {previous} -> {}",
                    p[0]
                );
                assert!(
                    (p[0] - p[1]).abs() < 1e-5 && (p[1] - p[2]).abs() < 1e-5,
                    "neutral tint: {p:?}"
                );
                assert_eq!(p[3], 0.375);
                previous = p[0];
            }
        }
    }
    let mut exposure = config();
    exposure.controls.exposure = 1.0;
    let exposed = engine
        .render(&input, &RenderPlan::build(exposure).unwrap(), true)
        .unwrap();
    let stages = exposed.stages.unwrap();
    for (working, graded) in stages.working.pixels().zip(stages.graded.pixels()) {
        for c in 0..3 {
            assert!((graded[c] - 2.0 * working[c]).abs() < 3e-6);
        }
    }
    let mut display = config();
    display.output_rendering = OutputRendering::DisplayGamutV1;
    let identity = engine
        .render(&input, &RenderPlan::build(display).unwrap(), false)
        .unwrap();
    for (a, b) in input.as_raw().iter().zip(identity.encoded_srgb.as_raw()) {
        assert!(
            (a - b).abs() < 5e-5,
            "display gamut transform changed in-gamut input: {a} -> {b}"
        );
    }
    captured_transform_contracts(&engine);
    effects_contracts(&engine);
    channel_curve_contracts(&engine);
    application_contracts(&context);
    advanced_control_contracts(&engine);
    output_and_grading_contracts(&engine);
    let signed = engine.render(&negative, &plan, true).unwrap();
    assert!(signed.stages.unwrap().working.get_pixel(0, 0)[0] < 0.0);
    assert!(
        engine
            .render(&ImageBuffer::new(0, 0), &plan, false)
            .is_err()
    );
    assert!(
        engine
            .render(
                &ImageBuffer::from_pixel(1, 1, Rgba([f32::NAN, 0.0, 0.0, 1.0])),
                &plan,
                false
            )
            .is_err()
    );
}

/// Detail through the application path, on an image with something to act on.
fn detail_contracts(context: &GpuContext) {
    use rapidraw_lib::color_engine::application::render_file;
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("textured.png");
    // Soft blobs at a few scales: clarity and structure have something to find.
    ImageBuffer::from_fn(512, 256, |x, y| {
        let (fx, fy) = (x as f32, y as f32);
        let v = 0.45
            + 0.2 * (fx / 37.0).sin() * (fy / 29.0).cos()
            + 0.1 * (fx / 9.0).sin() * (fy / 11.0).sin();
        let c = (v.clamp(0.0, 1.0) * 255.0) as u8;
        Rgba([c, (c as f32 * 0.85) as u8, (c as f32 * 0.7) as u8, 255])
    })
    .save(&path)
    .unwrap();
    let path = path.to_str().unwrap();
    let state = rapidraw_lib::AppState::default();
    let neutral = json!({"processVersion":3,"v3":{},"masks":[]});
    let base = render_file(context, &state, path, &neutral, None).unwrap();

    // Neutral detail is exactly no detail.
    let idle = json!({"processVersion":3,"v3":{"detail":{"threshold":40}},"masks":[]});
    assert_eq!(
        render_file(context, &state, path, &idle, None)
            .unwrap()
            .encoded_srgb,
        base.encoded_srgb,
        "a threshold with no sharpening changed the image"
    );

    let clarity =
        json!({"processVersion":3,"v3":{"detail":{"clarity":80,"structure":60}},"masks":[]});
    let full = render_file(context, &state, path, &clarity, None).unwrap();
    assert_ne!(full.encoded_srgb, base.encoded_srgb, "detail had no effect");

    // The preview must show what the export will do. Radii scale with the
    // preview, so a quarter-size render should match the full render scaled
    // down — not exactly, a smaller image cannot hold the same detail, but
    // closely, and much closer than to the image without the edit.
    let preview = render_file(context, &state, path, &clarity, Some(128)).unwrap();
    let down = image::imageops::resize(
        &full.encoded_srgb,
        preview.encoded_srgb.width(),
        preview.encoded_srgb.height(),
        image::imageops::FilterType::Triangle,
    );
    let base_down = image::imageops::resize(
        &base.encoded_srgb,
        preview.encoded_srgb.width(),
        preview.encoded_srgb.height(),
        image::imageops::FilterType::Triangle,
    );
    let mean_gap = |a: &image::Rgba32FImage, b: &image::Rgba32FImage| {
        a.as_raw()
            .iter()
            .zip(b.as_raw())
            .map(|(x, y)| (x - y).abs())
            .sum::<f32>()
            / a.as_raw().len() as f32
    };
    let to_preview = mean_gap(&preview.encoded_srgb, &down);
    let effect = mean_gap(&down, &base_down);
    assert!(
        to_preview < effect * 0.35,
        "preview does not show what the export does: preview differs by {to_preview}, the edit itself is {effect}"
    );

    // A cached detail pass must not survive a change to it.
    let stronger =
        json!({"processVersion":3,"v3":{"detail":{"clarity":100,"structure":60}},"masks":[]});
    assert_ne!(
        render_file(context, &state, path, &stronger, None)
            .unwrap()
            .encoded_srgb,
        full.encoded_srgb,
        "detail cache returned a stale result"
    );
    // Other sliders reuse it, and still take effect.
    let brighter = json!({"processVersion":3,"v3":{"exposure":0.5,"detail":{"clarity":80,"structure":60}},"masks":[]});
    assert_ne!(
        render_file(context, &state, path, &brighter, None)
            .unwrap()
            .encoded_srgb,
        full.encoded_srgb
    );

    // Grain belongs to the finished image, so it must survive the mask path
    // — where the last pass has neutral controls of its own.
    let grainy = json!({"processVersion":3,"v3":{"effects":{"grain_amount":80}},"masks":[]});
    let grainy_masked = json!({"processVersion":3,"v3":{"effects":{"grain_amount":80}},"masks":[{
        "id":"z","name":"z","visible":true,"invert":false,"opacity":0,
        "adjustments":{"v3":{"exposure":0.5}},
        "subMasks":[{"id":"l","type":"linear","visible":true,"mode":"additive",
            "parameters":{"startX":0,"startY":1000,"endX":100,"endY":1000,"range":1}}]
    }]});
    let with_grain = render_file(context, &state, path, &grainy, None).unwrap();
    assert_ne!(
        with_grain.encoded_srgb, base.encoded_srgb,
        "grain had no effect"
    );
    let gap = mean_gap(
        &render_file(context, &state, path, &grainy_masked, None)
            .unwrap()
            .encoded_srgb,
        &with_grain.encoded_srgb,
    );
    assert!(
        gap < 1e-4,
        "grain was lost or doubled when a mask was present: {gap}"
    );

    // Detail inside a mask. Luminance is the same quantity in any primaries,
    // so a mask covering everything must do what the global control does.
    let mut masked = json!({"processVersion":3,"v3":{},"masks":[{
        "id":"m","name":"m","visible":true,"invert":false,"opacity":100,
        "adjustments":{"v3":{"detail":{"clarity":80,"structure":60}}},
        "subMasks":[{"id":"l","type":"linear","visible":true,"mode":"additive",
            "parameters":{"startX":0,"startY":1000,"endX":100,"endY":1000,"range":1}}]
    }]});
    let local = render_file(context, &state, path, &masked, None).unwrap();
    let gap = mean_gap(&local.encoded_srgb, &full.encoded_srgb);
    assert!(
        gap < 1e-3,
        "full-coverage mask detail differs from global detail by {gap}"
    );
    // And a mask at zero opacity does nothing.
    masked["masks"][0]["opacity"] = json!(0);
    assert_eq!(
        render_file(context, &state, path, &masked, None)
            .unwrap()
            .encoded_srgb,
        base.encoded_srgb
    );
}

fn application_contracts(context: &GpuContext) {
    use rapidraw_lib::color_engine::application::render_file;
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.png");
    // Constant color makes spatial resizing independent of the color contract.
    ImageBuffer::from_pixel(64, 32, Rgba([90u8, 120, 150, 255]))
        .save(&path)
        .unwrap();
    let path = path.to_str().unwrap();
    let state = rapidraw_lib::AppState::default();
    let neutral = json!({"processVersion":3,"v3":{},"masks":[]});
    let exposed = json!({"processVersion":3,"v3":{"exposure":1.0},"masks":[]});
    let base = render_file(context, &state, path, &neutral, None).unwrap();
    let full = render_file(context, &state, path, &exposed, None).unwrap();
    assert_ne!(
        base.encoded_srgb, full.encoded_srgb,
        "control change reused stale render"
    );
    let preview = render_file(context, &state, path, &exposed, Some(32)).unwrap();
    assert_eq!(preview.encoded_srgb.dimensions(), (32, 16));
    for (a, b) in preview
        .encoded_srgb
        .get_pixel(16, 8)
        .0
        .iter()
        .zip(full.encoded_srgb.get_pixel(32, 16).0)
    {
        assert!((a - b).abs() < 2e-5, "preview/export color mismatch");
    }
    let mut masked = json!({"processVersion":3,"v3":{},"masks":[{
        "id":"test","name":"full","visible":true,"invert":false,"opacity":100,
        "adjustments":{"v3":{"exposure":1.0}},
        "subMasks":[{"id":"linear","type":"linear","visible":true,"mode":"additive",
            "parameters":{"startX":0,"startY":1000,"endX":100,"endY":1000,"range":1}}]
    }]});
    let local = render_file(context, &state, path, &masked, None).unwrap();
    for (a, b) in local
        .encoded_srgb
        .as_raw()
        .iter()
        .zip(full.encoded_srgb.as_raw())
    {
        assert!(
            (a - b).abs() < 2e-5,
            "full mask differs from global exposure {a} {b}"
        );
    }
    masked["masks"][0]["opacity"] = json!(0);
    assert_eq!(
        render_file(context, &state, path, &masked, None)
            .unwrap()
            .encoded_srgb,
        base.encoded_srgb
    );
    // Colour range masks sample the picture before grading. On a constant
    // colour, clicking it must select the whole frame — so the result has to
    // match a global exposure — and clicking a colour that is not there must
    // select nothing at all.
    masked["masks"][0]["opacity"] = json!(100);
    masked["masks"][0]["subMasks"][0]["type"] = json!("color");
    masked["masks"][0]["subMasks"][0]["parameters"] =
        json!({"targetX": 32, "targetY": 16, "tolerance": 40});
    let ranged = render_file(context, &state, path, &masked, None).unwrap();
    for (a, b) in ranged
        .encoded_srgb
        .as_raw()
        .iter()
        .zip(full.encoded_srgb.as_raw())
    {
        assert!(
            (a - b).abs() < 2e-5,
            "a colour range mask over its own colour should match global exposure: {a} {b}"
        );
    }
    // Sampling is stable under grading: the same click with the exposure
    // already pushed must still select the same region, because the mask
    // reads the picture before the grade rather than after it.
    let mut graded = masked.clone();
    graded["v3"] = json!({"exposure": 0.5});
    let a = render_file(context, &state, path, &graded, None).unwrap();
    graded["v3"] = json!({"exposure": -0.5});
    let b = render_file(context, &state, path, &graded, None).unwrap();
    assert_ne!(a.encoded_srgb, b.encoded_srgb, "grade had no effect");
    let mut elsewhere = masked.clone();
    elsewhere["masks"][0]["subMasks"][0]["parameters"] = json!({"targetX": 32, "targetY": 16, "tolerance": 40, "swatchHue": 0.0, "swatchWidth": 2.0});
    let none = render_file(context, &state, path, &elsewhere, None).unwrap();
    for (a, b) in none
        .encoded_srgb
        .as_raw()
        .iter()
        .zip(base.encoded_srgb.as_raw())
    {
        assert!(
            (a - b).abs() < 2e-5,
            "a colour range that matches nothing must change nothing: {a} {b}"
        );
    }
    detail_contracts(context);
    let mut invalid = neutral.clone();
    invalid["v3"]["revision"] = json!(999);
    assert!(render_file(context, &state, path, &invalid, None).is_err());
    use base64::Engine;
    use rapidraw_lib::color_engine::selection::inspect;
    let mut selecting = json!({"processVersion":3,"v3":{"ranges":[{"center":[0,0.1,0.5],"width":[45,0.2,0.5],"adjustment":[0,0,0]}]},"masks":[]});
    let picked = inspect(context, &state, path, &selecting, 0, Some([0.5, 0.5])).unwrap();
    let center = picked.center.unwrap();
    assert!(center[1] > 0.01);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(picked.selection.split(',').nth(1).unwrap())
        .unwrap();
    let matte = image::load_from_memory(&decoded).unwrap().to_luma8();
    assert_eq!(matte.dimensions(), (64, 32));
    assert_eq!(
        matte.get_pixel(32, 16)[0],
        255,
        "sample did not select its own color"
    );
    selecting["v3"]["hue"] = json!(100);
    selecting["v3"]["ranges"][0]["adjustment"] = json!([60, 100, 100]);
    assert_eq!(
        inspect(context, &state, path, &selecting, 0, Some([0.5, 0.5]))
            .unwrap()
            .center,
        Some(center),
        "sampling moved with selective edits"
    );
    assert!(inspect(context, &state, path, &selecting, 9, None).is_err());
    assert!(inspect(context, &state, path, &selecting, 0, Some([-0.1, 0.5])).is_err());
    let divided = directory.path().join("divided.png");
    ImageBuffer::from_fn(64, 32, |x, _| {
        if x < 32 {
            Rgba([200u8, 30, 30, 255])
        } else {
            Rgba([30u8, 30, 200, 255])
        }
    })
    .save(&divided)
    .unwrap();
    let path = divided.to_str().unwrap();
    let blue = inspect(context, &state, path, &selecting, 0, Some([0.75, 0.5]))
        .unwrap()
        .center;
    selecting["crop"] = json!({"unit":"px","x":32,"y":0,"width":32,"height":32});
    assert_eq!(
        inspect(context, &state, path, &selecting, 0, Some([0.5, 0.5]))
            .unwrap()
            .center,
        blue,
        "crop misregistered sampled color"
    );
}

/// The behaviours changed alongside the soft gamut mapper: wheels keyed to the
/// tone-mapped image, a shadow wheel that can lift black, and out-of-gamut
/// colours that stay distinguishable.
/// RGB curves act per channel in DaVinci Intermediate.
fn channel_curve_contracts(engine: &ColorEngine) {
    let mut c = config();
    c.source.transfer = Transfer::Linear;
    let ramp = ImageBuffer::from_fn(1025, 1, |x, _| {
        let v = x as f32 / 256.0;
        Rgba([v, v, v, 1.0])
    });
    let graded = |c: PipelineConfig| {
        engine
            .render(&ramp, &RenderPlan::build(c).unwrap(), true)
            .unwrap()
            .stages
            .unwrap()
            .graded
    };
    let base = graded(c.clone());
    // Identity curves change nothing, bit for bit.
    c.controls.channel_curves = [rapidraw_lib::color_engine::controls::Controls::IDENTITY_CURVE; 3];
    assert_eq!(graded(c.clone()), base);
    // Lifting only the red curve lifts only red: a neutral ramp turns warm,
    // monotonically, and green and blue are untouched.
    c.controls.channel_curves[0] = [0.0, 0.32, 0.6, 0.83, 1.0];
    let warm = graded(c.clone());
    let mut last = f32::MIN;
    for (w, b) in warm.pixels().zip(base.pixels()) {
        assert!(
            (w[1] - b[1]).abs() < 1e-5 && (w[2] - b[2]).abs() < 1e-5,
            "green or blue moved"
        );
        assert!(w[0] >= b[0] - 1e-6, "a lifted curve darkened red");
        assert!(w[0] >= last - 1e-5, "red curve reversed tones");
        last = w[0];
    }
    assert!(
        warm.get_pixel(64, 0)[0] > base.get_pixel(64, 0)[0] * 1.1,
        "red was not lifted"
    );
    // Above the encoding's range the end tangent continues: no clamp.
    let top = warm.get_pixel(1024, 0)[0];
    assert!(
        top.is_finite() && top > warm.get_pixel(900, 0)[0],
        "highlights clamped by the curve"
    );
}

/// Vignette and grain: position-dependent, so they get their own checks.
fn effects_contracts(engine: &ColorEngine) {
    let grey = ImageBuffer::from_pixel(200, 100, Rgba([0.3f32, 0.3, 0.3, 1.0]));
    let render = |c: PipelineConfig| {
        let mut plan = RenderPlan::build(c).unwrap();
        plan.set_render_scale(1.0);
        engine.render(&grey, &plan, false).unwrap().encoded_srgb
    };
    let base = render(config());
    let mut dark = config();
    dark.controls.effects.vignette_amount = -70.0;
    let v = render(dark.clone());
    let (centre, corner) = (v.get_pixel(100, 50)[0], v.get_pixel(0, 0)[0]);
    assert!(
        (centre - base.get_pixel(100, 50)[0]).abs() < 1e-4,
        "vignette touched the centre"
    );
    assert!(
        corner < base.get_pixel(0, 0)[0] - 0.05,
        "vignette did not darken the corner"
    );
    // Symmetric about the centre: all four corners alike.
    for (x, y) in [(199, 0), (0, 99), (199, 99)] {
        assert!(
            (v.get_pixel(x, y)[0] - corner).abs() < 1e-3,
            "vignette not symmetric"
        );
    }
    let mut light = config();
    light.controls.effects.vignette_amount = 70.0;
    assert!(render(light).get_pixel(0, 0)[0] > base.get_pixel(0, 0)[0] + 0.05);

    // Grain: present, deterministic, roughly zero-mean, absent at zero.
    let mut grainy = config();
    grainy.controls.effects.grain_amount = 80.0;
    let g = render(grainy.clone());
    assert_eq!(g, render(grainy), "grain must be deterministic");
    let deltas: Vec<f32> = g
        .pixels()
        .zip(base.pixels())
        .map(|(a, b)| a[0] - b[0])
        .collect();
    let mean = deltas.iter().sum::<f32>() / deltas.len() as f32;
    let spread =
        (deltas.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / deltas.len() as f32).sqrt();
    assert!(spread > 0.005, "no visible grain: {spread}");
    assert!(
        mean.abs() < spread * 0.5,
        "grain shifted the brightness: mean {mean} spread {spread}"
    );
    // Grain keeps its size relative to the photograph: rendering at half
    // scale must sample the same pattern at half the pixel spacing.
    let mut half = RenderPlan::build({
        let mut c = config();
        c.controls.effects.grain_amount = 80.0;
        c
    })
    .unwrap();
    half.set_render_scale(0.5);
    let small = ImageBuffer::from_pixel(100, 50, Rgba([0.3f32, 0.3, 0.3, 1.0]));
    let half_frame = engine.render(&small, &half, false).unwrap().encoded_srgb;
    // Pixel (x, y) at half scale is centred on full-resolution (2x+1, 2y+1).
    let mut agree = 0;
    for (x, y) in [(10, 10), (40, 20), (70, 35), (25, 40)] {
        let a = half_frame.get_pixel(x, y)[0] - base.get_pixel(0, 0)[0];
        let b = g.get_pixel(2 * x + 1, 2 * y + 1)[0] - base.get_pixel(0, 0)[0];
        if (a - b).abs() < 0.02 {
            agree += 1;
        }
    }
    assert!(agree >= 3, "grain pattern does not follow the render scale");
}

/// A transform captured from Resolve is only worth having if what runs on the
/// GPU is the transform that was captured.
fn captured_transform_contracts(engine: &ColorEngine) {
    use rapidraw_lib::color_engine::cube::CubeLut;
    let size = 17usize;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("captured.cube");
    // A deliberately awkward stand-in for a rendering transform: per-channel
    // curves plus cross-channel mixing, so a lookup that quietly ignored an
    // axis or transposed two of them could not pass.
    let mut text = format!("LUT_3D_SIZE {size}\n");
    let axis = |i: usize| i as f32 / (size - 1) as f32;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let (r, g, b) = (axis(r), axis(g), axis(b));
                text.push_str(&format!(
                    "{} {} {}\n",
                    (r * 0.8 + g * 0.2).powf(1.6),
                    (g * 0.9 + b * 0.1).powf(0.8),
                    (b * 0.7 + r * 0.3).powf(1.2)
                ));
            }
        }
    }
    std::fs::write(&path, &text).unwrap();
    let cube = CubeLut::parse(&text).unwrap();

    let mut c = config();
    c.source.transfer = Transfer::Linear;
    c.output_rendering = OutputRendering::ResolveCubeV1;
    c.output_lut = Some(path.clone());
    // A cube is required for this rendering, and forbidden for the others.
    let mut missing = c.clone();
    missing.output_lut = None;
    assert!(RenderPlan::build(missing).is_err());
    let mut stray = config();
    stray.output_lut = Some(path.clone());
    assert!(RenderPlan::build(stray).is_err());

    // Values spanning the working range, including above white and below
    // black, since the cube's domain is a log encoding of exactly that.
    let probe = ImageBuffer::from_fn(64, 1, |x, _| {
        let t = x as f32 / 63.0;
        Rgba([
            (t * 8.0).powf(2.0) - 0.01,
            0.02 + t * 1.2,
            (1.0 - t) * 3.0,
            1.0,
        ])
    });
    let rendered = engine
        .render(&probe, &RenderPlan::build(c.clone()).unwrap(), true)
        .unwrap();
    let graded = rendered.stages.as_ref().unwrap().graded.clone();
    for (working, out) in graded.pixels().zip(rendered.encoded_srgb.pixels()) {
        let logged: [f32; 3] =
            std::array::from_fn(|i| spaces::encode_intermediate(working[i] as f64) as f32);
        let want = cube.sample(logged);
        for c in 0..3 {
            // Tetrahedral on the GPU against trilinear here: they agree
            // exactly on the lattice and differ only inside a cell.
            assert!(
                (out[c] - want[c]).abs() < 4e-3,
                "captured transform not applied as captured: {out:?} vs {want:?}"
            );
        }
    }

    // And nothing may encode after it: the cube's output is already display
    // values, so a second sRGB encode would lift the whole image.
    let flat = ImageBuffer::from_pixel(1, 1, Rgba([0.18f32, 0.18, 0.18, 1.0]));
    let out = engine
        .render(&flat, &RenderPlan::build(c).unwrap(), false)
        .unwrap();
    let logged = spaces::encode_intermediate(0.18) as f32;
    let want = cube.sample([logged; 3]);
    assert!(
        (out.encoded_srgb.get_pixel(0, 0)[0] - want[0]).abs() < 4e-3,
        "a second encode ran after the captured transform: {:?} vs {want:?}",
        out.encoded_srgb.get_pixel(0, 0)
    );
}

fn output_and_grading_contracts(engine: &ColorEngine) {
    let render = |c: PipelineConfig, image: &image::Rgba32FImage| {
        engine
            .render(image, &RenderPlan::build(c).unwrap(), false)
            .unwrap()
            .encoded_srgb
    };
    let scene = || {
        let mut c = config();
        c.source.transfer = Transfer::Linear;
        c.source.reference = ReferenceDomain::Scene;
        c.output_rendering = OutputRendering::SceneLuminanceV2;
        c
    };

    // Soft compression: in-gamut colours well inside the cube are untouched.
    let mut display = config();
    display.output_rendering = OutputRendering::DisplayGamutV2;
    let modest = ImageBuffer::from_fn(32, 1, |x, _| {
        let v = x as f32 / 64.0 + 0.1;
        Rgba([v, v * 0.8, v * 0.6, 1.0])
    });
    for (a, b) in modest
        .as_raw()
        .iter()
        .zip(render(display.clone(), &modest).as_raw())
    {
        assert!(
            (a - b).abs() < 5e-5,
            "soft mapper moved a colour inside the gamut: {a} -> {b}"
        );
    }

    // Out of gamut at one fixed lightness, so only chroma is under test.
    // Projecting lands every one of these on the same gamut shell; the point
    // of compressing is that they stay apart.
    let hue = 0.6f64;
    let sweep = ImageBuffer::from_fn(6, 1, |x, _| {
        let chroma = 0.16 + x as f64 * 0.03;
        let rgb = spaces::rgb_from_oklab(
            Primaries::Srgb,
            glam::DVec3::new(0.62, chroma * hue.cos(), chroma * hue.sin()),
        );
        Rgba([rgb.x as f32, rgb.y as f32, rgb.z as f32, 1.0])
    });
    let mut linear = config();
    linear.source.transfer = Transfer::Linear;
    let separation = |rendering| {
        let mut c = linear.clone();
        c.output_rendering = rendering;
        let out = render(c, &sweep);
        let pixels: Vec<_> = out.pixels().map(|p| p.0).collect();
        pixels
            .windows(2)
            .map(|w| (0..3).fold(0.0f32, |m, c| m.max((w[1][c] - w[0][c]).abs())))
            .collect::<Vec<f32>>()
    };
    let soft = separation(OutputRendering::DisplayGamutV2);
    let hard = separation(OutputRendering::DisplayGamutV1);
    let smallest = |g: &[f32]| g.iter().fold(f32::MAX, |m, v| m.min(*v));
    let largest = |g: &[f32]| g.iter().fold(0.0f32, |m, v| m.max(*v));
    // Total separation is the wrong measure: the hard projection inflates it
    // with one big jump at the boundary and then flattens. What matters is
    // that every neighbouring pair stays apart, and that no pair jumps.
    assert!(
        smallest(&soft) > 1.0 / 255.0,
        "soft mapper made neighbouring out-of-gamut colours indistinguishable: {soft:?}"
    );
    assert!(
        smallest(&hard) < 1.0 / 255.0,
        "hard projection no longer collapses; this contract is measuring nothing: {hard:?}"
    );
    assert!(
        largest(&soft) < largest(&hard),
        "soft mapper kept the projection's discontinuity: {soft:?} vs {hard:?}"
    );

    // The shadow wheel's lightness must reach pure black; its tint must not.
    let black = ImageBuffer::from_pixel(1, 1, Rgba([0.0f32, 0.0, 0.0, 1.0]));
    let mut lift = scene();
    lift.controls.grading[1] = [30.0, 0.0, 100.0];
    let lifted = render(lift.clone(), &black);
    lift.controls.grading[1] = [30.0, 0.0, 40.0];
    let lifted_less = render(lift, &black);
    assert!(
        lifted.get_pixel(0, 0)[0] > lifted_less.get_pixel(0, 0)[0] + 1.0 / 255.0
            && lifted_less.get_pixel(0, 0)[0] > 1.0 / 255.0,
        "shadow wheel could not lift black: {:?}",
        lifted.get_pixel(0, 0)
    );
    let mut tint = scene();
    tint.controls.grading[1] = [30.0, 100.0, 0.0];
    let tinted = render(tint, &black);
    let p = tinted.get_pixel(0, 0).0;
    assert!(
        p[0].max(p[1]).max(p[2]) < 1e-3,
        "shadow wheel tinted pure black: {p:?}"
    );

    // Wheels follow the tone-mapped image: the same highlight wheel must act
    // on a patch that exposure has lifted into the highlights.
    let dim = ImageBuffer::from_pixel(1, 1, Rgba([0.05f32, 0.05, 0.05, 1.0]));
    let mut wheel = scene();
    wheel.controls.grading[3] = [30.0, 100.0, 0.0];
    let before = render(wheel.clone(), &dim).get_pixel(0, 0).0;
    wheel.controls.exposure = 4.0;
    let after = render(wheel.clone(), &dim).get_pixel(0, 0).0;
    let tintedness = |p: [f32; 4]| (p[0] - p[2]).abs();
    assert!(
        tintedness(after) > tintedness(before) + 1e-3,
        "highlight wheel ignored the tone-mapped luminance: {before:?} -> {after:?}"
    );
}

fn advanced_control_contracts(engine: &ColorEngine) {
    use rapidraw_lib::color_engine::controls::ColorRange;
    let mut c = config();
    c.source.transfer = Transfer::Linear;
    let ramp = ImageBuffer::from_fn(4097, 1, |x, _| {
        let v = x as f32 / 256.;
        Rgba([v, v, v, 0.7])
    });
    for curve in [
        [0., 0.01, 0.02, 0.03, 1.],
        [0., 0.97, 0.98, 0.99, 1.],
        [0., 0.15, 0.5, 0.85, 1.],
    ] {
        c.controls.curve = curve;
        let frame = engine
            .render(&ramp, &RenderPlan::build(c.clone()).unwrap(), true)
            .unwrap();
        let graded = frame.stages.unwrap().graded;
        let mut last = 0.;
        for p in graded.pixels() {
            assert!(
                p[0] >= last - 2e-5,
                "curve reversed {curve:?}: {last} -> {}",
                p[0]
            );
            assert!(
                (p[0] - p[1]).abs() < p[0].abs() * 2e-6 + 1e-6,
                "curve tinted neutral"
            );
            last = p[0];
        }
        assert!(last > 1.01, "curve clipped highlight headroom: {curve:?}");
    }
    c.controls.curve = [0., 0.5, 0.2, 0.75, 1.];
    assert!(RenderPlan::build(c.clone()).is_err());
    c.controls.curve = [0., 0.25, 0.5, 0.75, 1.];
    let range = ColorRange {
        center: [0., 0.1, 0.65],
        width: [40., 0.2, 0.5],
        adjustment: [30., 40., 0.],
    };
    let rgb_from_hue = |h: f64| {
        let a = 0.1 * h.to_radians().cos();
        let b = 0.1 * h.to_radians().sin();
        let l = (0.65 + 0.3963377774 * a + 0.2158037573 * b).powi(3);
        let m = (0.65 - 0.1055613458 * a - 0.0638541728 * b).powi(3);
        let s = (0.65 - 0.0894841775 * a - 1.2914855480 * b).powi(3);
        Rgba([
            (4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s) as f32,
            (-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s) as f32,
            (-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s) as f32,
            1.,
        ])
    };
    let samples = ImageBuffer::from_fn(3, 1, |x, _| rgb_from_hue([359.99, 0.01, 180.][x as usize]));
    let neutral = engine
        .render(&samples, &RenderPlan::build(c.clone()).unwrap(), true)
        .unwrap();
    c.controls.ranges = vec![range.clone()];
    let adjusted = engine
        .render(&samples, &RenderPlan::build(c.clone()).unwrap(), true)
        .unwrap();
    let graded = &adjusted.stages.as_ref().unwrap().graded;
    for ch in 0..3 {
        assert!(
            (graded.get_pixel(0, 0)[ch] - graded.get_pixel(1, 0)[ch]).abs() < 0.0002,
            "hue seam"
        );
        assert!(
            (graded.get_pixel(2, 0)[ch]
                - neutral.stages.as_ref().unwrap().graded.get_pixel(2, 0)[ch])
                .abs()
                < 2e-6,
            "unselected hue changed"
        );
    }
    assert!(
        (graded.get_pixel(0, 0)[0] - neutral.stages.as_ref().unwrap().graded.get_pixel(0, 0)[0])
            .abs()
            > 0.001,
        "range had no effect"
    );
    c.controls.ranges.push(ColorRange {
        adjustment: [-20., 10., 5.],
        ..range
    });
    let before = engine
        .render(&samples, &RenderPlan::build(c.clone()).unwrap(), false)
        .unwrap();
    c.controls.ranges.reverse();
    let after = engine
        .render(&samples, &RenderPlan::build(c.clone()).unwrap(), false)
        .unwrap();
    assert_eq!(
        before.encoded_srgb, after.encoded_srgb,
        "range ordering changed pixels"
    );
    let saved = serde_json::to_vec(&c).unwrap();
    let reopened = RenderPlan::build(serde_json::from_slice(&saved).unwrap()).unwrap();
    assert_eq!(
        after.encoded_srgb,
        engine
            .render(&samples, &reopened, false)
            .unwrap()
            .encoded_srgb,
        "saved advanced controls changed output"
    );
    let gray = ImageBuffer::from_pixel(1, 1, Rgba([0.18, 0.18, 0.18, 1.]));
    let gray_result = engine.render(&gray, &reopened, false).unwrap();
    let p = gray_result.encoded_srgb.get_pixel(0, 0);
    assert!(
        (p[0] - p[1]).abs() < 2e-6 && (p[1] - p[2]).abs() < 2e-6,
        "range tinted neutral"
    );
    c.controls.ranges.push(ColorRange {
        adjustment: [0.; 3],
        ..c.controls.ranges[0].clone()
    });
    let added_neutral = engine
        .render(&samples, &RenderPlan::build(c).unwrap(), false)
        .unwrap();
    assert_eq!(
        after.encoded_srgb, added_neutral.encoded_srgb,
        "untouched range diluted existing adjustments"
    );
}

/// A patch made by the previous engine's tools is in the file's own code
/// values — that engine never colour-manages a JPEG. Composited in v3, a
/// patch identical to the photograph under it must change nothing, on a
/// wide-gamut file as much as on an sRGB one.
#[test]
fn a_patch_of_the_photo_itself_is_invisible_on_a_p3_file() {
    use base64::Engine;
    use rapidraw_lib::color_engine::{input::decode_profiled_photo, patches};
    let profile = "/System/Library/ColorSync/Profiles/Display P3.icc";
    if !std::path::Path::new(profile).exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.png");
    let tagged = dir.path().join("p3.png");
    // Saturated colours, where P3 and sRGB disagree most.
    ImageBuffer::from_fn(16, 8, |x, _| {
        if x < 8 {
            Rgba([230u8, 40, 30, 255])
        } else {
            Rgba([20u8, 200, 60, 255])
        }
    })
    .save(&plain)
    .unwrap();
    let ok = std::process::Command::new("sips")
        .args(["--embedProfile", profile])
        .arg(&plain)
        .arg("--out")
        .arg(&tagged)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        return;
    }
    let bytes = std::fs::read(&tagged).unwrap();
    // What the previous engine's tools see and write back: the code values.
    let legacy_base = image::load_from_memory(&bytes).unwrap().to_rgb8();
    let encode = |image: image::DynamicImage| {
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        base64::engine::general_purpose::STANDARD.encode(out.into_inner())
    };
    let edits = serde_json::json!({"aiPatches": [{
        "id": "p", "visible": true, "opacity": 100, "feather": 0,
        "patchData": {
            "color": encode(image::DynamicImage::ImageRgb8(legacy_base.clone())),
            "mask": encode(image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(16, 8, image::Luma([255])))),
            "encoding": "linear"
        }
    }]});
    let frame = decode_profiled_photo(&bytes).unwrap();
    let mut patched = frame.pixels.clone();
    patches::composite(
        &mut patched,
        &edits,
        &frame.color,
        frame.source_profile.as_deref(),
        None,
    )
    .unwrap();
    for (a, b) in frame.pixels.pixels().zip(patched.pixels()) {
        for c in 0..3 {
            assert!(
                (a[c] - b[c]).abs() < 2e-3,
                "a patch of the photograph itself changed it: {a:?} -> {b:?}"
            );
        }
    }
}
