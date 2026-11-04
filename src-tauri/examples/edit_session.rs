//! Editing a photo from outside the editor, with its own tools: the AI
//! masks and Sky Replace as their panels run them, and renders exactly as
//! an export makes them (the installed transforms, sRGB). What comes out —
//! mask parameters, sky patches — goes into an ordinary sidecar, which the
//! editor then opens as if the edit had been made in it.
//!
//!   edit_session info PHOTO OUT_DIR            neutral.jpg, size.json, sky.json, subject.json
//!   edit_session render PHOTO EDITS.json OUT.jpg [MAX] [X,Y,W,H]
//!   edit_session subject PHOTO X0,Y0,X1,Y1 OUT.json [X,Y,+|-;...]
//!                                   (a box, then include/exclude clicks; photo pixels)
//!   edit_session mask PHOTO EDITS.json INDEX OUT.png   (a mask as renders place it)
//!   edit_session snap PHOTO EDITS.json OUT.json   (saved Subject mattes refitted to edges)
//!   edit_session pano PHOTO OUT.jpg PHOTO2 [PHOTO3...]   (a panorama stitch, as a 2400 px JPEG)
//!   edit_session merge hdr|pano PHOTO PHOTO2 [...]   (Merge, then Save, as the dialog does)
//!   edit_session sky PHOTO PLATE OPTIONS.json OUT.json [STROKES.json]   (also OUT.json.mask.png)
//!
//! Runs in a private home (models and transforms linked or copied in), so
//! nothing of the app's own data is written.
use anyhow::{Context, Result, ensure};
use darkroom_index::{color_engine::application, edit_harness, image_processing::GpuContext};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::Manager;

const DATA: &str = "Library/Application Support/io.github.chma3348.DarkroomIndex";

fn private_home(real_home: &Path) -> Result<PathBuf> {
    let home = std::env::temp_dir().join("darkroom-edit-session/home");
    let data = home.join(DATA);
    std::fs::create_dir_all(&data)?;
    let real = real_home.join(DATA);
    for name in ["models", "skies"] {
        let link = data.join(name);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(std::fs::canonicalize(real.join(name))?, &link)?;
        }
    }
    for name in [
        "settings.json",
        "output-transform.cube",
        "input-transform.cube",
        "input-transform-p3.cube",
        "output-transform-p3.cube",
    ] {
        if real.join(name).is_file() {
            std::fs::copy(real.join(name), data.join(name))?;
        }
    }
    Ok(home)
}

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

fn install_transforms(app: &tauri::AppHandle, home: &Path) {
    let state = app.state::<darkroom_index::AppState>();
    let data = home.join(DATA);
    for (slot, name) in [
        (&state.output_transform, "output-transform.cube"),
        (&state.input_transform, "input-transform.cube"),
        (&state.input_transform_p3, "input-transform-p3.cube"),
        (&state.output_transform_p3, "output-transform-p3.cube"),
    ] {
        let path = data.join(name);
        if path.is_file() {
            *slot.lock().unwrap() = Some(path);
        }
    }
}

fn save_jpeg(frame: &image::Rgba32FImage, out: &Path) -> Result<()> {
    let rgb = image::RgbImage::from_fn(frame.width(), frame.height(), |x, y| {
        let p = frame.get_pixel(x, y);
        image::Rgb([0, 1, 2].map(|c| (p[c].clamp(0.0, 1.0) * 255.0).round() as u8))
    });
    let mut file = std::fs::File::create(out)?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 90).encode_image(&rgb)?;
    Ok(())
}

fn render(
    app: &tauri::AppHandle,
    context: &GpuContext,
    photo: &str,
    edits: &Value,
    max: Option<u32>,
    region: Option<(f32, f32, f32, f32)>,
    out: &Path,
) -> Result<()> {
    let state = app.state::<darkroom_index::AppState>();
    let frame = match region {
        Some(roi) => application::render_region_for_output(context, &state, photo, edits, max, roi),
        None => application::render_file(context, &state, photo, edits, max),
    }?;
    save_jpeg(&frame.encoded_srgb, out)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(args.len() >= 2, "see the header for usage");
    let real_home = PathBuf::from(std::env::var("HOME")?);
    let home = private_home(&real_home)?;
    // SAFETY: still single-threaded; nothing has read the environment.
    unsafe { std::env::set_var("HOME", &home) };
    // As the app's setup does, so JPEG XL files open.
    jxl_oxide::integration::register_image_decoding_hook();
    let app = tauri::Builder::default()
        .manage(darkroom_index::AppState::default())
        .build(darkroom_index::tauri_context())?;
    let handle = app.handle().clone();
    install_transforms(&handle, &home);
    if args[0] == "merge" {
        // Saved beside the frames, as the dialog saves it (settings and
        // models still come from the private home).
        let frames: Vec<String> = args[2..]
            .iter()
            .map(|p| Ok(std::fs::canonicalize(p)?.to_string_lossy().into_owned()))
            .collect::<Result<_>>()?;
        let saved = tauri::async_runtime::block_on(edit_harness::merge_and_save(
            &handle,
            args[1] == "pano",
            frames,
        ))
        .map_err(anyhow::Error::msg)?;
        println!("{saved}");
        return Ok(());
    }
    let photo = std::fs::canonicalize(&args[1])?
        .to_string_lossy()
        .into_owned();
    let neutral = json!({"processVersion": 3, "toneMapper": "resolve"});
    match args[0].as_str() {
        "info" => {
            let out = PathBuf::from(&args[2]);
            std::fs::create_dir_all(&out)?;
            let (w, h) = edit_harness::open(&handle, &photo).map_err(anyhow::Error::msg)?;
            std::fs::write(
                out.join("size.json"),
                json!({"width": w, "height": h}).to_string(),
            )?;
            let context = gpu()?;
            render(
                &handle,
                &context,
                &photo,
                &neutral,
                Some(1600),
                None,
                &out.join("neutral.jpg"),
            )?;
            match tauri::async_runtime::block_on(edit_harness::sky_mask(&handle, neutral.clone())) {
                Ok(v) => std::fs::write(out.join("sky.json"), v.to_string())?,
                Err(e) => eprintln!("sky: {e}"),
            }
            match tauri::async_runtime::block_on(edit_harness::foreground_mask(
                &handle,
                &photo,
                neutral.clone(),
            )) {
                Ok(v) => std::fs::write(out.join("subject.json"), v.to_string())?,
                Err(e) => eprintln!("subject: {e}"),
            }
            println!("{w}x{h}");
        }
        "render" => {
            let edits: Value = serde_json::from_str(&std::fs::read_to_string(&args[2])?)?;
            let max = args.get(4).map(|m| m.parse()).transpose()?;
            let region = args
                .get(5)
                .map(|r| -> Result<(f32, f32, f32, f32)> {
                    let v: Vec<f32> = r.split(',').map(|s| s.parse()).collect::<Result<_, _>>()?;
                    ensure!(v.len() == 4, "X,Y,W,H");
                    Ok((v[0], v[1], v[2], v[3]))
                })
                .transpose()?;
            let context = gpu()?;
            // The editor renders straight from the file; holding it open only
            // matters to the tools (and the legacy loader can't read merges).
            if let Err(e) = edit_harness::open(&handle, &photo) {
                eprintln!("(not held open: {e})");
            }
            render(
                &handle,
                &context,
                &photo,
                &edits,
                max,
                region,
                Path::new(&args[3]),
            )?;
        }
        "subject" => {
            let v: Vec<f64> = args[2]
                .split(',')
                .map(|s| s.parse())
                .collect::<Result<_, _>>()?;
            ensure!(v.len() == 4, "X0,Y0,X1,Y1");
            let clicks = args
                .get(4)
                .map(|c| -> Result<Vec<(f64, f64, bool)>> {
                    c.split(';')
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            let f: Vec<&str> = s.split(',').collect();
                            ensure!(f.len() == 3, "X,Y,+|-");
                            Ok((f[0].parse()?, f[1].parse()?, f[2] == "+"))
                        })
                        .collect()
                })
                .transpose()?
                .unwrap_or_default();
            edit_harness::open(&handle, &photo).map_err(anyhow::Error::msg)?;
            let p = tauri::async_runtime::block_on(edit_harness::subject_mask(
                &handle,
                &photo,
                neutral,
                (v[0], v[1]),
                (v[2], v[3]),
                &clicks,
            ))
            .map_err(anyhow::Error::msg)?;
            std::fs::write(&args[3], p.to_string())?;
        }
        "mask" => {
            let edits: Value = serde_json::from_str(&std::fs::read_to_string(&args[2])?)?;
            edit_harness::open(&handle, &photo).map_err(anyhow::Error::msg)?;
            edit_harness::mask_bitmap(&handle, &edits, args[3].parse()?)
                .map_err(anyhow::Error::msg)?
                .save(&args[4])?;
        }
        "sky" => {
            let options: Value = serde_json::from_str(&std::fs::read_to_string(&args[3])?)?;
            let strokes: Value = match args.get(5) {
                Some(p) => serde_json::from_str(&std::fs::read_to_string(p)?)?,
                None => json!([]),
            };
            edit_harness::open(&handle, &photo).map_err(anyhow::Error::msg)?;
            let (patch, mask) = tauri::async_runtime::block_on(edit_harness::sky_replace(
                &handle, &photo, &args[2], options, strokes,
            ))
            .map_err(anyhow::Error::msg)
            .context("sky replace")?;
            std::fs::write(&args[4], patch.to_string())?;
            use base64::Engine as _;
            let png = base64::engine::general_purpose::STANDARD
                .decode(mask.split_once(',').map_or(mask.as_str(), |(_, b)| b))?;
            std::fs::write(format!("{}.mask.png", args[4]), png)?;
        }
        "snap" => {
            let mut edits: Value = serde_json::from_str(&std::fs::read_to_string(&args[2])?)?;
            edit_harness::open(&handle, &photo).map_err(anyhow::Error::msg)?;
            let n = edit_harness::snap_saved_subjects(&handle, &mut edits)
                .map_err(anyhow::Error::msg)?;
            std::fs::write(&args[3], edits.to_string())?;
            println!("{n}");
        }
        "pano" => {
            let mut paths = vec![photo.clone()];
            for p in &args[3..] {
                paths.push(std::fs::canonicalize(p)?.to_string_lossy().into_owned());
            }
            let pano = edit_harness::panorama(&handle, &paths).map_err(anyhow::Error::msg)?;
            let rgb = pano.thumbnail(2400, 2400).to_rgb8();
            let mut file = std::fs::File::create(&args[2])?;
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 88).encode_image(&rgb)?;
            println!("{}x{}", pano.width(), pano.height());
        }
        other => anyhow::bail!("unknown command {other}"),
    }
    Ok(())
}
