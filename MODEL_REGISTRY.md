# Model Registry (fork feature)

All local AI features (masking, inpainting, upscaling, deblurring) resolve their
ONNX model through a manifest-based registry instead of hardcoded paths, so
models are swappable per task from **Settings → Local AI Models** or the
model dropdown in each feature's dialog.

## Where things live

- **Weights:** `<app-data>/models/`
  (macOS: `~/Library/Application Support/io.github.CyberTimon.RapidRAW/models/`)
- **Custom manifests:** `<app-data>/models/manifests/*.json`
- **Built-in manifests:** compiled in — see `builtin_manifests()` in
  [`src-tauri/src/model_registry.rs`](src-tauri/src/model_registry.rs)
  (SAM 2, U-2-Net foreground/sky, Depth Anything V2, LaMa, Real-ESRGAN ×2
  variants, NAFNet deblur, SCUNet artifact removal, ESDNet-L demoiré, plus
  engine-backed manifests: SeedVR2 restore (3B and 7B), SDXL Generative Fill, SDXL
  Fill+ (Fooocus) and Flux Fill (Best)).

Engine-backed manifests (`params.engine == "comfy"`) point at weights inside
the managed ComfyUI engine (`<app-data>/comfy/ComfyUI/models/...`) and run
through it rather than ONNX. The three inpaint tiers, fast → best:
`sdxl-fill` (latent-noise-mask refine), `sdxl-fill-fooocus` (Fooocus inpaint
patch, `params.fill_workflow == "fooocus"`), and `flux-fill` (Flux Fill dev
Q8 GGUF, `params.fill_workflow == "flux"`, ~2 min per fill but Firefly-class
quality). Pick the tier in the AI panel's inpaint model dropdown or the
Expand dialog.

The ESDNet-L demoiré model has no public ONNX; it is converted locally from
the official PyTorch checkpoint with
[`src-tauri/scripts/export_esdnet_onnx.py`](src-tauri/scripts/export_esdnet_onnx.py)
(instructions in the script header), so its manifest has no `download` spec.

A model whose weight file is missing still appears in dropdowns, marked
"not downloaded"; if its manifest has a `download` spec the file is fetched
(and SHA-256 verified) automatically on first use.

## Manifest format

```json
{
  "id": "my-upscaler",
  "display_name": "My Upscaler 4x",
  "task_type": "upscale",
  "file_path": "my_upscaler.onnx",
  "download": { "url": "https://…", "sha256": "…" },
  "aux_files": { "decoder": "my_decoder.onnx" },
  "aux_downloads": { "decoder": { "url": "https://…", "sha256": "…" } },
  "params": { "scale_factor": 4, "tile_size": 512, "tile_overlap": 16 }
}
```

- `task_type`: `upscale` | `deblur` | `restore` | `mask` | `inpaint`
- `file_path`: relative to the models dir, or absolute.
- `download` / `aux_downloads`: optional; omit for models you place manually.
- `params` (task-specific, all optional):
  - `scale_factor` — output size multiplier (upscalers: 4, deblur: 1)
  - `tile_size` / `tile_overlap` — tiling for VRAM/RAM control (default 512/16)
  - `input_height` + `input_width` — set both to force fixed-size tiles with
    edge padding, for exports that reject arbitrary dimensions (e.g. the
    NAFNet deblur export needs ≥ ~384 per side, so it runs at 512×512)
  - `mask_subtype` — for `mask` models: `subject` | `foreground` | `sky` | `depth`

Enhancement models must take one NCHW `f32` RGB tensor in `[0,1]` and return
the same layout scaled by `scale_factor`.

The chosen model per task is stored in settings (`preferredModels`, keyed by
`upscale`, `deblur`, `restore`, `inpaint`, `mask_subject`, `mask_foreground`,
`mask_sky`, `mask_depth`). Heavy enhancement sessions are unloaded after each
run; mask/inpaint sessions stay resident.

## Quality controls

The enhance dialog exposes a **Strength** slider (blends the model output
with the original — lower is subtler) and, for upscaling, an **Output size**
choice (a 4x model can deliver 2x, which suppresses hallucinated detail).
Tiled runs cross-fade overlapping tiles; models flagged `single_pass` (the
demoiré model) process the whole image at once, since per-tile runs of
global corrections show up as visible boxes.

## Preview before committing

The enhance dialog's left thumbnail is clickable: it runs the selected model
on a small crop around that spot and shows a zoomed before/after with the
strength slider blending live — judge a model in seconds instead of after a
full run. (For the demoiré model the crop preview is indicative only, since
that correction is global.)

## Conversion assistant

PyTorch checkpoints (`.pth`/`.safetensors`/`.ckpt`) for single-image models
are converted to ONNX automatically — via "Choose file…", or straight from
search results (badged "will be converted"). The converter uses spandrel
(chaiNNer's architecture loader, ~30 community architectures) in a Python
environment the app sets up on first use (one-time ~2 GB download; managed
under `<app-data>/pyenv`). Converted models go through the same probe
validation; diffusion checkpoints are rejected with an explanation — they
are pipelines, not single models, and cannot run in-process.

## Model Library

**Settings → Processing tab → Model Library** lists every known model with
download/remove buttons, plus a **search box** that queries Hugging Face for
repositories containing `.onnx` files and installs them in one click. Every
install path (search, URL, local file) runs an **automatic probe**: the model
is loaded, a test tensor is run through it, and its scale factor and any
fixed input size are detected and written into the manifest — incompatible
files (wrong tensor layout, non-f32, text models) are rejected and cleaned
up instead of registered. The list is driven by a curated catalog
([`src-tauri/model_catalog.json`](src-tauri/model_catalog.json), entries are
manifests plus `description`/`size_bytes`). Set `modelCatalogUrl` in
settings to also pull an updated catalog from a URL on refresh — new models
can then be offered without rebuilding the app. "Add model from URL"
downloads any direct `.onnx` link, records its checksum, and generates the
manifest.

Tests: `src-tauri/tests/model_registry_roundtrip.rs` (a 200-byte identity
ONNX fixture proves the load→infer→reassemble round trip; regenerate it with
`python3 src-tauri/scripts/make_dummy_model.py`).
