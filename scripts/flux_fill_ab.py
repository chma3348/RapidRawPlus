#!/usr/bin/env python3
"""
Run controlled Flux Fill workflow variants against a saved RapidRAW fill debug crop.

This intentionally bypasses the editor pipeline. It reuses one pair of files:
  blob-00-engine-input.png
  blob-00-engine-mask.png

The outputs are raw Comfy results, so they answer the only question that matters
for Reconstruct right now: which Flux graph actually changes the masked area.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import time
import urllib.parse
import urllib.request
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any


PORT = 8399
BASE_URL = f"http://127.0.0.1:{PORT}"
APP_SUPPORT = Path.home() / "Library/Application Support/io.github.CyberTimon.RapidRAW"
DEBUG_ROOT = APP_SUPPORT / "ai-fill-debug"
DOCUMENT_ENGINE = Path.home() / "Documents/RapidRAW Models/Engine"
APP_ENGINE = APP_SUPPORT / "comfy"


@dataclass(frozen=True)
class Variant:
    name: str
    guidance: float | None
    differential: bool
    noise_mask: bool
    steps: int = 28
    cfg: float = 1.0
    sampler: str = "euler"
    scheduler: str = "normal"


VARIANTS = [
    Variant("current_g30_diff_noise", guidance=30.0, differential=True, noise_mask=True),
    Variant("g15_diff_noise", guidance=15.0, differential=True, noise_mask=True),
    Variant("g7_5_diff_noise", guidance=7.5, differential=True, noise_mask=True),
    Variant("g30_no_diff_noise", guidance=30.0, differential=False, noise_mask=True),
    Variant("g15_no_diff_noise", guidance=15.0, differential=False, noise_mask=True),
    Variant("g30_diff_no_noise_mask", guidance=30.0, differential=True, noise_mask=False),
    Variant("g15_diff_no_noise_mask", guidance=15.0, differential=True, noise_mask=False),
    Variant("no_flux_guidance_diff_noise", guidance=None, differential=True, noise_mask=True),
]


def request_json(method: str, path: str, body: bytes | None = None, headers: dict[str, str] | None = None) -> Any:
    req = urllib.request.Request(
        f"{BASE_URL}{path}",
        data=body,
        headers=headers or {},
        method=method,
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = resp.read()
    return json.loads(data.decode("utf-8")) if data else None


def request_bytes(path: str) -> bytes:
    with urllib.request.urlopen(f"{BASE_URL}{path}", timeout=120) as resp:
        return resp.read()


def is_engine_running() -> bool:
    try:
        request_json("GET", "/system_stats")
        return True
    except Exception:
        return False


def engine_dir() -> Path:
    if (APP_ENGINE / "ComfyUI/main.py").is_file() and (APP_ENGINE / "venv/bin/python").is_file():
        return APP_ENGINE
    if (DOCUMENT_ENGINE / "ComfyUI/main.py").is_file() and (DOCUMENT_ENGINE / "venv/bin/python").is_file():
        return DOCUMENT_ENGINE
    raise SystemExit("Could not find a RapidRAW Comfy engine install.")


def engine_needs_cpu_flag(root: Path) -> bool:
    probe = """import torch
has_mps = hasattr(torch.backends, "mps") and torch.backends.mps.is_available()
has_cuda = torch.cuda.is_available()
print("cpu" if not (has_mps or has_cuda) else "accelerated")
"""
    try:
        out = subprocess.run(
            [str(root / "venv/bin/python"), "-c", probe],
            check=True,
            capture_output=True,
            text=True,
            timeout=20,
        )
    except Exception:
        return False
    return out.stdout.strip() == "cpu"


def start_engine(out_dir: Path, force_cpu: bool) -> subprocess.Popen[bytes] | None:
    if is_engine_running():
        return None

    root = engine_dir()
    log_path = out_dir / "comfy-engine.log"
    log = open(log_path, "ab", buffering=0)
    cmd = [
        str(root / "venv/bin/python"),
        "main.py",
        "--listen",
        "127.0.0.1",
        "--port",
        str(PORT),
    ]
    cpu_mode = force_cpu or engine_needs_cpu_flag(root)
    if cpu_mode:
        cmd.append("--cpu")

    env = os.environ.copy()
    env.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")
    env.setdefault("CUDA_VISIBLE_DEVICES", "-1")
    env.setdefault("HIP_VISIBLE_DEVICES", "-1")

    proc = subprocess.Popen(
        cmd,
        cwd=root / "ComfyUI",
        env=env,
        stdout=log,
        stderr=log,
    )

    deadline = time.time() + 120
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SystemExit(f"Comfy exited early. See {log_path}")
        if is_engine_running():
            return proc
        time.sleep(2)
    raise SystemExit(f"Comfy did not become ready in time. See {log_path}")


def upload_image(name: str, path: Path) -> None:
    boundary = f"----rapidraw-ab-{uuid.uuid4().hex}"
    payload = bytearray()
    payload.extend(f"--{boundary}\r\n".encode())
    payload.extend(f'Content-Disposition: form-data; name="image"; filename="{name}"\r\n'.encode())
    payload.extend(b"Content-Type: image/png\r\n\r\n")
    payload.extend(path.read_bytes())
    payload.extend(b"\r\n")
    payload.extend(f"--{boundary}\r\n".encode())
    payload.extend(b'Content-Disposition: form-data; name="overwrite"\r\n\r\ntrue\r\n')
    payload.extend(f"--{boundary}--\r\n".encode())

    req = urllib.request.Request(
        f"{BASE_URL}/upload/image",
        data=bytes(payload),
        headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=120) as resp:
        if resp.status >= 300:
            raise RuntimeError(f"upload failed: {resp.status}")


def workflow(variant: Variant, image_name: str, mask_name: str, prompt: str, seed: int) -> dict[str, Any]:
    model_ref: list[Any] = ["3", 0]
    nodes: dict[str, Any] = {
        "1": {"class_type": "LoadImage", "inputs": {"image": image_name}},
        "2": {"class_type": "LoadImageMask", "inputs": {"image": mask_name, "channel": "red"}},
        "3": {"class_type": "UnetLoaderGGUF", "inputs": {"unet_name": "flux1-fill-dev-Q8_0.gguf"}},
        "3c": {
            "class_type": "DualCLIPLoaderGGUF",
            "inputs": {
                "clip_name1": "t5-v1_1-xxl-encoder-Q8_0.gguf",
                "clip_name2": "clip_l.safetensors",
                "type": "flux",
            },
        },
        "3v": {"class_type": "VAELoader", "inputs": {"vae_name": "ae.safetensors"}},
        "4": {"class_type": "CLIPTextEncode", "inputs": {"text": prompt, "clip": ["3c", 0]}},
        "5": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["3c", 0]}},
    }

    if variant.differential:
        nodes["3d"] = {"class_type": "DifferentialDiffusion", "inputs": {"model": ["3", 0]}}
        model_ref = ["3d", 0]

    positive_ref: list[Any] = ["4", 0]
    if variant.guidance is not None:
        nodes["4g"] = {
            "class_type": "FluxGuidance",
            "inputs": {"conditioning": ["4", 0], "guidance": variant.guidance},
        }
        positive_ref = ["4g", 0]

    nodes["6"] = {
        "class_type": "InpaintModelConditioning",
        "inputs": {
            "positive": positive_ref,
            "negative": ["5", 0],
            "vae": ["3v", 0],
            "pixels": ["1", 0],
            "mask": ["2", 0],
            "noise_mask": variant.noise_mask,
        },
    }
    nodes["7"] = {
        "class_type": "KSampler",
        "inputs": {
            "model": model_ref,
            "positive": ["6", 0],
            "negative": ["6", 1],
            "latent_image": ["6", 2],
            "seed": seed,
            "steps": variant.steps,
            "cfg": variant.cfg,
            "sampler_name": variant.sampler,
            "scheduler": variant.scheduler,
            "denoise": 1.0,
        },
    }
    nodes["8"] = {"class_type": "VAEDecode", "inputs": {"samples": ["7", 0], "vae": ["3v", 0]}}
    nodes["9"] = {"class_type": "SaveImage", "inputs": {"images": ["8", 0], "filename_prefix": "rapidraw_ab"}}
    return nodes


def run_workflow(prompt_graph: dict[str, Any], label: str) -> bytes:
    resp = request_json("POST", "/prompt", json.dumps({"prompt": prompt_graph}).encode("utf-8"), {"Content-Type": "application/json"})
    errors = resp.get("node_errors") if isinstance(resp, dict) else None
    if errors:
        raise RuntimeError(f"{label}: workflow rejected: {json.dumps(errors, indent=2)}")
    prompt_id = resp.get("prompt_id")
    if not prompt_id:
        raise RuntimeError(f"{label}: no prompt id in response: {resp}")

    started = time.time()
    while True:
        time.sleep(2)
        history = request_json("GET", f"/history/{prompt_id}")
        entry = history.get(prompt_id, {})
        status = entry.get("status", {})
        if status.get("status_str") == "error":
            raise RuntimeError(f"{label}: engine error: {json.dumps(status, indent=2)}")
        if not status.get("completed"):
            elapsed = int(time.time() - started)
            print(f"{label}: running ({elapsed}s)")
            continue
        outputs = entry.get("outputs", {})
        for node in outputs.values():
            images = node.get("images") or []
            if images:
                img = images[0]
                query = urllib.parse.urlencode(
                    {
                        "filename": img.get("filename", ""),
                        "subfolder": img.get("subfolder", ""),
                        "type": img.get("type", "output"),
                    }
                )
                return request_bytes(f"/view?{query}")
        raise RuntimeError(f"{label}: completed without an output image")


def newest_debug_dir() -> Path:
    candidates = [
        p
        for p in DEBUG_ROOT.iterdir()
        if p.is_dir() and (p / "blob-00-engine-input.png").is_file() and (p / "blob-00-engine-mask.png").is_file()
    ]
    if not candidates:
        raise SystemExit(f"No usable blob-00 engine input/mask found under {DEBUG_ROOT}")
    return max(candidates, key=lambda p: p.stat().st_mtime)


def default_output_root() -> Path:
    return APP_SUPPORT / "flux-fill-ab"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--debug-dir", type=Path, default=None, help="RapidRAW ai-fill-debug run directory to use.")
    parser.add_argument("--prompt", default="blue sky", help="Prompt to test.")
    parser.add_argument("--seed", type=int, default=123456789, help="Fixed seed for all variants.")
    parser.add_argument("--out", type=Path, default=None, help="Output directory. Defaults under app support.")
    parser.add_argument("--variant", action="append", default=None, help="Run only named variant(s).")
    parser.add_argument("--limit", type=int, default=0, help="Run only the first N variants after filtering.")
    parser.add_argument(
        "--cpu",
        action="store_true",
        help="Force CPU when this script starts Comfy. Useful if standalone Comfy incorrectly selects CUDA on macOS.",
    )
    parser.add_argument(
        "--start-only",
        action="store_true",
        help="Start/connect to Comfy, print the selected input/output paths, then exit without running variants.",
    )
    args = parser.parse_args()

    debug_dir = args.debug_dir or newest_debug_dir()
    image_path = debug_dir / "blob-00-engine-input.png"
    mask_path = debug_dir / "blob-00-engine-mask.png"
    if not image_path.is_file() or not mask_path.is_file():
        raise SystemExit(f"Missing blob-00-engine-input.png or blob-00-engine-mask.png in {debug_dir}")

    run_id = time.strftime("%Y%m%d-%H%M%S")
    out_dir = args.out or (default_output_root() / run_id)
    out_dir.mkdir(parents=True, exist_ok=True)

    proc = start_engine(out_dir, args.cpu)
    image_name = f"rapidraw_ab_{run_id}_image.png"
    mask_name = f"rapidraw_ab_{run_id}_mask.png"
    upload_image(image_name, image_path)
    upload_image(mask_name, mask_path)

    selected = VARIANTS
    if args.variant:
        wanted = set(args.variant)
        selected = [v for v in selected if v.name in wanted]
        missing = wanted - {v.name for v in selected}
        if missing:
            raise SystemExit(f"Unknown variant(s): {', '.join(sorted(missing))}")
    if args.limit > 0:
        selected = selected[: args.limit]

    metadata = {
        "debug_dir": str(debug_dir),
        "image": str(image_path),
        "mask": str(mask_path),
        "prompt": args.prompt,
        "seed": args.seed,
        "variants": [v.__dict__ for v in selected],
    }
    (out_dir / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")

    print(f"Input: {image_path}")
    print(f"Mask:  {mask_path}")
    print(f"Out:   {out_dir}")
    if args.start_only:
        print("Engine is reachable; exiting because --start-only was set.")
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
        return 0

    for variant in selected:
        graph = workflow(variant, image_name, mask_name, args.prompt, args.seed)
        (out_dir / f"{variant.name}.workflow.json").write_text(json.dumps(graph, indent=2), encoding="utf-8")
        print(f"Running {variant.name}")
        try:
            png = run_workflow(graph, variant.name)
            (out_dir / f"{variant.name}.png").write_bytes(png)
            print(f"Saved {variant.name}.png")
        except Exception as exc:
            (out_dir / f"{variant.name}.error.txt").write_text(str(exc), encoding="utf-8")
            print(f"FAILED {variant.name}: {exc}")

    if proc is not None:
        print("Leaving Comfy running for inspection/reuse.")
    print(f"Done: {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
