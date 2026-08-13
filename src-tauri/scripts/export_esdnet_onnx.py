#!/usr/bin/env python3
"""Exports the ESDNet-L demoiréing model (UHDM, ECCV 2022) to ONNX.

No pre-converted ONNX of ESDNet exists publicly, so this converts the
official PyTorch checkpoint. The export is fixed at 1x3x512x512 because the
app's tiled-enhancement pipeline feeds fixed 512x512 tiles anyway (ESDNet
needs multiple-of-32 input dims).

Usage:
  python -m venv venv && venv/bin/pip install torch onnx gdown
  git clone --depth 1 https://github.com/CVMI-Lab/UHDM.git
  venv/bin/gdown -O uhdm_large_checkpoint.pth 1PyCLCytsu4F8gEk_04a8Qs7pcsHazAie
  venv/bin/python export_esdnet_onnx.py \
      --uhdm-repo UHDM \
      --checkpoint uhdm_large_checkpoint.pth \
      --out esdnet_l_uhdm_demoire.onnx
"""

import argparse
import sys


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--uhdm-repo", required=True, help="Path to a clone of CVMI-Lab/UHDM")
    parser.add_argument("--checkpoint", required=True, help="Path to uhdm_large_checkpoint.pth")
    parser.add_argument("--out", required=True, help="Output .onnx path")
    parser.add_argument("--size", type=int, default=512, help="Example spatial size (multiple of 32)")
    parser.add_argument(
        "--fixed",
        action="store_true",
        help="Export with a fixed input size instead of dynamic height/width",
    )
    args = parser.parse_args()

    sys.path.insert(0, args.uhdm_repo)
    import torch
    from model.nets import my_model

    # ESDNet-L hyperparameters (sam_number=2; plain ESDNet uses 1).
    model = my_model(
        en_feature_num=48,
        en_inter_num=32,
        de_feature_num=64,
        de_inter_num=32,
        sam_number=2,
    )
    state_dict = torch.load(args.checkpoint, map_location="cpu", weights_only=False)
    model.load_state_dict(state_dict)
    model.eval()

    class FullResOnly(torch.nn.Module):
        """ESDNet returns three scales; only the full-res output matters."""

        def __init__(self, net: torch.nn.Module):
            super().__init__()
            self.net = net

        def forward(self, x: torch.Tensor) -> torch.Tensor:
            out_1, _, _ = self.net(x)
            return torch.clamp(out_1, 0.0, 1.0)

    wrapper = FullResOnly(model)
    dummy = torch.rand(1, 3, args.size, args.size)
    dynamic_axes = (
        None
        if args.fixed
        else {"input": {2: "height", 3: "width"}, "output": {2: "height", 3: "width"}}
    )
    with torch.no_grad():
        torch.onnx.export(
            wrapper,
            dummy,
            args.out,
            input_names=["input"],
            output_names=["output"],
            opset_version=17,
            do_constant_folding=True,
            dynamic_axes=dynamic_axes,
            dynamo=False,
        )
    kind = f"fixed 1x3x{args.size}x{args.size}" if args.fixed else "dynamic H/W (multiples of 32)"
    print(f"Exported {args.out} ({kind})")


if __name__ == "__main__":
    main()
