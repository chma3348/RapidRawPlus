use std::fs;
use std::path::PathBuf;

use image::Rgb32FImage;
use ndarray::{Array, IxDyn};
use rapidraw_lib::enhancement::{run_single_pass_enhancement, run_tiled_enhancement};
use rapidraw_lib::model_registry::{ModelRegistry, TaskType};

const DUMMY_MANIFEST: &str = r#"{
    "id": "dummy-identity",
    "display_name": "Dummy Identity",
    "task_type": "test",
    "file_path": "dummy_identity.onnx",
    "params": { "note": "identity graph for registry round-trip testing" }
}"#;

const MISSING_WEIGHTS_MANIFEST: &str = r#"{
    "id": "ghost-model",
    "display_name": "Ghost Model",
    "task_type": "test",
    "file_path": "not_downloaded.onnx"
}"#;

fn setup_models_dir() -> tempfile::TempDir {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    // ort is built with load-dynamic; point it at the runtime library that
    // build.rs downloads into resources/.
    let lib_name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    let dylib_path = manifest_dir.join("resources").join(lib_name);
    assert!(
        dylib_path.is_file(),
        "onnxruntime library not found at {:?} (build.rs should have downloaded it)",
        dylib_path
    );
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        unsafe { std::env::set_var("ORT_DYLIB_PATH", &dylib_path) };
    }

    let models_dir = tempfile::tempdir().expect("create temp models dir");
    fs::copy(
        manifest_dir.join("tests/fixtures/dummy_identity.onnx"),
        models_dir.path().join("dummy_identity.onnx"),
    )
    .expect("copy dummy model fixture");

    let manifests_dir = models_dir.path().join("manifests");
    fs::create_dir_all(&manifests_dir).unwrap();
    fs::write(manifests_dir.join("dummy_identity.json"), DUMMY_MANIFEST).unwrap();
    fs::write(manifests_dir.join("ghost_model.json"), MISSING_WEIGHTS_MANIFEST).unwrap();

    models_dir
}

#[test]
fn dummy_model_round_trip() {
    // ONNX Runtime's environment may abort during process teardown (after
    // the test harness has already reported results); exit cleanly instead,
    // the same way the app does.
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();
    let registry = ModelRegistry::new(models_dir.path().to_path_buf());

    // Manifest scan: both test manifests registered, availability reflects
    // whether the weight file exists.
    let test_models = registry.list(Some(TaskType::Test));
    assert_eq!(test_models.len(), 2);
    let dummy = test_models.iter().find(|m| m.id == "dummy-identity").unwrap();
    assert!(dummy.available);
    assert!(!dummy.builtin);
    let ghost = test_models.iter().find(|m| m.id == "ghost-model").unwrap();
    assert!(!ghost.available);

    // Builtins are registered too (weights absent in the temp dir).
    let masks = registry.list(Some(TaskType::Mask));
    assert!(masks.iter().any(|m| m.id == "sam-vit-b" && m.builtin && !m.available));

    // Selection prefers an explicit id, but falls back to the first
    // available model when the preferred one has no weights and no
    // download source.
    let selected = registry.select_for_task(TaskType::Test, Some("ghost-model"), |_| true);
    assert_eq!(selected.unwrap().manifest.id, "dummy-identity");

    // The actual round trip: load session, run inference, output == input.
    let input = Array::from_shape_vec(IxDyn(&[1, 3, 4, 4]), (0..48).map(|v| v as f32).collect())
        .unwrap();
    let outputs = registry
        .run_inference("dummy-identity", input.clone())
        .expect("inference through the registry should succeed");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0], input);

    // Missing weights: a clear error, not a crash.
    let err = registry
        .run_inference("ghost-model", input)
        .expect_err("model without weights must not run");
    assert!(err.to_string().contains("weight file is missing"), "unexpected error: {err}");

    // Session unload keeps the entry registered and re-loadable.
    registry.unload("dummy-identity");
    assert!(registry.get("dummy-identity").is_some());
    registry
        .run_inference(
            "dummy-identity",
            Array::from_shape_vec(IxDyn(&[1, 1, 2, 2]), vec![1.0, 2.0, 3.0, 4.0]).unwrap(),
        )
        .expect("model should reload after unload");
}

/// The identity model at scale 1 must reproduce the input exactly through
/// the tiled pipeline — any tiling/reassembly indexing error shows up as a
/// pixel mismatch.
#[test]
fn tiled_enhancement_identity_reassembly() {
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();
    let registry = ModelRegistry::new(models_dir.path().to_path_buf());
    let session = registry.get_session("dummy-identity", None).unwrap();

    // Odd dimensions on purpose: forces partial tiles on both edges.
    let (w, h) = (150u32, 97u32);
    let mut input = Rgb32FImage::new(w, h);
    for (x, y, p) in input.enumerate_pixels_mut() {
        p.0 = [
            (x as f32 / w as f32).min(1.0),
            (y as f32 / h as f32).min(1.0),
            ((x + y) % 255) as f32 / 255.0,
        ];
    }

    let mut progress_calls = 0;
    let output =
        run_tiled_enhancement(&input, &session, 1, 64, 8, None, |_, _| progress_calls += 1)
            .expect("tiled enhancement should succeed");

    assert_eq!(output.dimensions(), (w, h));
    assert!(progress_calls > 1, "expected multiple tiles");
    for (x, y, p) in input.enumerate_pixels() {
        let q = output.get_pixel(x, y);
        for c in 0..3 {
            assert!(
                (p[c] - q[c]).abs() < 1e-6,
                "pixel mismatch at ({x},{y}) channel {c}: {} vs {}",
                p[c],
                q[c]
            );
        }
    }
}

/// End-to-end 4x upscale through a real model. Gated on an env var so CI /
/// fresh checkouts without the weights skip it:
/// `RAPIDRAW_UPSCALE_TEST_MODEL=/path/to/realesr.onnx cargo test -- --ignored`
#[test]
#[ignore]
fn tiled_enhancement_real_upscaler_4x() {
    let Ok(model_path) = std::env::var("RAPIDRAW_UPSCALE_TEST_MODEL") else {
        eprintln!("RAPIDRAW_UPSCALE_TEST_MODEL not set; skipping");
        return;
    };
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();

    // The auto-probe must classify a real dynamic 4x upscaler correctly.
    let probe =
        rapidraw_lib::model_registry::probe_onnx_model(std::path::Path::new(&model_path)).unwrap();
    assert_eq!(probe.scale_factor, 4);
    assert_eq!(probe.fixed_size, None);
    let manifest = format!(
        r#"{{
            "id": "test-upscaler",
            "display_name": "Test Upscaler",
            "task_type": "upscale",
            "file_path": "{}",
            "params": {{ "scale_factor": 4, "tile_size": 128, "tile_overlap": 8 }}
        }}"#,
        model_path
    );
    fs::write(models_dir.path().join("manifests/upscaler.json"), manifest).unwrap();
    let registry = ModelRegistry::new(models_dir.path().to_path_buf());
    let session = registry.get_session("test-upscaler", None).unwrap();

    let (w, h) = (150u32, 97u32);
    let mut input = Rgb32FImage::new(w, h);
    for (x, y, p) in input.enumerate_pixels_mut() {
        p.0 = [
            (x as f32 / w as f32).min(1.0),
            (y as f32 / h as f32).min(1.0),
            0.5,
        ];
    }

    let output = run_tiled_enhancement(&input, &session, 4, 128, 8, None, |done, total| {
        eprintln!("tile {done}/{total}");
    })
    .expect("4x upscale should succeed");
    assert_eq!(output.dimensions(), (w * 4, h * 4));
}

/// The identity model with a fixed input size exercises the edge-padding
/// path: the image is smaller than the tile, so every window is padded and
/// then cropped back. Output must still equal the input exactly.
#[test]
fn tiled_enhancement_fixed_size_padding() {
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();
    let registry = ModelRegistry::new(models_dir.path().to_path_buf());
    let session = registry.get_session("dummy-identity", None).unwrap();

    let (w, h) = (90u32, 70u32);
    let mut input = Rgb32FImage::new(w, h);
    for (x, y, p) in input.enumerate_pixels_mut() {
        p.0 = [(x % 17) as f32 / 17.0, (y % 13) as f32 / 13.0, 0.25];
    }

    let output =
        run_tiled_enhancement(&input, &session, 1, 512, 8, Some((128, 128)), |_, _| {})
            .expect("fixed-size tiled enhancement should succeed");
    assert_eq!(output.dimensions(), (w, h));
    for (x, y, p) in input.enumerate_pixels() {
        let q = output.get_pixel(x, y);
        for c in 0..3 {
            assert!((p[c] - q[c]).abs() < 1e-6, "mismatch at ({x},{y}) ch {c}");
        }
    }
}

/// The auto-probe must accept the identity model and report a 1x dynamic
/// image-to-image graph.
#[test]
fn probe_detects_identity_model() {
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();
    let probe =
        rapidraw_lib::model_registry::probe_onnx_model(&models_dir.path().join("dummy_identity.onnx"))
            .expect("identity model should pass the probe");
    assert_eq!(probe.scale_factor, 1);
    assert_eq!(probe.fixed_size, None);

    // A non-model file must be rejected, not crash.
    let junk = models_dir.path().join("junk.onnx");
    fs::write(&junk, b"not a model").unwrap();
    assert!(rapidraw_lib::model_registry::probe_onnx_model(&junk).is_err());
}

/// The bundled model catalog must always parse, and its entries must carry
/// verifiable download specs.
#[test]
fn bundled_catalog_is_valid() {
    let catalog = rapidraw_lib::model_library::bundled_catalog();
    assert!(!catalog.is_empty());
    for entry in &catalog {
        let dl = entry
            .manifest
            .download
            .as_ref()
            .unwrap_or_else(|| panic!("catalog entry '{}' has no download spec", entry.manifest.id));
        assert_eq!(dl.sha256.len(), 64, "bad sha256 for '{}'", entry.manifest.id);
        assert!(dl.url.starts_with("https://"), "bad url for '{}'", entry.manifest.id);
    }
}

/// Single-pass mode pads to a multiple, runs once, and crops back; with the
/// identity model the result must equal the input exactly.
#[test]
fn single_pass_identity_roundtrip() {
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();
    let registry = ModelRegistry::new(models_dir.path().to_path_buf());
    let session = registry.get_session("dummy-identity", None).unwrap();

    let (w, h) = (90u32, 70u32);
    let mut input = Rgb32FImage::new(w, h);
    for (x, y, p) in input.enumerate_pixels_mut() {
        p.0 = [(x % 19) as f32 / 19.0, (y % 11) as f32 / 11.0, 0.75];
    }

    let output = run_single_pass_enhancement(&input, &session, 1, 32)
        .expect("single-pass enhancement should succeed");
    assert_eq!(output.dimensions(), (w, h));
    for (x, y, p) in input.enumerate_pixels() {
        let q = output.get_pixel(x, y);
        for c in 0..3 {
            assert!((p[c] - q[c]).abs() < 1e-6, "mismatch at ({x},{y}) ch {c}");
        }
    }
}

/// Canvas/mask construction for AI Expand: correct dimensions, original
/// pixels preserved, mask covering exactly the new area plus seam band.
#[test]
fn expansion_canvas_and_mask_geometry() {
    use image::{DynamicImage, Rgb32FImage};
    let (w, h) = (120u32, 90u32);
    let mut img = Rgb32FImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        p.0 = [(x % 11) as f32 / 11.0, (y % 13) as f32 / 13.0, 0.5];
    }
    let dynamic = DynamicImage::ImageRgb32F(img);
    let (canvas, mask) =
        rapidraw_lib::expansion::build_canvas_and_mask(&dynamic, 30, 0, 0, 20).unwrap();
    assert_eq!(canvas.dimensions(), (150, 110));
    assert_eq!(mask.dimensions(), (150, 110));

    let rgba = dynamic.to_rgba8();
    // Original pixels land at offset (30, 0), unchanged.
    for (x, y) in [(0u32, 0u32), (60, 45), (119, 89)] {
        assert_eq!(canvas.get_pixel(x + 30, y), rgba.get_pixel(x, y));
    }
    // New area is masked; deep-inside original is not; seam band is.
    assert_eq!(mask.get_pixel(5, 5)[0], 255, "new left strip must be masked");
    assert_eq!(mask.get_pixel(150 - 1, 109)[0], 255, "new bottom strip must be masked");
    assert_eq!(mask.get_pixel(100, 40)[0], 0, "deep inside original must be unmasked");
    assert_eq!(mask.get_pixel(32, 40)[0], 255, "seam band inside expanded edge must be masked");
    // Right edge was not expanded: no seam band there.
    assert_eq!(mask.get_pixel(149 - 1, 40)[0], 0, "unexpanded right edge must stay unmasked");
}

/// End-to-end expansion fill through the real LaMa model, gated on env:
/// `RAPIDRAW_INPAINT_TEST_MODEL=/path/to/lama_fp16.onnx`
#[test]
#[ignore]
fn expansion_fill_with_real_lama() {
    use image::{DynamicImage, Rgb32FImage};
    let Ok(model_path) = std::env::var("RAPIDRAW_INPAINT_TEST_MODEL") else {
        eprintln!("RAPIDRAW_INPAINT_TEST_MODEL not set; skipping");
        return;
    };
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();
    let manifest = format!(
        r#"{{"id":"lama-test","display_name":"LaMa","task_type":"inpaint","file_path":"{}","params":{{}}}}"#,
        model_path
    );
    fs::write(models_dir.path().join("manifests/lama.json"), manifest).unwrap();
    let registry = ModelRegistry::new(models_dir.path().to_path_buf());
    let session = registry.get_session("lama-test", None).unwrap();

    let (w, h) = (200u32, 150u32);
    let mut img = Rgb32FImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        p.0 = [0.2 + (x % 3) as f32 * 0.1, 0.5, 0.3 + (y % 2) as f32 * 0.1];
    }
    let dynamic = DynamicImage::ImageRgb32F(img);
    let (canvas, mask) =
        rapidraw_lib::expansion::build_canvas_and_mask(&dynamic, 0, 0, 60, 0).unwrap();
    let result = rapidraw_lib::expansion::fill_variant(&canvas, &mask, &session, 512)
        .expect("expansion fill should succeed");
    assert_eq!(result.dimensions(), (260, 150));
    // Unmasked original pixels must be untouched.
    let rgba = dynamic.to_rgba8();
    assert_eq!(result.get_pixel(50, 75), rgba.get_pixel(50, 75));
}

/// End-to-end deblur through a real fixed-size model, gated on an env var:
/// `RAPIDRAW_DEBLUR_TEST_MODEL=/path/to/nafnet.onnx cargo test -- --ignored`
#[test]
#[ignore]
fn tiled_enhancement_real_deblur_fixed_size() {
    let Ok(model_path) = std::env::var("RAPIDRAW_DEBLUR_TEST_MODEL") else {
        eprintln!("RAPIDRAW_DEBLUR_TEST_MODEL not set; skipping");
        return;
    };
    rapidraw_lib::register_exit_handler();
    let models_dir = setup_models_dir();

    // NAFNet declares dynamic dims but rejects small inputs at runtime; the
    // probe must fall back and classify it as a fixed 512x512 model.
    let probe =
        rapidraw_lib::model_registry::probe_onnx_model(std::path::Path::new(&model_path)).unwrap();
    assert_eq!(probe.scale_factor, 1);
    assert_eq!(probe.fixed_size, Some((512, 512)));
    let manifest = format!(
        r#"{{
            "id": "test-deblur",
            "display_name": "Test Deblur",
            "task_type": "deblur",
            "file_path": "{}",
            "params": {{ "scale_factor": 1, "input_height": 512, "input_width": 512, "tile_overlap": 32 }}
        }}"#,
        model_path
    );
    fs::write(models_dir.path().join("manifests/deblur.json"), manifest).unwrap();
    let registry = ModelRegistry::new(models_dir.path().to_path_buf());
    let session = registry.get_session("test-deblur", None).unwrap();

    // Smaller than one tile AND non-multiple dims: exercises padding.
    let (w, h) = (300u32, 220u32);
    let mut input = Rgb32FImage::new(w, h);
    for (x, y, p) in input.enumerate_pixels_mut() {
        p.0 = [
            (x as f32 / w as f32).min(1.0),
            (y as f32 / h as f32).min(1.0),
            0.5,
        ];
    }

    let output = run_tiled_enhancement(&input, &session, 1, 512, 32, Some((512, 512)), |d, t| {
        eprintln!("tile {d}/{t}");
    })
    .expect("deblur should succeed");
    assert_eq!(output.dimensions(), (w, h));
}
