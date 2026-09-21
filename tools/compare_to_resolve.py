#!/usr/bin/env python3
"""Diff a photograph rendered here against the same photograph from Resolve.

The captured transforms are verified against each other; this is the step
that checks them against the thing they were captured from. Render one of
your own stills through Resolve with no grade, render the same file through
v3 with no adjustments, and see whether they agree.

Agreement is expected at *neutral* only. The transforms are captured; the
grading controls are still ours, so anything but neutral is comparing two
different sets of tools and proves nothing about the colour chain.

    tools/compare_to_resolve.py --ours OURS.png --resolve FROM_RESOLVE.tif
"""

from __future__ import annotations

import argparse

import cv2
import numpy as np


def load(path: str) -> np.ndarray:
    raw = cv2.imread(path, cv2.IMREAD_UNCHANGED)
    if raw is None:
        raise SystemExit(f"could not read {path}")
    full = {np.dtype(np.uint8): 255.0, np.dtype(np.uint16): 65535.0}.get(raw.dtype, 1.0)
    return raw[:, :, :3][:, :, ::-1].astype(np.float64) / full


def srgb_to_linear(v: np.ndarray) -> np.ndarray:
    return np.where(v <= 0.04045, v / 12.92, ((v + 0.055) / 1.055) ** 2.4)


def oklab(rgb: np.ndarray) -> np.ndarray:
    m = np.array([[0.4122214708, 0.5363325363, 0.0514459929],
                  [0.2119034982, 0.6806995451, 0.1073969566],
                  [0.0883024619, 0.2817188376, 0.6299787005]])
    lms = np.cbrt(srgb_to_linear(rgb) @ m.T)
    n = np.array([[0.2104542553, 0.7936177850, -0.0040720468],
                  [1.9779984951, -2.4285922050, 0.4505937099],
                  [0.0259040371, 0.7827717662, -0.8086757660]])
    return lms @ n.T


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--ours", required=True)
    parser.add_argument("--resolve", required=True)
    parser.add_argument("--report", help="write a side-by-side and a difference map here")
    args = parser.parse_args()

    ours, theirs = load(args.ours), load(args.resolve)
    if ours.shape != theirs.shape:
        raise SystemExit(
            f"{ours.shape[1]}x{ours.shape[0]} against {theirs.shape[1]}x{theirs.shape[0]}: "
            "render both at the same size, with no resizing in Resolve's delivery."
        )
    delta = np.abs(ours - theirs)
    print(f"encoded difference   mean {delta.mean()*255:6.2f}/255   "
          f"p99 {np.percentile(delta,99)*255:6.2f}   max {delta.max()*255:6.2f}")
    a, b = oklab(ours), oklab(theirs)
    dl = np.abs(a[..., 0] - b[..., 0])
    dc = np.linalg.norm(a[..., 1:] - b[..., 1:], axis=-1)
    print(f"Oklab lightness      mean {dl.mean():.4f}   p99 {np.percentile(dl,99):.4f}")
    print(f"Oklab chroma         mean {dc.mean():.4f}   p99 {np.percentile(dc,99):.4f}")
    # A constant offset is a different answer from noise, and says something
    # different: a bias means a transform is wrong, scatter means precision.
    bias = (ours - theirs).mean(axis=(0, 1))
    print(f"per-channel bias     R {bias[0]*255:+.2f}  G {bias[1]*255:+.2f}  B {bias[2]*255:+.2f}")
    verdict = "agree" if delta.mean() * 255 < 2.0 and dl.mean() < 0.01 else "DISAGREE"
    print(f"\nverdict: {verdict}")
    if args.report:
        gap = np.ones((ours.shape[0], 16, 3))
        sheet = np.concatenate([ours, gap, theirs, gap, np.clip(delta * 8, 0, 1)], axis=1)
        cv2.imwrite(args.report, (np.clip(sheet, 0, 1)[:, :, ::-1] * 255).astype(np.uint8))
        print(f"wrote {args.report} (ours | Resolve | difference x8)")


if __name__ == "__main__":
    main()
