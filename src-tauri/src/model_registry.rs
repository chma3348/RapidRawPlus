use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use ndarray::{Array, IxDyn};
use ort::session::Session;
use ort::value::Tensor;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex as TokioMutex;

use crate::app_state::AppState;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Upscale,
    Deblur,
    Restore,
    Mask,
    Inpaint,
    Test,
}

impl TaskType {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DownloadSpec {
    pub url: String,
    pub sha256: String,
}

/// A model manifest, either built in or loaded from
/// `<app_data>/models/manifests/*.json`. `file_path` (and any `aux_files`
/// values) are resolved relative to `<app_data>/models` unless absolute.
/// `download` / `aux_downloads` let missing weight files be fetched on
/// demand; without them a model is simply unavailable until the user drops
/// the file in place.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModelManifest {
    pub id: String,
    pub display_name: String,
    pub task_type: TaskType,
    pub file_path: String,
    #[serde(default)]
    pub aux_files: HashMap<String, String>,
    #[serde(default)]
    pub download: Option<DownloadSpec>,
    #[serde(default)]
    pub aux_downloads: HashMap<String, DownloadSpec>,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl ModelManifest {
    fn downloadable(&self) -> bool {
        self.download.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct RegisteredModel {
    pub manifest: ModelManifest,
    pub weight_path: PathBuf,
    pub aux_paths: HashMap<String, PathBuf>,
    pub available: bool,
    pub builtin: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub task_type: TaskType,
    pub available: bool,
    pub builtin: bool,
    pub params: serde_json::Value,
}

impl From<&RegisteredModel> for ModelInfo {
    fn from(m: &RegisteredModel) -> Self {
        ModelInfo {
            id: m.manifest.id.clone(),
            display_name: m.manifest.display_name.clone(),
            task_type: m.manifest.task_type,
            available: m.available,
            builtin: m.builtin,
            params: m.manifest.params.clone(),
        }
    }
}

pub struct ModelRegistry {
    models_dir: PathBuf,
    entries: Mutex<HashMap<String, RegisteredModel>>,
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
    download_lock: TokioMutex<()>,
}

/// Manifests for the models RapidRAW already ships with, so existing
/// features can be driven through the registry.
fn builtin_manifests() -> Vec<ModelManifest> {
    use crate::ai_processing as ai;
    vec![
        ModelManifest {
            id: "sam-vit-b".to_string(),
            display_name: "Segment Anything (ViT-B)".to_string(),
            task_type: TaskType::Mask,
            file_path: ai::ENCODER_FILENAME.to_string(),
            aux_files: HashMap::from([("decoder".to_string(), ai::DECODER_FILENAME.to_string())]),
            download: Some(DownloadSpec {
                url: ai::ENCODER_URL.to_string(),
                sha256: ai::ENCODER_SHA256.to_string(),
            }),
            aux_downloads: HashMap::from([(
                "decoder".to_string(),
                DownloadSpec {
                    url: ai::DECODER_URL.to_string(),
                    sha256: ai::DECODER_SHA256.to_string(),
                },
            )]),
            params: json!({ "mask_subtype": "subject", "input_size": 1024 }),
        },
        ModelManifest {
            id: "u2net-foreground".to_string(),
            display_name: "U-2-Net (Foreground)".to_string(),
            task_type: TaskType::Mask,
            file_path: ai::U2NETP_FILENAME.to_string(),
            aux_files: HashMap::new(),
            download: Some(DownloadSpec {
                url: ai::U2NETP_URL.to_string(),
                sha256: ai::U2NETP_SHA256.to_string(),
            }),
            aux_downloads: HashMap::new(),
            params: json!({ "mask_subtype": "foreground", "input_size": 320 }),
        },
        ModelManifest {
            id: "u2net-sky".to_string(),
            display_name: "U-2-Net (Sky)".to_string(),
            task_type: TaskType::Mask,
            file_path: ai::SKYSEG_FILENAME.to_string(),
            aux_files: HashMap::new(),
            download: Some(DownloadSpec {
                url: ai::SKYSEG_URL.to_string(),
                sha256: ai::SKYSEG_SHA256.to_string(),
            }),
            aux_downloads: HashMap::new(),
            params: json!({ "mask_subtype": "sky", "input_size": 320 }),
        },
        ModelManifest {
            id: "depth-anything-v2-vits".to_string(),
            display_name: "Depth Anything V2 (Small)".to_string(),
            task_type: TaskType::Mask,
            file_path: ai::DEPTH_FILENAME.to_string(),
            aux_files: HashMap::new(),
            download: Some(DownloadSpec {
                url: ai::DEPTH_URL.to_string(),
                sha256: ai::DEPTH_SHA256.to_string(),
            }),
            aux_downloads: HashMap::new(),
            params: json!({ "mask_subtype": "depth", "input_size": 518 }),
        },
        ModelManifest {
            id: "realesrgan-x4plus".to_string(),
            display_name: "Real-ESRGAN x4plus (FP16)".to_string(),
            task_type: TaskType::Upscale,
            file_path: "realesrgan_x4plus_fp16.onnx".to_string(),
            aux_files: HashMap::new(),
            download: Some(DownloadSpec {
                url: "https://huggingface.co/tamnvcc/RealESRGAN-onnx/resolve/main/onnx/RealESRGAN_x4plus.fp16.onnx?download=true".to_string(),
                sha256: "0a06c68f463a14bf5563b78d77d61ba4394024e148383c4308d6d3783eac2dc5"
                    .to_string(),
            }),
            aux_downloads: HashMap::new(),
            params: json!({ "scale_factor": 4, "tile_size": 512, "tile_overlap": 16 }),
        },
        ModelManifest {
            id: "realesr-general-x4v3".to_string(),
            display_name: "Real-ESRGAN General x4v3 (Fast)".to_string(),
            task_type: TaskType::Upscale,
            file_path: "realesr_general_x4v3.onnx".to_string(),
            aux_files: HashMap::new(),
            download: Some(DownloadSpec {
                url: "https://huggingface.co/Heliosoph/realesrgan-onnx/resolve/main/realesr-general-x4v3.onnx?download=true".to_string(),
                sha256: "09b757accd747d7e423c1d352b3e8f23e77cc5742d04bae958d4eb8082b76fa4"
                    .to_string(),
            }),
            aux_downloads: HashMap::new(),
            params: json!({ "scale_factor": 4, "tile_size": 512, "tile_overlap": 16 }),
        },
        ModelManifest {
            id: "nafnet-gopro-deblur".to_string(),
            display_name: "NAFNet Deblur (GoPro)".to_string(),
            task_type: TaskType::Deblur,
            file_path: "deblurring_nafnet_2025may.onnx".to_string(),
            aux_files: HashMap::new(),
            download: Some(DownloadSpec {
                url: "https://huggingface.co/opencv/deblurring_nafnet/resolve/main/deblurring_nafnet_2025may.onnx?download=true".to_string(),
                sha256: "07263f416febecce10193dd648e950b22e397cf521eedab1a114ef77b2bc9587"
                    .to_string(),
            }),
            aux_downloads: HashMap::new(),
            // This export needs both spatial dims >= ~384, so tiles are fed
            // at a fixed 512x512 with edge padding.
            params: json!({ "scale_factor": 1, "input_height": 512, "input_width": 512, "tile_overlap": 32 }),
        },
        ModelManifest {
            id: "scunet-real-gan".to_string(),
            display_name: "SCUNet Artifact Removal (Real GAN)".to_string(),
            task_type: TaskType::Restore,
            file_path: "scunet_color_real_gan.onnx".to_string(),
            aux_files: HashMap::from([(
                "data".to_string(),
                "scunet_color_real_gan.onnx.data".to_string(),
            )]),
            download: Some(DownloadSpec {
                url: "https://huggingface.co/Heliosoph/scunet-onnx/resolve/main/scunet_color_real_gan.onnx?download=true".to_string(),
                sha256: "e2bf014ad711f99322f2a94304431ffd6766b79ff00bcebb87d1a4f71c127b2d"
                    .to_string(),
            }),
            aux_downloads: HashMap::from([(
                "data".to_string(),
                DownloadSpec {
                    url: "https://huggingface.co/Heliosoph/scunet-onnx/resolve/main/scunet_color_real_gan.onnx.data?download=true".to_string(),
                    sha256: "1e8d582046abf4a773215445c248d0f54751a57d69d2a2312ec59f1150256b79"
                        .to_string(),
                },
            )]),
            // Swin windowing needs multiple-of-64 dims -> fixed tiles.
            params: json!({ "scale_factor": 1, "input_height": 512, "input_width": 512, "tile_overlap": 32 }),
        },
        ModelManifest {
            id: "esdnet-l-uhdm".to_string(),
            display_name: "ESDNet-L Demoiré (screen patterns)".to_string(),
            task_type: TaskType::Restore,
            file_path: "esdnet_l_uhdm_demoire.onnx".to_string(),
            aux_files: HashMap::new(),
            // Converted locally from the official PyTorch checkpoint (no
            // public ONNX exists) — see scripts/export_esdnet_onnx.py.
            download: None,
            aux_downloads: HashMap::new(),
            // Demoiréing is a global correction: tiles disagree and show as
            // boxes, so run the whole image in one pass (padded to /32),
            // falling back to large blended tiles only for huge images.
            params: json!({ "scale_factor": 1, "single_pass": true, "pad_multiple": 32, "input_height": 1024, "input_width": 1024, "tile_overlap": 64 }),
        },
        ModelManifest {
            id: "seedvr2-3b".to_string(),
            display_name: "SeedVR2 3B (Generative)".to_string(),
            task_type: TaskType::Restore,
            // Lives in the managed ComfyUI engine's model folder (sibling of
            // the models dir); availability = weights on disk.
            file_path: "../comfy/ComfyUI/models/SEEDVR2/seedvr2_ema_3b_fp16.safetensors"
                .to_string(),
            aux_files: HashMap::from([(
                "vae".to_string(),
                "../comfy/ComfyUI/models/SEEDVR2/ema_vae_fp16.safetensors".to_string(),
            )]),
            download: None,
            aux_downloads: HashMap::new(),
            params: json!({ "engine": "comfy", "resolution": 1080, "size_bytes": 7_284_000_000u64, "description": "Generative photo restoration (fast tier)" }),
        },
        ModelManifest {
            id: "seedvr2-7b".to_string(),
            display_name: "SeedVR2 7B (Best)".to_string(),
            task_type: TaskType::Restore,
            file_path: "../comfy/ComfyUI/models/SEEDVR2/seedvr2_ema_7b_fp16.safetensors"
                .to_string(),
            aux_files: HashMap::from([(
                "vae".to_string(),
                "../comfy/ComfyUI/models/SEEDVR2/ema_vae_fp16.safetensors".to_string(),
            )]),
            download: None,
            aux_downloads: HashMap::new(),
            params: json!({ "engine": "comfy", "resolution": 1080, "size_bytes": 16_984_000_000u64, "description": "Generative photo restoration (best quality, slower)" }),
        },
        ModelManifest {
            id: "sdxl-fill".to_string(),
            display_name: "SDXL Generative Fill".to_string(),
            task_type: TaskType::Inpaint,
            file_path: "../comfy/ComfyUI/models/checkpoints/sd_xl_base_1.0.safetensors"
                .to_string(),
            aux_files: HashMap::new(),
            download: None,
            aux_downloads: HashMap::new(),
            params: json!({ "engine": "comfy", "size_bytes": 6_938_078_334u64, "description": "Generative fill for Expand and Remove (baseline)" }),
        },
        ModelManifest {
            id: "sdxl-fill-fooocus".to_string(),
            display_name: "SDXL Fill+ (Fooocus)".to_string(),
            task_type: TaskType::Inpaint,
            file_path: "../comfy/ComfyUI/models/inpaint/inpaint_v26.fooocus.patch".to_string(),
            aux_files: HashMap::from([
                (
                    "head".to_string(),
                    "../comfy/ComfyUI/models/inpaint/fooocus_inpaint_head.pth".to_string(),
                ),
                (
                    "checkpoint".to_string(),
                    "../comfy/ComfyUI/models/checkpoints/sd_xl_base_1.0.safetensors".to_string(),
                ),
            ]),
            download: None,
            aux_downloads: HashMap::new(),
            params: json!({ "engine": "comfy", "fill_workflow": "fooocus", "size_bytes": 8_261_000_000u64, "description": "Much better fill blending; includes SDXL base" }),
        },
        ModelManifest {
            id: "flux-fill".to_string(),
            display_name: "Flux Fill (Best)".to_string(),
            task_type: TaskType::Inpaint,
            file_path: "../comfy/ComfyUI/models/unet/flux1-fill-dev-Q8_0.gguf".to_string(),
            aux_files: HashMap::from([
                (
                    "t5".to_string(),
                    "../comfy/ComfyUI/models/clip/t5-v1_1-xxl-encoder-Q8_0.gguf".to_string(),
                ),
                (
                    "clip_l".to_string(),
                    "../comfy/ComfyUI/models/clip/clip_l.safetensors".to_string(),
                ),
                (
                    "vae".to_string(),
                    "../comfy/ComfyUI/models/vae/ae.safetensors".to_string(),
                ),
            ]),
            download: None,
            aux_downloads: HashMap::new(),
            params: json!({ "engine": "comfy", "fill_workflow": "flux", "size_bytes": 18_368_000_000u64, "description": "Firefly-class generative fill (best, ~2 min per fill)" }),
        },
        ModelManifest {
            id: "lama-fp16".to_string(),
            display_name: "LaMa (FP16)".to_string(),
            task_type: TaskType::Inpaint,
            file_path: ai::LAMA_FILENAME.to_string(),
            aux_files: HashMap::new(),
            download: Some(DownloadSpec {
                url: ai::LAMA_URL.to_string(),
                sha256: ai::LAMA_SHA256.to_string(),
            }),
            aux_downloads: HashMap::new(),
            params: json!({ "pad_multiple": 64, "max_dim": 768 }),
        },
    ]
}

impl ModelRegistry {
    pub fn new(models_dir: PathBuf) -> Self {
        let registry = ModelRegistry {
            models_dir,
            entries: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            download_lock: TokioMutex::new(()),
        };
        registry.rescan();
        registry
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    fn manifests_dir(&self) -> PathBuf {
        self.models_dir.join("manifests")
    }

    fn resolve_path(&self, file_path: &str) -> PathBuf {
        let p = Path::new(file_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            // Normalize ".." lexically: engine manifests use paths like
            // "../comfy/..." meaning "sibling of the models dir". If the
            // models dir is a symlink (users may relocate their weights),
            // letting the filesystem resolve ".." would walk out of the
            // link's TARGET instead of its location.
            let mut out = self.models_dir.clone();
            for c in p.components() {
                match c {
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => out.push(other),
                }
            }
            out
        }
    }

    fn register(&self, manifest: ModelManifest, builtin: bool) {
        let weight_path = self.resolve_path(&manifest.file_path);
        let aux_paths: HashMap<String, PathBuf> = manifest
            .aux_files
            .iter()
            .map(|(k, v)| (k.clone(), self.resolve_path(v)))
            .collect();
        let available = weight_path.is_file() && aux_paths.values().all(|p| p.is_file());
        let id = manifest.id.clone();
        self.entries.lock().unwrap().insert(
            id,
            RegisteredModel {
                manifest,
                weight_path,
                aux_paths,
                available,
                builtin,
            },
        );
    }

    /// Re-reads all manifests and re-checks weight availability. Loaded
    /// sessions are kept; they are only dropped via `unload`/`unload_all`.
    pub fn rescan(&self) {
        self.entries.lock().unwrap().clear();

        for manifest in builtin_manifests() {
            self.register(manifest, true);
        }

        let manifests_dir = self.manifests_dir();
        if let Err(e) = fs::create_dir_all(&manifests_dir) {
            log::warn!("Could not create manifests dir {:?}: {}", manifests_dir, e);
            return;
        }
        let read_dir = match fs::read_dir(&manifests_dir) {
            Ok(rd) => rd,
            Err(e) => {
                log::warn!("Could not read manifests dir {:?}: {}", manifests_dir, e);
                return;
            }
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|s| serde_json::from_str::<ModelManifest>(&s).map_err(Into::into))
            {
                Ok(manifest) => self.register(manifest, false),
                Err(e) => log::warn!("Skipping invalid model manifest {:?}: {}", path, e),
            }
        }
    }

    pub fn list(&self, task_type: Option<TaskType>) -> Vec<ModelInfo> {
        let entries = self.entries.lock().unwrap();
        let mut models: Vec<ModelInfo> = entries
            .values()
            .filter(|m| task_type.is_none_or(|t| m.manifest.task_type == t))
            .map(ModelInfo::from)
            .collect();
        models.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        models
    }

    pub fn get(&self, model_id: &str) -> Option<RegisteredModel> {
        self.entries.lock().unwrap().get(model_id).cloned()
    }

    /// Returns the model to use for a task: the preferred id if it is
    /// registered and usable (available on disk, or fetchable via its
    /// download spec), otherwise the first available model, otherwise the
    /// first downloadable one. `filter` narrows within the task type, e.g.
    /// by `params.mask_subtype`.
    pub fn select_for_task(
        &self,
        task_type: TaskType,
        preferred_id: Option<&str>,
        filter: impl Fn(&ModelManifest) -> bool,
    ) -> Option<RegisteredModel> {
        let entries = self.entries.lock().unwrap();
        let usable = |m: &RegisteredModel| m.available || m.manifest.downloadable();
        if let Some(id) = preferred_id
            && let Some(m) = entries.get(id)
            && m.manifest.task_type == task_type
            && filter(&m.manifest)
            && usable(m)
        {
            return Some(m.clone());
        }
        let mut candidates: Vec<&RegisteredModel> = entries
            .values()
            .filter(|m| m.manifest.task_type == task_type && filter(&m.manifest) && usable(m))
            .collect();
        candidates.sort_by(|a, b| {
            b.available
                .cmp(&a.available)
                .then_with(|| a.manifest.display_name.cmp(&b.manifest.display_name))
        });
        candidates.first().map(|m| (*m).clone())
    }

    /// Downloads any missing weight files for a model that carries download
    /// specs, then re-checks availability. Errors if the model still has no
    /// weights afterwards.
    pub async fn ensure_available(
        &self,
        app_handle: &tauri::AppHandle,
        model_id: &str,
    ) -> Result<RegisteredModel> {
        let model = self
            .get(model_id)
            .ok_or_else(|| anyhow!("Model '{}' is not registered", model_id))?;
        if model.available {
            return Ok(model);
        }

        let _guard = self.download_lock.lock().await;
        let model = self.get(model_id).unwrap_or(model);
        if model.available {
            return Ok(model);
        }

        let mut files: Vec<(&Path, Option<&DownloadSpec>, String)> = vec![(
            model.weight_path.as_path(),
            model.manifest.download.as_ref(),
            model.manifest.display_name.clone(),
        )];
        for (name, path) in &model.aux_paths {
            files.push((
                path.as_path(),
                model.manifest.aux_downloads.get(name),
                format!("{} ({})", model.manifest.display_name, name),
            ));
        }

        for (path, spec, display_name) in files {
            if path.is_file() {
                continue;
            }
            let Some(spec) = spec else {
                return Err(anyhow!(
                    "Model '{}' is missing its weight file at {:?} and has no download source. \
                     Place the file there manually or fix the manifest.",
                    model_id,
                    path
                ));
            };
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let models_dir = path.parent().unwrap_or(&self.models_dir);
            let filename = path
                .file_name()
                .and_then(|f| f.to_str())
                .ok_or_else(|| anyhow!("Invalid weight file path {:?}", path))?;
            crate::ai_processing::download_and_verify_model(
                app_handle,
                models_dir,
                filename,
                &spec.url,
                &spec.sha256,
                &display_name,
            )
            .await?;
        }

        // Refresh availability for this entry.
        self.register(model.manifest.clone(), model.builtin);
        let refreshed = self.get(model_id).unwrap();
        if !refreshed.available {
            return Err(anyhow!(
                "Model '{}' is still unavailable after download (expected weights at {:?})",
                model_id,
                refreshed.weight_path
            ));
        }
        Ok(refreshed)
    }

    /// Lazily loads (and caches) the ONNX session for a model's primary
    /// weight file. `aux` selects an entry from `aux_files` instead (e.g.
    /// the SAM decoder).
    pub fn get_session(&self, model_id: &str, aux: Option<&str>) -> Result<Arc<Mutex<Session>>> {
        let model = self
            .get(model_id)
            .ok_or_else(|| anyhow!("Model '{}' is not registered", model_id))?;
        if model.manifest.params.get("engine").and_then(|v| v.as_str()) == Some("comfy") {
            return Err(anyhow!(
                "Model '{}' runs on the generative engine, not the built-in ONNX runtime",
                model_id
            ));
        }
        if !model.available {
            return Err(anyhow!(
                "Model '{}' is registered but its weight file is missing (expected at {:?})",
                model_id,
                model.weight_path
            ));
        }
        let (cache_key, path) = match aux {
            Some(name) => {
                let path = model.aux_paths.get(name).ok_or_else(|| {
                    anyhow!("Model '{}' has no aux file named '{}'", model_id, name)
                })?;
                (format!("{}::{}", model_id, name), path.clone())
            }
            None => (model_id.to_string(), model.weight_path.clone()),
        };

        if let Some(session) = self.sessions.lock().unwrap().get(&cache_key) {
            return Ok(session.clone());
        }

        let _ = ort::init().with_name("AI").commit();
        let session = Session::builder()?
            .commit_from_file(&path)
            .with_context(|| format!("Failed to load ONNX model from {:?}", path))?;
        // The ONNX Runtime environment can abort during process teardown on
        // macOS; the app-wide SIGABRT handler turns that into a clean exit.
        crate::register_exit_handler();
        let session = Arc::new(Mutex::new(session));
        self.sessions
            .lock()
            .unwrap()
            .insert(cache_key, session.clone());
        Ok(session)
    }

    pub fn unload(&self, model_id: &str) {
        let prefix = format!("{}::", model_id);
        self.sessions
            .lock()
            .unwrap()
            .retain(|k, _| k != model_id && !k.starts_with(&prefix));
    }

    pub fn unload_all(&self) {
        self.sessions.lock().unwrap().clear();
    }

    /// Generic single-input inference: feeds one f32 tensor through the
    /// model and returns all outputs. Models with bespoke input signatures
    /// (e.g. the SAM decoder) should fetch their session via `get_session`
    /// and run it directly.
    pub fn run_inference(
        &self,
        model_id: &str,
        input: Array<f32, IxDyn>,
    ) -> Result<Vec<Array<f32, IxDyn>>> {
        let session = self.get_session(model_id, None)?;
        let tensor = Tensor::from_array(input.as_standard_layout().into_owned())?;
        let mut session = session.lock().unwrap();
        let n_outputs = session.outputs.len();
        let outputs = session.run(ort::inputs![tensor])?;
        let mut results = Vec::with_capacity(n_outputs);
        for i in 0..n_outputs {
            results.push(outputs[i].try_extract_array::<f32>()?.to_owned());
        }
        Ok(results)
    }
}

/// Manifest filter matching a mask model's subtype ("subject",
/// "foreground", "sky", "depth").
pub fn mask_subtype_filter(subtype: &'static str) -> impl Fn(&ModelManifest) -> bool {
    move |m: &ModelManifest| m.params.get("mask_subtype").and_then(|v| v.as_str()) == Some(subtype)
}

/// Resolves which model to use for a task (honouring the user's
/// `preferredModels` setting under `preference_key`), downloads missing
/// weights when possible, and returns the ready model plus the registry.
pub async fn resolve_and_prepare(
    app_handle: &tauri::AppHandle,
    registry_mutex: &Mutex<Option<Arc<ModelRegistry>>>,
    task_type: TaskType,
    preference_key: &str,
    filter: impl Fn(&ModelManifest) -> bool,
) -> Result<(Arc<ModelRegistry>, RegisteredModel)> {
    let registry = get_or_init_registry(app_handle, registry_mutex)?;
    let preferred = crate::app_settings::load_settings(app_handle.clone())
        .ok()
        .and_then(|s| s.preferred_models)
        .and_then(|m| m.get(preference_key).cloned());
    let model = registry
        .select_for_task(task_type, preferred.as_deref(), filter)
        .ok_or_else(|| {
            anyhow!(
                "No usable {:?} model is registered for '{}'",
                task_type,
                preference_key
            )
        })?;
    let model = registry
        .ensure_available(app_handle, &model.manifest.id)
        .await?;
    Ok((registry, model))
}

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelProbe {
    pub scale_factor: u32,
    pub fixed_size: Option<(u32, u32)>,
}

/// Validates that an ONNX file is a compatible image-to-image model by
/// loading it and running a small test tensor through it. Returns the
/// detected scale factor and, for exports with baked-in spatial dims, the
/// fixed input size — everything a manifest needs.
pub fn probe_onnx_model(path: &Path) -> Result<ModelProbe> {
    let _ = ort::init().with_name("AI").commit();
    let mut session = Session::builder()?
        .commit_from_file(path)
        .with_context(|| format!("Could not load {:?} as an ONNX model", path))?;
    crate::register_exit_handler();

    if session.inputs.len() != 1 {
        return Err(anyhow!(
            "Model has {} inputs; only single-input image models are supported",
            session.inputs.len()
        ));
    }
    let dims: Vec<i64> = match &session.inputs[0].input_type {
        ort::value::ValueType::Tensor { ty, shape, .. } => {
            if *ty != ort::tensor::TensorElementType::Float32 {
                return Err(anyhow!(
                    "Model input is {:?}; only float32 image models are supported",
                    ty
                ));
            }
            shape.iter().copied().collect()
        }
        other => return Err(anyhow!("Model input is not a tensor: {:?}", other)),
    };
    if dims.len() != 4 {
        return Err(anyhow!(
            "Model input has {} dimensions; expected 4 (batch, channels, height, width)",
            dims.len()
        ));
    }
    if dims[1] > 0 && dims[1] != 3 {
        return Err(anyhow!(
            "Model expects {} channels; only 3-channel RGB is supported",
            dims[1]
        ));
    }

    let fixed_h = if dims[2] > 0 {
        Some(dims[2] as u32)
    } else {
        None
    };
    let fixed_w = if dims[3] > 0 {
        Some(dims[3] as u32)
    } else {
        None
    };
    let declared_fixed = fixed_h.is_some() || fixed_w.is_some();

    let run_at = |session: &mut Session, h: u32, w: u32| -> Result<Vec<usize>> {
        let input = Array::from_shape_vec(
            IxDyn(&[1, 3, h as usize, w as usize]),
            vec![0.5f32; 3 * h as usize * w as usize],
        )?;
        let tensor = Tensor::from_array(input.as_standard_layout().into_owned())?;
        let outputs = session
            .run(ort::inputs![tensor])
            .map_err(|e| anyhow!("Model failed a test run: {}", e))?;
        Ok(outputs[0].try_extract_array::<f32>()?.shape().to_vec())
    };

    let (mut test_h, mut test_w) = (fixed_h.unwrap_or(64), fixed_w.unwrap_or(64));
    let mut runtime_fixed = false;
    let out_shape = match run_at(&mut session, test_h, test_w) {
        Ok(shape) if !declared_fixed => {
            // Declared dynamic and ran once — but some exports only accept
            // shapes resembling the one they were traced with (transformer
            // window reshapes). Confirm with a second, differently-shaped
            // run; if it fails, find a fixed tile size that works.
            if run_at(&mut session, 96, 64).is_ok() {
                shape
            } else {
                runtime_fixed = true;
                let mut pinned = None;
                for size in [512u32, 256, 128, 64] {
                    if let Ok(s) = run_at(&mut session, size, size) {
                        (test_h, test_w) = (size, size);
                        pinned = Some(s);
                        break;
                    }
                }
                pinned.ok_or_else(|| {
                    anyhow!("Model only accepts one input shape and no usable tile size was found")
                })?
            }
        }
        Ok(shape) => shape,
        // Some exports declare dynamic dims but constrain them at runtime
        // (e.g. NAFNet needs ~384+ per side, SCUNet multiples of 64). Retry
        // at 512x512 and, if that works, pin the model to fixed tiles.
        Err(first_err) if !declared_fixed => {
            (test_h, test_w) = (512, 512);
            runtime_fixed = true;
            run_at(&mut session, test_h, test_w).map_err(|_| first_err)?
        }
        Err(e) => return Err(e),
    };
    if out_shape.len() != 4 || out_shape[1] != 3 {
        return Err(anyhow!(
            "Model output shape {:?} is not a 3-channel image; unsupported",
            out_shape
        ));
    }
    let (oh, ow) = (out_shape[2] as u32, out_shape[3] as u32);
    if oh % test_h != 0 || ow % test_w != 0 || oh / test_h != ow / test_w || oh < test_h {
        return Err(anyhow!(
            "Model output {}x{} is not an integer scale of the {}x{} input; unsupported",
            ow,
            oh,
            test_w,
            test_h
        ));
    }

    Ok(ModelProbe {
        scale_factor: oh / test_h,
        fixed_size: if runtime_fixed {
            Some((test_h, test_w))
        } else {
            match (fixed_h, fixed_w) {
                (Some(h), Some(w)) => Some((h, w)),
                // A single baked-in dim is exotic; treat as fixed square on
                // the known dim to stay safe.
                (Some(h), None) => Some((h, h)),
                (None, Some(w)) => Some((w, w)),
                (None, None) => None,
            }
        },
    })
}

pub fn get_or_init_registry(
    app_handle: &tauri::AppHandle,
    registry_mutex: &Mutex<Option<Arc<ModelRegistry>>>,
) -> Result<Arc<ModelRegistry>> {
    let mut guard = registry_mutex.lock().unwrap();
    if let Some(registry) = guard.as_ref() {
        return Ok(registry.clone());
    }
    let models_dir = crate::ai_processing::get_models_dir(app_handle)?;
    let registry = Arc::new(ModelRegistry::new(models_dir));
    *guard = Some(registry.clone());
    Ok(registry)
}

#[tauri::command]
pub async fn list_registered_models(
    task_type: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<ModelInfo>, String> {
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;
    let task = match task_type.as_deref() {
        Some(s) => Some(TaskType::parse(s).ok_or_else(|| format!("Unknown task type '{}'", s))?),
        None => None,
    };
    Ok(registry.list(task))
}

#[tauri::command]
pub async fn rescan_model_registry(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<ModelInfo>, String> {
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;
    registry.rescan();
    Ok(registry.list(None))
}

/// Proves the registry round trip: loads the model, runs a small tensor
/// through it, and reports the output shape.
#[tauri::command]
pub async fn test_model_round_trip(
    model_id: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let registry =
        get_or_init_registry(&app_handle, &state.model_registry).map_err(|e| e.to_string())?;
    let input = Array::from_shape_vec(IxDyn(&[1, 3, 8, 8]), (0..192).map(|v| v as f32).collect())
        .map_err(|e| e.to_string())?;
    let outputs = registry
        .run_inference(&model_id, input)
        .map_err(|e| e.to_string())?;
    let shapes: Vec<Vec<usize>> = outputs.iter().map(|o| o.shape().to_vec()).collect();
    Ok(format!(
        "Model '{}' ran successfully: {} output(s) with shapes {:?}",
        model_id,
        outputs.len(),
        shapes
    ))
}
