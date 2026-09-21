#!/usr/bin/env python3
"""Capture Resolve's output transform as a 3D LUT, by measurement.

The single biggest reason a graded image "looks like Resolve" is its rendering
transform — the step that takes wide-gamut scene values to something a display
can show. Everything else (how a wheel responds, how contrast pivots) is a
smaller term. That transform is proprietary, and reimplementing it by fitting
constants is the slow, uncertain way to get close.

It does not have to be reimplemented. It can be *sampled*. Feed Resolve an
image whose pixels are a regular lattice covering the whole working range, let
it apply its transform, and read the result back: that is the transform, at
lattice resolution, exactly as this machine's Resolve computes it.

    make-lattice  writes the image to put through Resolve
    read-lattice  turns Resolve's exported frame into a .cube

The lattice is laid out in DaVinci Intermediate, not linear. Intermediate is a
log encoding, so an evenly spaced lattice in it is evenly spaced *perceptually*
and covers roughly -0.01 to 100 in linear light. A linear lattice would spend
most of its entries on highlights nobody looks at and leave the shadows coarse.

A captured cube is derived from Blackmagic's transform. Keep it local — it is
for matching your own machine, not for redistribution.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

import cv2
import numpy as np

# DaVinci Intermediate, from Blackmagic's published specification.
A, B, C, D, E = 0.0075, 7.0, 0.07329248, 10.44426855, 0.02740668
LIN_CUT = 0.00262409


def intermediate_to_linear(v: np.ndarray) -> np.ndarray:
    return np.where(v <= E, v / D, np.exp2(v / C - B) - A)


def linear_to_intermediate(v: np.ndarray) -> np.ndarray:
    return np.where(v <= LIN_CUT, v * D, (np.log2(np.maximum(v + A, 1e-10)) + B) * C)


def layout(size: int) -> tuple[int, int]:
    """Image shape for a size^3 lattice, square when that is possible."""
    total = size**3
    side = int(round(math.sqrt(total)))
    if side * side == total:
        return side, side
    return size * size, size


def lattice(size: int) -> np.ndarray:
    """Lattice values in Intermediate, ordered red fastest, blue slowest."""
    axis = np.linspace(0.0, 1.0, size, dtype=np.float64)
    r, g, b = np.meshgrid(axis, axis, axis, indexing="ij")
    # Index = r + g*size + b*size^2, which is also .cube's own ordering.
    return np.stack([r, g, b], axis=-1).transpose(2, 1, 0, 3).reshape(-1, 3)


def make_lattice(args: argparse.Namespace) -> None:
    values = lattice(args.size)
    width, height = layout(args.size)
    image = (np.clip(values, 0.0, 1.0) * 65535.0 + 0.5).astype(np.uint16)
    # OpenCV writes BGR; the lattice itself stays in RGB order throughout.
    written = cv2.imwrite(args.output, image.reshape(height, width, 3)[:, :, ::-1])
    if not written:
        raise SystemExit(f"could not write {args.output}")
    manifest = {
        "size": args.size,
        "width": width,
        "height": height,
        "encoding": "davinci_intermediate",
        "primaries": "davinci_wide_gamut",
        "order": "red fastest, blue slowest",
        "bit_depth": 16,
    }
    Path(args.output).with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"{args.size}^3 lattice -> {args.output} ({width}x{height}, 16-bit TIFF)")
    print(RECIPE.format(width=width, height=height, path=args.output))


def read_lattice(args: argparse.Namespace) -> None:
    manifest = json.loads(Path(args.manifest).read_text())
    size, width, height = manifest["size"], manifest["width"], manifest["height"]
    raw = cv2.imread(args.rendered, cv2.IMREAD_UNCHANGED)
    if raw is None:
        raise SystemExit(f"could not read {args.rendered}")
    if raw.shape[1] != width or raw.shape[0] != height:
        raise SystemExit(
            f"{args.rendered} is {raw.shape[1]}x{raw.shape[0]}, but the lattice is "
            f"{width}x{height}. Resolve resized the frame; set the timeline resolution to "
            "match the lattice exactly and disable any scaling."
        )
    full = {np.dtype(np.uint8): 255.0, np.dtype(np.uint16): 65535.0}.get(raw.dtype, 1.0)
    data = raw[:, :, :3][:, :, ::-1].astype(np.float64) / full
    if data.max() <= 1.0 / 255.0:
        raise SystemExit("The rendered frame is black; check the clip was actually graded.")
    entries = data.reshape(-1, 3)

    # An untouched round trip means the transform never ran: the usual cause is
    # the clip being tagged as the timeline space with output set to match.
    identity = lattice(size)
    if float(np.abs(entries - identity).max()) < 2.0 / 255.0:
        raise SystemExit(
            "The rendered frame matches the lattice to within a quantization step, so no "
            "transform was applied. Check the project's output colour space is a display "
            "space, not the timeline space."
        )

    lines = [
        "# Captured from DaVinci Resolve's output transform by tools/resolve_drt.py.",
        f"# Input is {manifest['primaries']} in {manifest['encoding']}.",
        f"# {args.note}" if args.note else "# No project note recorded.",
        f"LUT_3D_SIZE {size}",
        "DOMAIN_MIN 0.0 0.0 0.0",
        "DOMAIN_MAX 1.0 1.0 1.0",
    ]
    lines += [f"{r:.6f} {g:.6f} {b:.6f}" for r, g, b in np.clip(entries, 0.0, 1.0)]
    Path(args.output).write_text("\n".join(lines) + "\n")
    print(f"{size}^3 cube -> {args.output}")
    mid = entries[len(entries) // 2]
    print(f"sanity: lattice centre renders to {mid[0]:.4f} {mid[1]:.4f} {mid[2]:.4f}")


RECIPE = """
In Resolve, once:

  1. Project Settings > Color Management
       Color science:        DaVinci YRGB Color Managed
       Color processing:     DaVinci Wide Gamut (not ACES)
       Timeline colour space: DaVinci WG/Intermediate
       Output colour space:   Rec.709 Gamma 2.4   (or sRGB, if that is the
                              transform you actually grade to)
       Untick "Use white point adaptation" and any auto tone/gamut mapping.
  2. Project Settings > Master Settings
       Timeline resolution:  {width} x {height}  (Custom)
       Set "Mismatched resolution" to "Center crop with no resizing".
  3. Import {path}, put it on the timeline, and in the Media Pool set its
     Input Colour Space to DaVinci WG/Intermediate so nothing is applied
     going in.
  4. Add no grade at all. The clip must reach the output untouched.
  5. Deliver: TIFF, 16-bit, Data levels Full, single frame, no resizing,
     and NO output LUT.

Then: tools/resolve_drt.py read-lattice --manifest ... --rendered ... --output resolve.cube
"""


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    make = sub.add_parser("make-lattice", help="write the image to put through Resolve")
    make.add_argument("--size", type=int, default=64, help="lattice per axis (default 64)")
    make.add_argument("--output", required=True, help="16-bit TIFF to write")
    make.set_defaults(func=make_lattice)

    read = sub.add_parser("read-lattice", help="turn Resolve's export into a .cube")
    read.add_argument("--manifest", required=True, help="the .json written beside the lattice")
    read.add_argument("--rendered", required=True, help="the frame Resolve delivered")
    read.add_argument("--output", required=True, help=".cube to write")
    read.add_argument("--note", default="", help="project settings this was captured under")
    read.set_defaults(func=read_lattice)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
