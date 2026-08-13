use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};

use crate::app_state::AppState;
use crate::model_registry::{
    DownloadSpec, ModelManifest, ModelProbe, ModelRegistry, TaskType, get_or_init_registry,
    probe_onnx_model,
};

/// Curated list of downloadable models, bundled with the app. New models
/// can be offered later by editing this file (or, without rebuilding, by
/// pointing `modelCatalogUrl` in settings at a remote copy).
const BUNDLED_CATALOG: &str = include_str!("../model_catalog.json");
const CONVERT_SCRIPT: &str = include_str!("../scripts/convert_model_to_onnx.py");

/// Extensions the conversion assistant accepts (PyTorch checkpoints).
pub const CONVERTIBLE_EXTENSIONS: [&str; 3] = ["pth", "safetensors", "ckpt"];

fn is_convertible_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    CONVERTIBLE_EXTENSIONS.iter().any(|e| lower.ends_with(&format!(".{e}")))
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub manifest: ModelManifest,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub size_bytes: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LibraryModelInfo {
    pub id: String,
    pub display_name: String,
    pub task_type: TaskType,
    pub description: String,
    pub size_bytes: u64,
    pub downloaded: bool,
    pub downloadable: bool,
    pub builtin: bool,
}

pub fn bundled_catalog() -> Vec<CatalogEntry> {
    serde_json::from_str(BUNDLED_CATALOG).expect("bundled model_catalog.json must parse")
}

async fn load_catalog(remote_url: Option<&str>) -> Vec<CatalogEntry> {
    if let Some(url) = remote_url {
        match reqwest::get(url).await {
            Ok(resp) => match resp.json::<Vec<CatalogEntry>>().await {
                Ok(entries) => return entries,
                Err(e) => log::warn!("Remote model catalog at {} is invalid: {}", url, e),
            },
            Err(e) => log::warn!("Could not fetch remote model catalog {}: {}", url, e),
        }
    }
    bundled_catalog()
}

fn library_view(registry: &ModelRegistry, catalog: &[CatalogEntry]) -> Vec<LibraryModelInfo> {
    // Catalog entries plus every registered model (builtins and manual
    // manifests), so the library shows one unified list.
    let mut seen: HashMap<String, LibraryModelInfo> = HashMap::new();

    for entry in catalog {
        let registered = registry.get(&entry.manifest.id);
        seen.insert(
            entry.manifest.id.clone(),
            LibraryModelInfo {
                id: entry.manifest.id.clone(),
                display_name: entry.manifest.display_name.clone(),
                task_type: entry.manifest.task_type,
                description: entry.description.clone(),
                size_bytes: entry.size_bytes,
                downloaded: registered.as_ref().map(|m| m.available).unwrap_or(false),
                downloadable: entry.manifest.download.is_some(),
                builtin: registered.map(|m| m.builtin).unwrap_or(false),
            },
        );
    }

    for info in registry.list(None) {
        if info.task_type == TaskType::Test {
            continue;
        }
        seen.entry(info.id.clone()).or_insert(LibraryModelInfo {
            id: info.id.clone(),
            display_name: info.display_name.clone(),
            task_type: info.task_type,
            description: info
                .params
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            size_bytes: info
                .params
                .get("size_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            downloaded: info.available,
            downloadable: true,
            builtin: info.builtin,
        });
    }

    let mut list: Vec<LibraryModelInfo> = seen.into_values().collect();
    list.sort_by(|a, b| {
        format!("{:?}{}", a.task_type, a.display_name).cmp(&format!("{:?}{}", b.task_type, b.display_name))
    });
    list
}

fn write_manifest_file(registry: &ModelRegistry, manifest: &ModelManifest) -> Result<()> {
    let manifests_dir = registry.models_dir().join("manifests");
    fs::create_dir_all(&manifests_dir)?;
    let path = manifests_dir.join(format!("{}.json", manifest.id));
    fs::write(&path, serde_json::to_string_pretty(manifest)?)?;
    Ok(())
}

#[tauri::command]
pub async fn get_model_library(
    refresh: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<LibraryModelInfo>, String> {
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;
    let remote_url = if refresh.unwrap_or(false) {
        crate::app_settings::load_settings(app_handle.clone())
            .ok()
            .and_then(|s| s.model_catalog_url)
    } else {
        None
    };
    let catalog = load_catalog(remote_url.as_deref()).await;
    registry.rescan();
    Ok(library_view(&registry, &catalog))
}

#[tauri::command]
pub async fn download_library_model(
    model_id: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<LibraryModelInfo>, String> {
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;
    let remote_url = crate::app_settings::load_settings(app_handle.clone())
        .ok()
        .and_then(|s| s.model_catalog_url);
    let catalog = load_catalog(remote_url.as_deref()).await;

    // Catalog models that aren't registered yet need their manifest written
    // first so the registry can resolve and download them.
    if registry.get(&model_id).is_none() {
        let entry = catalog
            .iter()
            .find(|e| e.manifest.id == model_id)
            .ok_or_else(|| format!("Model '{}' is not in the catalog", model_id))?;
        write_manifest_file(&registry, &entry.manifest).map_err(|e| e.to_string())?;
        registry.rescan();
    }

    registry
        .ensure_available(&app_handle, &model_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(library_view(&registry, &catalog))
}

#[tauri::command]
pub async fn delete_library_model(
    model_id: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<LibraryModelInfo>, String> {
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;
    let model = registry
        .get(&model_id)
        .ok_or_else(|| format!("Model '{}' is not registered", model_id))?;

    registry.unload(&model_id);
    if model.weight_path.is_file() {
        fs::remove_file(&model.weight_path).map_err(|e| e.to_string())?;
    }
    for path in model.aux_paths.values() {
        if path.is_file() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    registry.rescan();

    let catalog = load_catalog(None).await;
    Ok(library_view(&registry, &catalog))
}

/// Builds manifest params from a successful probe.
fn params_from_probe(probe: &ModelProbe) -> serde_json::Value {
    match probe.fixed_size {
        Some((h, w)) => serde_json::json!({
            "scale_factor": probe.scale_factor,
            "input_height": h,
            "input_width": w,
            "tile_overlap": 32,
        }),
        None => serde_json::json!({
            "scale_factor": probe.scale_factor,
            "tile_size": 512,
            "tile_overlap": 16,
        }),
    }
}

async fn python_has_spandrel(python: &Path) -> bool {
    tokio::process::Command::new(python)
        .args(["-c", "import spandrel, torch, onnx"])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn pip_install(python: &Path, packages: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new(python)
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg("--quiet")
        .args(packages)
        .output()
        .await
        .map_err(|e| format!("Could not run pip: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "Installing conversion packages failed: {}",
            String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("unknown error")
        ));
    }
    Ok(())
}

/// Finds (or creates) a Python environment with the conversion toolchain.
/// First run downloads PyTorch (~2 GB), so progress is reported via the
/// "convert-progress" event.
async fn ensure_convert_env(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = app_handle.path().app_data_dir() {
        candidates.push(dir.join("pyenv/bin/python3"));
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join(".local/rapidraw-export-venv/bin/python3"));
    }

    for python in &candidates {
        if python.is_file() {
            if python_has_spandrel(python).await {
                return Ok(python.clone());
            }
            let _ = app_handle.emit("convert-progress", "Installing conversion packages...");
            if pip_install(python, &["spandrel", "spandrel_extra_arches", "onnx", "packaging", "einops"]).await.is_ok()
                && python_has_spandrel(python).await
            {
                return Ok(python.clone());
            }
        }
    }

    let venv_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("pyenv");
    let _ = app_handle.emit(
        "convert-progress",
        "Setting up conversion tools (one-time download, ~2 GB)...",
    );
    let output = tokio::process::Command::new("python3")
        .args(["-m", "venv"])
        .arg(&venv_dir)
        .output()
        .await
        .map_err(|_| {
            "python3 was not found. Install the Xcode Command Line Tools (xcode-select --install) \
             and try again."
                .to_string()
        })?;
    if !output.status.success() {
        return Err(format!(
            "Could not create the conversion environment: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let python = venv_dir.join("bin/python3");
    pip_install(&python, &["--upgrade", "pip"]).await?;
    pip_install(&python, &["torch", "spandrel", "spandrel_extra_arches", "onnx", "packaging", "einops"])
        .await?;
    if !python_has_spandrel(&python).await {
        return Err("Conversion environment setup did not complete correctly.".to_string());
    }
    Ok(python)
}

#[derive(Deserialize)]
struct ConvertResult {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    architecture: Option<String>,
}

/// Converts a PyTorch checkpoint into `<models_dir>/<slug>.onnx`.
async fn convert_checkpoint_to_onnx(
    app_handle: &tauri::AppHandle,
    registry: &ModelRegistry,
    source: &Path,
    slug: &str,
) -> Result<String, String> {
    let python = ensure_convert_env(app_handle).await?;

    let script_path = std::env::temp_dir().join("rapidraw_convert_model.py");
    fs::write(&script_path, CONVERT_SCRIPT).map_err(|e| e.to_string())?;

    let out_name = format!("{}.onnx", slug);
    let out_path = registry.models_dir().join(&out_name);
    let _ = app_handle.emit("convert-progress", "Converting model to ONNX...");

    let output = tokio::process::Command::new(&python)
        .arg(&script_path)
        .arg("--input")
        .arg(source)
        .arg("--out")
        .arg(&out_path)
        .output()
        .await
        .map_err(|e| format!("Could not run the converter: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: Option<ConvertResult> =
        stdout.lines().last().and_then(|l| serde_json::from_str(l).ok());
    match result {
        Some(r) if r.ok => {
            if let Some(arch) = r.architecture {
                log::info!("Converted {} ({}) to ONNX", source.display(), arch);
            }
            Ok(out_name)
        }
        Some(r) => {
            let _ = fs::remove_file(&out_path);
            Err(r.error.unwrap_or_else(|| "Conversion failed".to_string()))
        }
        None => {
            let _ = fs::remove_file(&out_path);
            Err(format!(
                "Conversion crashed: {}",
                String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("unknown error")
            ))
        }
    }
}

fn slug_from_display_name(display_name: &str) -> Result<String, String> {
    let id: String = display_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if id.is_empty() {
        return Err("Display name must contain letters or digits".to_string());
    }
    Ok(id)
}

/// Registers a model from a `.onnx` file on disk: copies it (and a sibling
/// external-data file, if any) into the models folder and writes a
/// manifest. This is the path for self-converted or manually downloaded
/// models.
#[tauri::command]
pub async fn add_model_from_file(
    file_path: String,
    display_name: String,
    task_type: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<LibraryModelInfo>, String> {
    let task = TaskType::parse(&task_type).ok_or_else(|| format!("Unknown task '{}'", task_type))?;
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;

    let source = std::path::Path::new(&file_path);
    if !source.is_file() {
        return Err(format!("File not found: {}", file_path));
    }
    let source_name = source
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or("Invalid file name")?
        .to_string();

    let id = slug_from_display_name(&display_name)?;
    if registry.get(&id).is_some() {
        return Err(format!("A model with id '{}' already exists", id));
    }

    // PyTorch checkpoints go through the conversion assistant; anything
    // else must already be ONNX.
    let filename = if is_convertible_file(&source_name) {
        convert_checkpoint_to_onnx(&app_handle, &registry, source, &id).await?
    } else if source_name.to_ascii_lowercase().ends_with(".onnx") {
        source_name
    } else {
        return Err(
            "Unsupported file type. Choose a .onnx model, or a .pth/.safetensors/.ckpt \
             checkpoint to convert automatically. Note: diffusion checkpoints (SeedVR2, \
             Stable Diffusion, Flux…) are one part of a multi-model pipeline and need a \
             diffusion engine — RapidRAW runs self-contained image models only."
                .to_string(),
        );
    };

    let dest = registry.models_dir().join(&filename);
    if source != dest && !dest.is_file() {
        fs::copy(source, &dest).map_err(|e| format!("Could not copy model file: {}", e))?;
    }

    // Models with external weights keep them in a sibling "<name>.onnx.data"
    // file referenced by relative name — bring it along.
    let mut aux_files = HashMap::new();
    let data_name = format!("{}.data", filename);
    let data_source = source.with_file_name(&data_name);
    if data_source.is_file() {
        let aux_dest = registry.models_dir().join(&data_name);
        if data_source != aux_dest {
            fs::copy(&data_source, &aux_dest)
                .map_err(|e| format!("Could not copy model data file: {}", e))?;
        }
        aux_files.insert("data".to_string(), data_name);
    }

    // Validate the model and detect its parameters before registering it.
    let probe_path = dest.clone();
    let probe = tokio::task::spawn_blocking(move || probe_onnx_model(&probe_path))
        .await
        .map_err(|e| e.to_string())?;
    let probe = match probe {
        Ok(p) => p,
        Err(e) => {
            let _ = fs::remove_file(&dest);
            if let Some(name) = aux_files.get("data") {
                let _ = fs::remove_file(registry.models_dir().join(name));
            }
            return Err(format!("This model can't be used: {}", e));
        }
    };

    let manifest = ModelManifest {
        id,
        display_name,
        task_type: task,
        file_path: filename,
        aux_files,
        download: None,
        aux_downloads: HashMap::new(),
        params: params_from_probe(&probe),
    };
    write_manifest_file(&registry, &manifest).map_err(|e| e.to_string())?;
    registry.rescan();

    let catalog = load_catalog(None).await;
    Ok(library_view(&registry, &catalog))
}

/// Registers a model from a direct download URL: fetches the file, computes
/// its checksum, and writes a manifest so it appears like any other model.
async fn download_to_models_dir(
    registry: &ModelRegistry,
    url: &str,
    filename: &str,
) -> Result<DownloadSpec, String> {
    let dest = registry.models_dir().join(filename);
    let resp = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Download failed with status {} for {}", resp.status(), url));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
    Ok(DownloadSpec { url: url.to_string(), sha256 })
}

/// Probes a freshly downloaded model; on failure removes the files and
/// returns a user-facing error.
async fn probe_or_cleanup(
    registry: &ModelRegistry,
    filename: &str,
    extra_files: &[String],
) -> Result<ModelProbe, String> {
    let path = registry.models_dir().join(filename);
    let probe_path = path.clone();
    let probe = tokio::task::spawn_blocking(move || probe_onnx_model(&probe_path))
        .await
        .map_err(|e| e.to_string())?;
    match probe {
        Ok(p) => Ok(p),
        Err(e) => {
            let _ = fs::remove_file(&path);
            for f in extra_files {
                let _ = fs::remove_file(registry.models_dir().join(f));
            }
            Err(format!("This model can't be used: {}", e))
        }
    }
}

#[tauri::command]
pub async fn add_model_from_url(
    url: String,
    display_name: String,
    task_type: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<LibraryModelInfo>, String> {
    let task = TaskType::parse(&task_type).ok_or_else(|| format!("Unknown task '{}'", task_type))?;
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;

    let filename = url
        .split('/')
        .next_back()
        .and_then(|s| s.split('?').next())
        .filter(|s| s.to_ascii_lowercase().ends_with(".onnx"))
        .ok_or("The URL must point directly to a .onnx file")?
        .to_string();

    let id = slug_from_display_name(&display_name)?;
    if registry.get(&id).is_some() {
        return Err(format!("A model with id '{}' already exists", id));
    }

    let download = download_to_models_dir(&registry, &url, &filename).await?;
    let probe = probe_or_cleanup(&registry, &filename, &[]).await?;

    let manifest = ModelManifest {
        id,
        display_name,
        task_type: task,
        file_path: filename,
        aux_files: HashMap::new(),
        download: Some(download),
        aux_downloads: HashMap::new(),
        params: params_from_probe(&probe),
    };
    write_manifest_file(&registry, &manifest).map_err(|e| e.to_string())?;
    registry.rescan();

    let catalog = load_catalog(None).await;
    Ok(library_view(&registry, &catalog))
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelFile {
    pub filename: String,
    pub size_bytes: u64,
    pub data_filename: Option<String>,
    pub needs_conversion: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RemoteModelRepo {
    pub repo_id: String,
    pub downloads: u64,
    pub likes: u64,
    pub files: Vec<RemoteModelFile>,
}

/// Searches Hugging Face for repositories containing .onnx files. Only
/// repos that actually ship ONNX files are returned.
#[tauri::command]
pub async fn search_remote_models(query: String) -> Result<Vec<RemoteModelRepo>, String> {
    let encoded_query: String = query
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c.to_string()
            } else {
                c.to_string()
                    .bytes()
                    .map(|b| format!("%{:02X}", b))
                    .collect()
            }
        })
        .collect();
    let client = reqwest::Client::new();
    let repos: serde_json::Value = client
        .get(format!(
            "https://huggingface.co/api/models?search={}&limit=12",
            encoded_query
        ))
        .header("User-Agent", "RapidRAW-ModelLibrary")
        .send()
        .await
        .map_err(|e| format!("Search failed: {}", e))?
        .json()
        .await
        .map_err(|e| format!("Search returned invalid data: {}", e))?;

    let repo_meta: Vec<(String, u64, u64)> = repos
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some((
                        r.get("id")?.as_str()?.to_string(),
                        r.get("downloads").and_then(|v| v.as_u64()).unwrap_or(0),
                        r.get("likes").and_then(|v| v.as_u64()).unwrap_or(0),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let detail_futures = repo_meta.iter().map(|(id, _, _)| {
        let client = client.clone();
        let url = format!("https://huggingface.co/api/models/{}?blobs=true", id);
        async move {
            client
                .get(&url)
                .header("User-Agent", "RapidRAW-ModelLibrary")
                .send()
                .await
                .ok()?
                .json::<serde_json::Value>()
                .await
                .ok()
        }
    });
    let details = futures::future::join_all(detail_futures).await;

    let mut results = Vec::new();
    for ((repo_id, downloads, likes), detail) in repo_meta.into_iter().zip(details) {
        let Some(detail) = detail else { continue };
        let siblings: Vec<(String, u64)> = detail
            .get("siblings")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        Some((
                            s.get("rfilename")?.as_str()?.to_string(),
                            s.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Ready-to-run ONNX files, plus PyTorch checkpoints small enough to
        // plausibly be single-image models (huge ones are diffusion weights
        // that the converter would reject after a long download anyway).
        const MAX_CONVERTIBLE_BYTES: u64 = 2_000_000_000;
        let files: Vec<RemoteModelFile> = siblings
            .iter()
            .filter(|(name, size)| {
                name.to_ascii_lowercase().ends_with(".onnx")
                    || (is_convertible_file(name) && *size > 0 && *size < MAX_CONVERTIBLE_BYTES)
            })
            .map(|(name, size)| {
                let data_name = format!("{}.data", name);
                let data = siblings.iter().find(|(n, _)| *n == data_name);
                RemoteModelFile {
                    filename: name.clone(),
                    size_bytes: size + data.map(|(_, s)| *s).unwrap_or(0),
                    data_filename: data.map(|(n, _)| n.clone()),
                    needs_conversion: is_convertible_file(name),
                }
            })
            .collect();

        if !files.is_empty() {
            results.push(RemoteModelRepo { repo_id, downloads, likes, files });
        }
    }
    results.sort_by_key(|r| std::cmp::Reverse(r.downloads));
    Ok(results)
}

/// Downloads a model file found via search, validates it by probing, and
/// registers it with auto-detected parameters.
#[tauri::command]
pub async fn install_remote_model(
    repo_id: String,
    filename: String,
    data_filename: Option<String>,
    display_name: String,
    task_type: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<LibraryModelInfo>, String> {
    let task = TaskType::parse(&task_type).ok_or_else(|| format!("Unknown task '{}'", task_type))?;
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;

    let id = slug_from_display_name(&display_name)?;
    if registry.get(&id).is_some() {
        return Err(format!("A model with id '{}' already exists", id));
    }

    // rfilename may sit in a subfolder of the repo; store flat locally.
    let source_name = filename.split('/').next_back().unwrap_or(&filename).to_string();
    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}?download=true",
        repo_id, filename
    );

    let (local_name, download, aux_files, aux_downloads) = if is_convertible_file(&source_name) {
        // Download the checkpoint to a temp location, convert, discard it.
        let tmp = std::env::temp_dir().join(&source_name);
        let resp = reqwest::get(&url).await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Download failed with status {}", resp.status()));
        }
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        let converted = convert_checkpoint_to_onnx(&app_handle, &registry, &tmp, &id).await;
        let _ = fs::remove_file(&tmp);
        // Converted locally: no re-download source recorded on purpose (the
        // ONNX only exists on this machine).
        (converted?, None, HashMap::new(), HashMap::new())
    } else {
        let download = download_to_models_dir(&registry, &url, &source_name).await?;
        let mut aux_files = HashMap::new();
        let mut aux_downloads = HashMap::new();
        if let Some(data_rfile) = data_filename {
            let local_data = format!("{}.data", source_name);
            let data_url = format!(
                "https://huggingface.co/{}/resolve/main/{}?download=true",
                repo_id, data_rfile
            );
            let spec = download_to_models_dir(&registry, &data_url, &local_data).await?;
            aux_files.insert("data".to_string(), local_data.clone());
            aux_downloads.insert("data".to_string(), spec);
        }
        (source_name, Some(download), aux_files, aux_downloads)
    };

    let cleanup: Vec<String> = aux_files.values().cloned().collect();
    let probe = probe_or_cleanup(&registry, &local_name, &cleanup).await?;

    let manifest = ModelManifest {
        id,
        display_name,
        task_type: task,
        file_path: local_name,
        aux_files,
        download,
        aux_downloads,
        params: params_from_probe(&probe),
    };
    write_manifest_file(&registry, &manifest).map_err(|e| e.to_string())?;
    registry.rescan();

    let catalog = load_catalog(None).await;
    Ok(library_view(&registry, &catalog))
}
