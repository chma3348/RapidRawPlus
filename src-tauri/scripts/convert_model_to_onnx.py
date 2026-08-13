#!/usr/bin/env python3
"""Converts a community PyTorch image model (.pth / .safetensors) to ONNX.

Uses spandrel (the architecture loader behind chaiNNer) so the ~30 common
super-resolution / restoration architectures load without per-model code.
Prints a single JSON line on success or failure — consumed by RapidRAW's
conversion assistant.

Requires: torch, spandrel, onnx (and optionally spandrel_extra_arches).
"""

import argparse
import json
import sys


def fail(message: str) -> None:
    print(json.dumps({"ok": False, "error": message}))
    sys.exit(1)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    try:
        import torch
        from spandrel import ImageModelDescriptor, ModelLoader
    except ImportError as e:
        fail(f"Conversion environment is missing packages: {e}")

    try:
        import spandrel_extra_arches

        spandrel_extra_arches.install()
    except ImportError:
        pass

    try:
        descriptor = ModelLoader().load_from_file(args.input)
    except Exception as e:  # noqa: BLE001 — surface any loader error verbatim
        fail(
            "Unrecognized model architecture. This converter handles self-contained "
            "image models (upscalers, restorers). Diffusion checkpoints (SeedVR2, Stable "
            "Diffusion, Flux…) are one part of a multi-model pipeline and need a diffusion "
            f"engine like ComfyUI — they cannot be converted. ({e})"
        )

    if not isinstance(descriptor, ImageModelDescriptor):
        fail("This checkpoint is not a single-image model and cannot run in RapidRAW.")
    if descriptor.input_channels != 3 or descriptor.output_channels != 3:
        fail(
            f"Model uses {descriptor.input_channels}->{descriptor.output_channels} channels; "
            "only 3-channel RGB models are supported."
        )

    model = descriptor.model.eval()

    class Wrapper(torch.nn.Module):
        def __init__(self, net):
            super().__init__()
            self.net = net

        def forward(self, x):
            return torch.clamp(self.net(x), 0.0, 1.0)

    wrapper = Wrapper(model)
    dynamic_axes = {
        "input": {0: "batch", 2: "height", 3: "width"},
        "output": {0: "batch", 2: "height", 3: "width"},
    }

    # Larger trace size first: shape-inflexible architectures (window
    # attention etc.) get pinned to the traced size at probe time, and 256px
    # tiles are far more practical than 64px ones.
    last_error = None
    for size in (256, 64):
        try:
            with torch.no_grad():
                torch.onnx.export(
                    wrapper,
                    torch.rand(1, 3, size, size),
                    args.out,
                    input_names=["input"],
                    output_names=["output"],
                    opset_version=17,
                    do_constant_folding=True,
                    dynamic_axes=dynamic_axes,
                    dynamo=False,
                )
            print(
                json.dumps(
                    {
                        "ok": True,
                        "scale": descriptor.scale,
                        "architecture": getattr(descriptor.architecture, "name", "unknown"),
                    }
                )
            )
            return
        except Exception as e:  # noqa: BLE001
            last_error = e

    fail(f"ONNX export failed: {last_error}")


if __name__ == "__main__":
    main()
