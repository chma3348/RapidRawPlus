use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tauri::{Emitter, Manager};

use crate::app_state::AppState;

/// Local-only port for the managed engine; deliberately not the ComfyUI
/// default (8188) to avoid colliding with a user's own installation.
const ENGINE_PORT: u16 = 8399;

fn base_url() -> String {
    format!("http://127.0.0.1:{}", ENGINE_PORT)
}

pub fn engine_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf> {
    // Test/dev override so installer changes can be validated in isolation.
    if let Ok(dir) = std::env::var("RAPIDRAW_ENGINE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(app_handle.path().app_data_dir()?.join("comfy"))
}

// ---- managed engine installer ----
// Pinned to the exact versions validated on this machine so recipients get
// a known-good stack rather than whatever upstream shipped that day.
const COMFYUI_COMMIT: &str = "35c1470935044be5610a81d46e57922a8a598c6c";
const NODE_PACKS: &[(&str, &str, &str)] = &[
    (
        "ComfyUI-SeedVR2_VideoUpscaler",
        "numz/ComfyUI-SeedVR2_VideoUpscaler",
        "4490bd1f482e026674543386bb2a4d176da245b9",
    ),
    (
        "ComfyUI-GGUF",
        "city96/ComfyUI-GGUF",
        "6ea2651e7df66d7585f6ffee804b20e92fb38b8a",
    ),
    (
        "comfyui-inpaint-nodes",
        "Acly/comfyui-inpaint-nodes",
        "d4a318f00fffbd269418057f869e9bc912832229",
    ),
];
const UV_VERSION: &str = "0.11.26";

/// Finds a usable `uv` (PATH, ~/.local/bin) or downloads a pinned copy
/// into the app's data dir.
async fn ensure_uv(app_handle: &tauri::AppHandle) -> Result<PathBuf> {
    for candidate in [
        PathBuf::from("uv"),
        dirs_next_local_bin().join("uv"),
        app_handle.path().app_data_dir()?.join("bin/uv"),
    ] {
        if std::process::Command::new(&candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Ok(candidate);
        }
    }
    let bin_dir = app_handle.path().app_data_dir()?.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    let url = format!(
        "https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-{arch}-apple-darwin.tar.gz"
    );
    let tarball = bin_dir.join("uv.tar.gz");
    download_to_file(&url, &tarball).await?;
    run_step(
        std::process::Command::new("tar")
            .args(["-xzf", "uv.tar.gz", "--strip-components=1"])
            .current_dir(&bin_dir),
        "extract uv",
    )?;
    let _ = fs::remove_file(&tarball);
    let uv = bin_dir.join("uv");
    if !uv.is_file() {
        return Err(anyhow!("uv download did not produce a binary"));
    }
    Ok(uv)
}

fn dirs_next_local_bin() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".local/bin"))
        .unwrap_or_else(|_| PathBuf::from("/usr/local/bin"))
}

async fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    let resp = reqwest::get(url).await?;
    if !resp.status().is_success() {
        return Err(anyhow!("Download failed ({}): {}", resp.status(), url));
    }
    let bytes = resp.bytes().await?;
    fs::write(dest, &bytes)?;
    Ok(())
}

fn run_step(cmd: &mut std::process::Command, what: &str) -> Result<()> {
    let out = cmd
        .output()
        .map_err(|e| anyhow!("Failed to {}: {}", what, e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(anyhow!("Failed to {}: {}", what, tail));
    }
    Ok(())
}

/// Downloads a pinned GitHub commit tarball and extracts it as `dest`.
async fn fetch_repo_snapshot(repo: &str, commit: &str, dest: &Path) -> Result<()> {
    if dest.join(".").is_dir() && dest.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Ok(());
    }
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("Invalid destination {:?}", dest))?;
    fs::create_dir_all(parent)?;
    let url = format!("https://codeload.github.com/{repo}/tar.gz/{commit}");
    let tarball = parent.join("snapshot.tar.gz");
    download_to_file(&url, &tarball).await?;
    fs::create_dir_all(dest)?;
    run_step(
        std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&tarball)
            .arg("--strip-components=1")
            .arg("-C")
            .arg(dest),
        &format!("extract {}", repo),
    )?;
    let _ = fs::remove_file(&tarball);
    Ok(())
}

/// One-click engine install: pinned ComfyUI + node packs + a uv-managed
/// Python 3.12 environment. Idempotent — a partial install resumes.
pub async fn install_engine(
    app_handle: &tauri::AppHandle,
    mut on_progress: impl FnMut(String),
) -> Result<()> {
    let dir = engine_dir(app_handle)?;
    fs::create_dir_all(&dir)?;

    on_progress("Downloading ComfyUI...".to_string());
    fetch_repo_snapshot("comfyanonymous/ComfyUI", COMFYUI_COMMIT, &dir.join("ComfyUI")).await?;

    for (name, repo, commit) in NODE_PACKS {
        on_progress(format!("Downloading extension: {name}..."));
        fetch_repo_snapshot(repo, commit, &dir.join("ComfyUI/custom_nodes").join(name)).await?;
    }

    on_progress("Preparing Python environment (first time only)...".to_string());
    let uv = ensure_uv(app_handle).await?;
    let venv = dir.join("venv");
    if !venv.join("bin/python").is_file() {
        run_step(
            std::process::Command::new(&uv)
                .args(["venv", "--python", "3.12"])
                .arg(&venv),
            "create Python environment",
        )?;
    }

    on_progress("Installing engine dependencies (~2 GB, one time)...".to_string());
    let python = venv.join("bin/python");
    run_step(
        std::process::Command::new(&uv)
            .args(["pip", "install", "--python"])
            .arg(&python)
            .arg("-r")
            .arg(dir.join("ComfyUI/requirements.txt"))
            .arg("-r")
            .arg(dir.join("ComfyUI/custom_nodes/ComfyUI-SeedVR2_VideoUpscaler/requirements.txt"))
            .args(["gguf", "sentencepiece", "protobuf"]),
        "install engine dependencies",
    )?;

    // Model folders so downloads have somewhere to land.
    for sub in ["SEEDVR2", "checkpoints", "unet", "clip", "vae", "inpaint"] {
        fs::create_dir_all(dir.join("ComfyUI/models").join(sub))?;
    }

    on_progress("Engine installed.".to_string());
    Ok(())
}

pub fn is_installed(app_handle: &tauri::AppHandle) -> bool {
    engine_dir(app_handle)
        .map(|d| d.join("ComfyUI/main.py").is_file() && d.join("venv/bin/python").is_file())
        .unwrap_or(false)
}

pub fn seedvr2_model_present(app_handle: &tauri::AppHandle, model_file: &str) -> bool {
    engine_dir(app_handle)
        .map(|d| {
            d.join("ComfyUI/models/SEEDVR2").join(model_file).is_file()
                && d.join("ComfyUI/models/SEEDVR2/ema_vae_fp16.safetensors").is_file()
        })
        .unwrap_or(false)
}

async fn is_running() -> bool {
    reqwest::Client::new()
        .get(format!("{}/system_stats", base_url()))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Starts the engine if it isn't already running; waits until the API
/// responds. The child handle lives in AppState so the engine dies with
/// the app (or via `stop_engine`).
pub async fn ensure_running(
    app_handle: &tauri::AppHandle,
    engine_process: &Mutex<Option<std::process::Child>>,
) -> Result<()> {
    if is_running().await {
        return Ok(());
    }
    if !is_installed(app_handle) {
        return Err(anyhow!(
            "The generative engine is not installed. Install it from Settings → Model Library."
        ));
    }
    let dir = engine_dir(app_handle)?;
    let _ = app_handle.emit("comfy-progress", "Starting generative engine...");
    let child = std::process::Command::new(dir.join("venv/bin/python"))
        .arg("main.py")
        .arg("--listen")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(ENGINE_PORT.to_string())
        .current_dir(dir.join("ComfyUI"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("Could not start the engine: {}", e))?;
    *engine_process.lock().unwrap() = Some(child);

    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if is_running().await {
            return Ok(());
        }
    }
    Err(anyhow!("The generative engine did not become ready in time."))
}

pub fn stop(engine_process: &Mutex<Option<std::process::Child>>) {
    if let Some(mut child) = engine_process.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Uploads an image (PNG bytes) to the engine's input store.
async fn upload_image(client: &reqwest::Client, name: &str, png_bytes: Vec<u8>) -> Result<()> {
    let part = reqwest::multipart::Part::bytes(png_bytes)
        .file_name(name.to_string())
        .mime_str("image/png")?;
    let form = reqwest::multipart::Form::new()
        .part("image", part)
        .text("overwrite", "true");
    let resp = client
        .post(format!("{}/upload/image", base_url()))
        .multipart(form)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(anyhow!("Engine upload failed: {}", resp.status()));
    }
    Ok(())
}

/// Submits a workflow and returns the first output image's PNG bytes.
/// `on_progress` receives coarse status strings.
pub async fn run_workflow(
    prompt: Value,
    mut on_progress: impl FnMut(String),
) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let resp: Value = client
        .post(format!("{}/prompt", base_url()))
        .json(&json!({ "prompt": prompt }))
        .send()
        .await?
        .json()
        .await?;
    if let Some(node_errors) = resp.get("node_errors").filter(|e| {
        e.as_object().map(|o| !o.is_empty()).unwrap_or(false)
    }) {
        return Err(anyhow!("Engine rejected the workflow: {}", node_errors));
    }
    let prompt_id = resp
        .get("prompt_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Engine did not accept the job: {}", resp))?
        .to_string();

    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        on_progress(format!(
            "Generating... ({}s elapsed)",
            started.elapsed().as_secs()
        ));
        let history: Value = client
            .get(format!("{}/history/{}", base_url(), prompt_id))
            .send()
            .await?
            .json()
            .await?;
        let Some(entry) = history.get(&prompt_id) else { continue };
        let status = entry.pointer("/status/status_str").and_then(|v| v.as_str());
        if status == Some("error") {
            // Surface just the exception message, not the whole traceback.
            let detail = entry
                .pointer("/status/messages")
                .and_then(|m| m.as_array())
                .and_then(|msgs| {
                    msgs.iter().find_map(|m| {
                        m.get(1)
                            .and_then(|d| d.get("exception_message"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.lines().next().unwrap_or(s).to_string())
                    })
                })
                .unwrap_or_else(|| "unknown engine error".to_string());
            return Err(anyhow!("Generative engine failed: {}", detail));
        }
        let completed = entry
            .pointer("/status/completed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !completed {
            continue;
        }
        // Find the first output image.
        if let Some(outputs) = entry.get("outputs").and_then(|v| v.as_object()) {
            for node_out in outputs.values() {
                if let Some(img) = node_out
                    .get("images")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                {
                    let filename = img.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                    let subfolder = img.get("subfolder").and_then(|v| v.as_str()).unwrap_or("");
                    let bytes = client
                        .get(format!(
                            "{}/view?filename={}&subfolder={}&type=output",
                            base_url(),
                            filename,
                            subfolder
                        ))
                        .send()
                        .await?
                        .bytes()
                        .await?;
                    return Ok(bytes.to_vec());
                }
            }
        }
        return Err(anyhow!("Engine finished but produced no image."));
    }
}

pub fn sdxl_model_present(app_handle: &tauri::AppHandle) -> bool {
    engine_dir(app_handle)
        .map(|d| d.join("ComfyUI/models/checkpoints/sd_xl_base_1.0.safetensors").is_file())
        .unwrap_or(false)
}

/// Which generative fill pipeline a manifest requests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillKind {
    /// SDXL base with the latent-noise-mask recipe (fast, baseline).
    SdxlBase,
    /// SDXL + Fooocus inpaint patch (fast, much better blending).
    SdxlFooocus,
    /// Flux Fill dev (GGUF) — purpose-trained fill, best quality, slow.
    Flux,
}

impl FillKind {
    pub fn from_params(params: &Value) -> Self {
        match params.get("fill_workflow").and_then(|v| v.as_str()) {
            Some("fooocus") => FillKind::SdxlFooocus,
            Some("flux") => FillKind::Flux,
            _ => FillKind::SdxlBase,
        }
    }
}

fn fill_files_present(app_handle: &tauri::AppHandle, kind: FillKind) -> bool {
    let Ok(dir) = engine_dir(app_handle) else { return false };
    let m = dir.join("ComfyUI/models");
    match kind {
        FillKind::SdxlBase => m.join("checkpoints/sd_xl_base_1.0.safetensors").is_file(),
        FillKind::SdxlFooocus => {
            m.join("checkpoints/sd_xl_base_1.0.safetensors").is_file()
                && m.join("inpaint/inpaint_v26.fooocus.patch").is_file()
                && m.join("inpaint/fooocus_inpaint_head.pth").is_file()
        }
        FillKind::Flux => {
            m.join("unet/flux1-fill-dev-Q8_0.gguf").is_file()
                && m.join("clip/t5-v1_1-xxl-encoder-Q8_0.gguf").is_file()
                && m.join("clip/clip_l.safetensors").is_file()
                && m.join("vae/ae.safetensors").is_file()
        }
    }
}

/// Generative fill: inpaints the masked region of `image_png` guided by
/// `prompt` (empty prompt = fill from context). Different seeds give
/// genuinely different results — the source of real variants.
#[allow(clippy::too_many_arguments)]
pub async fn run_generative_fill(
    app_handle: &tauri::AppHandle,
    state: &AppState,
    kind: FillKind,
    image_png: Vec<u8>,
    mask_png: Vec<u8>,
    prompt: &str,
    seed: u64,
    mut on_progress: impl FnMut(String),
) -> Result<Vec<u8>> {
    if !fill_files_present(app_handle, kind) {
        return Err(anyhow!(
            "The selected fill model's files are missing from the engine's models folder."
        ));
    }
    ensure_running(app_handle, &state.comfy_process).await?;
    on_progress("Uploading image to engine...".to_string());
    let client = reqwest::Client::new();
    upload_image(&client, "rapidraw_fill_image.png", image_png).await?;
    upload_image(&client, "rapidraw_fill_mask.png", mask_png).await?;

    let positive = if prompt.trim().is_empty() {
        "seamless photographic continuation of the scene, natural, detailed".to_string()
    } else {
        prompt.to_string()
    };
    let negative = "blurry, low quality, artifacts, watermark, text";

    let workflow = match kind {
        // Refine-the-prefill recipe: encode the whole canvas (whose masked
        // area holds an edge-replicated hint), then rework only the masked
        // latents at high-but-not-full denoise.
        FillKind::SdxlBase => json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "rapidraw_fill_image.png"}},
            "2": {"class_type": "LoadImageMask",
                  "inputs": {"image": "rapidraw_fill_mask.png", "channel": "red"}},
            "3": {"class_type": "CheckpointLoaderSimple",
                  "inputs": {"ckpt_name": "sd_xl_base_1.0.safetensors"}},
            "4": {"class_type": "CLIPTextEncode", "inputs": {"text": positive, "clip": ["3", 1]}},
            "5": {"class_type": "CLIPTextEncode", "inputs": {"text": negative, "clip": ["3", 1]}},
            "6": {"class_type": "VAEEncode", "inputs": {"pixels": ["1", 0], "vae": ["3", 2]}},
            "6b": {"class_type": "SetLatentNoiseMask",
                   "inputs": {"samples": ["6", 0], "mask": ["2", 0]}},
            "7": {"class_type": "KSampler",
                  "inputs": {"model": ["3", 0], "positive": ["4", 0], "negative": ["5", 0],
                              "latent_image": ["6b", 0], "seed": seed, "steps": 28, "cfg": 6.0,
                              "sampler_name": "dpmpp_2m", "scheduler": "karras", "denoise": 0.85}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["3", 2]}},
            "9": {"class_type": "SaveImage",
                  "inputs": {"images": ["8", 0], "filename_prefix": "rapidraw_fill"}}
        }),
        // Fooocus patch turns base SDXL into a proper inpainting model.
        FillKind::SdxlFooocus => json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "rapidraw_fill_image.png"}},
            "2": {"class_type": "LoadImageMask",
                  "inputs": {"image": "rapidraw_fill_mask.png", "channel": "red"}},
            "3": {"class_type": "CheckpointLoaderSimple",
                  "inputs": {"ckpt_name": "sd_xl_base_1.0.safetensors"}},
            "4": {"class_type": "CLIPTextEncode", "inputs": {"text": positive, "clip": ["3", 1]}},
            "5": {"class_type": "CLIPTextEncode", "inputs": {"text": negative, "clip": ["3", 1]}},
            "6": {"class_type": "INPAINT_LoadFooocusInpaint",
                  "inputs": {"head": "fooocus_inpaint_head.pth",
                              "patch": "inpaint_v26.fooocus.patch"}},
            // Canonical graph (same as the Krita plugin): the patch node
            // gets the concat-latent, the sampler starts from the original
            // image latents. Feeding it VAEEncodeForInpaint's zeroed encode
            // instead produces banded garbage.
            "7": {"class_type": "INPAINT_VAEEncodeInpaintConditioning",
                  "inputs": {"positive": ["4", 0], "negative": ["5", 0], "vae": ["3", 2],
                              "pixels": ["1", 0], "mask": ["2", 0]}},
            "8": {"class_type": "INPAINT_ApplyFooocusInpaint",
                  "inputs": {"model": ["3", 0], "patch": ["6", 0], "latent": ["7", 2]}},
            "9": {"class_type": "KSampler",
                  "inputs": {"model": ["8", 0], "positive": ["7", 0], "negative": ["7", 1],
                              "latent_image": ["7", 3], "seed": seed, "steps": 28, "cfg": 6.0,
                              "sampler_name": "dpmpp_2m", "scheduler": "karras", "denoise": 1.0}},
            "10": {"class_type": "VAEDecode", "inputs": {"samples": ["9", 0], "vae": ["3", 2]}},
            "11": {"class_type": "SaveImage",
                   "inputs": {"images": ["10", 0], "filename_prefix": "rapidraw_fill"}}
        }),
        // Flux Fill: purpose-trained inpainting DiT (quantized), cfg 1 +
        // FluxGuidance, InpaintModelConditioning supplies image+mask.
        FillKind::Flux => json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "rapidraw_fill_image.png"}},
            "2": {"class_type": "LoadImageMask",
                  "inputs": {"image": "rapidraw_fill_mask.png", "channel": "red"}},
            "3": {"class_type": "UnetLoaderGGUF",
                  "inputs": {"unet_name": "flux1-fill-dev-Q8_0.gguf"}},
            "3c": {"class_type": "DualCLIPLoaderGGUF",
                   "inputs": {"clip_name1": "t5-v1_1-xxl-encoder-Q8_0.gguf",
                               "clip_name2": "clip_l.safetensors", "type": "flux"}},
            "3v": {"class_type": "VAELoader", "inputs": {"vae_name": "ae.safetensors"}},
            "4": {"class_type": "CLIPTextEncode", "inputs": {"text": positive, "clip": ["3c", 0]}},
            "4g": {"class_type": "FluxGuidance",
                   "inputs": {"conditioning": ["4", 0], "guidance": 30.0}},
            "5": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["3c", 0]}},
            "6": {"class_type": "InpaintModelConditioning",
                  "inputs": {"positive": ["4g", 0], "negative": ["5", 0], "vae": ["3v", 0],
                              "pixels": ["1", 0], "mask": ["2", 0], "noise_mask": true}},
            "7": {"class_type": "KSampler",
                  "inputs": {"model": ["3", 0], "positive": ["6", 0], "negative": ["6", 1],
                              "latent_image": ["6", 2], "seed": seed, "steps": 24, "cfg": 1.0,
                              "sampler_name": "euler", "scheduler": "normal", "denoise": 1.0}},
            "8": {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["3v", 0]}},
            "9": {"class_type": "SaveImage",
                  "inputs": {"images": ["8", 0], "filename_prefix": "rapidraw_fill"}}
        }),
    };
    run_workflow(workflow, &mut on_progress).await
}

/// SeedVR2 restoration: upscales/restores the whole image to
/// `target_short_edge` using the 3B fp16 model.
#[allow(clippy::too_many_arguments)]
pub async fn run_seedvr2(
    app_handle: &tauri::AppHandle,
    state: &AppState,
    model_file: &str,
    input_png: Vec<u8>,
    target_short_edge: u32,
    seed: u64,
    mut on_progress: impl FnMut(String),
) -> Result<Vec<u8>> {
    if !seedvr2_model_present(app_handle, model_file) {
        return Err(anyhow!(
            "SeedVR2 model weights are missing from the engine's models folder."
        ));
    }
    ensure_running(app_handle, &state.comfy_process).await?;
    on_progress("Uploading image to engine...".to_string());
    let client = reqwest::Client::new();
    let input_name = "rapidraw_seedvr2_input.png";
    upload_image(&client, input_name, input_png).await?;

    let prompt = json!({
        "1": {"class_type": "LoadImage", "inputs": {"image": input_name}},
        "2": {"class_type": "SeedVR2LoadDiTModel",
              "inputs": {"model": model_file, "device": "mps"}},
        // Tiled VAE keeps memory bounded at high resolutions — a full-frame
        // encode of a large photo crashes on MPS.
        "3": {"class_type": "SeedVR2LoadVAEModel",
              "inputs": {"model": "ema_vae_fp16.safetensors", "device": "mps",
                          "encode_tiled": true, "encode_tile_size": 1024,
                          "encode_tile_overlap": 128, "decode_tiled": true,
                          "decode_tile_size": 1024, "decode_tile_overlap": 128}},
        "4": {"class_type": "SeedVR2VideoUpscaler",
              "inputs": {"image": ["1", 0], "dit": ["2", 0], "vae": ["3", 0],
                          "seed": seed, "resolution": target_short_edge,
                          "max_resolution": 0, "batch_size": 1,
                          "uniform_batch_size": false, "color_correction": "lab"}},
        "5": {"class_type": "SaveImage",
              "inputs": {"images": ["4", 0], "filename_prefix": "rapidraw_seedvr2"}}
    });
    run_workflow(prompt, &mut on_progress).await
}


#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub installed: bool,
}

#[tauri::command]
pub fn get_engine_status(app_handle: tauri::AppHandle) -> EngineStatus {
    EngineStatus {
        installed: is_installed(&app_handle),
    }
}

#[tauri::command]
pub async fn install_ai_engine(app_handle: tauri::AppHandle) -> Result<(), String> {
    let progress_handle = app_handle.clone();
    install_engine(&app_handle, move |msg| {
        let _ = progress_handle.emit("engine-install-progress", msg);
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = app_handle.emit("engine-install-complete", ());
    Ok(())
}
