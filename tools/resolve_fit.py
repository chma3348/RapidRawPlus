#!/usr/bin/env python3
"""Identify what one Resolve control does, from a lattice capture.

The output transform was captured, not fitted. The controls cannot be
captured the same way — a slider is a continuum, not one transform — so
each is captured at a known setting and the operation identified from it.

The captures are made with the project's *output* colour space set to
DaVinci WG/Intermediate, the same as the timeline. Then Resolve applies no
output transform at all, and a capture is the control's own operation in
Resolve's working encoding: lattice value in, graded value out, both
Intermediate. Nothing has to be undone before it can be read.

For each capture this reports:

- **pointwise or spatial.** Each control is captured twice, on the ordinary
  lattice and on a scrambled one. A pointwise operation gives every
  lattice entry the same answer either way; one that looks at neighbouring
  pixels does not, and cannot be represented by any formula or LUT here.
- **per channel or mixing.** Whether each output channel depends only on the
  same input channel. Contrast and lift/gamma/gain usually do; saturation,
  hue and white balance cannot.
- **the curve.** For per-channel operations, the response along the neutral
  axis, and the best of a few candidate forms with its residual.
- **for mixing operations**, how well a luminance-anchored scaling explains
  it, with the luminance weights that fit best.

    resolve_fit.py make-scrambled --output lattice-scrambled.tiff
    resolve_fit.py identify --capture contrast-1.5.tif \\
        [--scrambled contrast-1.5-scrambled.tif] --report contrast.json
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import cv2
import numpy as np

import importlib.util
import sys

_spec = importlib.util.spec_from_file_location(
    "resolve_drt", Path(__file__).with_name("resolve_drt.py")
)
drt = importlib.util.module_from_spec(_spec)
sys.modules["resolve_drt"] = drt
_spec.loader.exec_module(drt)

SIZE = 64
SEED = 20260920


def permutation(n: int) -> np.ndarray:
    """A fixed shuffle of lattice positions, the same every time."""
    return np.random.default_rng(SEED).permutation(n)


def load(path: str) -> np.ndarray:
    raw = cv2.imread(path, cv2.IMREAD_UNCHANGED)
    if raw is None:
        raise SystemExit(f"could not read {path}")
    full = {np.dtype(np.uint8): 255.0, np.dtype(np.uint16): 65535.0}.get(raw.dtype, 1.0)
    width, height = drt.layout(SIZE)
    if raw.shape[1] != width or raw.shape[0] != height:
        raise SystemExit(
            f"{path} is {raw.shape[1]}x{raw.shape[0]}; captures must be {width}x{height} "
            "with no resizing"
        )
    return (raw[:, :, :3][:, :, ::-1].astype(np.float64) / full).reshape(-1, 3)


def make_scrambled(args: argparse.Namespace) -> None:
    lattice = drt.lattice(SIZE)
    scrambled = lattice[permutation(len(lattice))]
    width, height = drt.layout(SIZE)
    image = (np.clip(scrambled, 0, 1) * 65535.0 + 0.5).astype(np.uint16).reshape(height, width, 3)
    if not cv2.imwrite(args.output, image[:, :, ::-1]):
        raise SystemExit(f"could not write {args.output}")
    print(f"scrambled {SIZE}^3 lattice -> {args.output}")


def neutral_indices() -> np.ndarray:
    i = np.arange(SIZE)
    return i + (i + i * SIZE) * SIZE


def unclipped(y: np.ndarray) -> np.ndarray:
    """A 16-bit TIFF cannot hold values below 0 or above 1, so strong
    adjustments clip at the ends. Clipped samples say nothing about the
    operation and would drag any fit toward a shallower answer."""
    edge = 2.0 / 65535.0
    return (y > edge) & (y < 1.0 - edge)


def fit_curve(x: np.ndarray, y: np.ndarray) -> dict:
    """Try a few forms for a 1D response in Intermediate, keep the best."""
    keep = unclipped(y)
    if keep.sum() < 8:
        return {"best": None, "candidates": {}, "note": "almost everything clipped"}
    x, y = x[keep], y[keep]
    candidates = {}
    # Linear in Intermediate: contrast about a pivot, offset, gain in log.
    a, b = np.polyfit(x, y, 1)
    candidates["linear"] = (a * x + b, {"slope": a, "intercept": b,
                                        "pivot": (b / (1 - a)) if abs(1 - a) > 1e-6 else None})
    # Linear in linear light: a gain.
    xl, yl = drt.intermediate_to_linear(x), drt.intermediate_to_linear(y)
    keep = xl > 1e-4
    g = float(np.median(yl[keep] / xl[keep]))
    candidates["gain_linear"] = (drt.linear_to_intermediate(g * xl), {"gain": g, "stops": float(np.log2(g))})
    # A power in normalised Intermediate: a gamma.
    ok = (x > 0.02) & (y > 0.02)
    p = float(np.polyfit(np.log(x[ok]), np.log(y[ok]), 1)[0])
    candidates["power"] = (np.clip(x, 0, None) ** p, {"exponent": p})
    report = {}
    for name, (pred, params) in candidates.items():
        report[name] = {"rms": float(np.sqrt(np.mean((pred - y) ** 2))), **params}
    best = min(report, key=lambda k: report[k]["rms"])
    return {"best": best, "candidates": report, "samples": int(keep.sum())}


def identify(args: argparse.Namespace) -> None:
    lattice = drt.lattice(SIZE)
    captured = load(args.capture)
    result: dict = {"capture": args.capture}

    moved = float(np.abs(captured - lattice).max())
    result["max_change"] = moved
    if moved < 2.0 / 255.0:
        raise SystemExit("The capture matches the lattice: the control had no effect, or was not applied.")

    if args.scrambled:
        scrambled = load(args.scrambled)
        # Put the scrambled answers back in lattice order.
        order = np.empty(len(lattice), dtype=np.int64)
        order[permutation(len(lattice))] = np.arange(len(lattice))
        unscrambled = scrambled[order]
        disagreement = float(np.abs(unscrambled - captured).mean())
        result["spatial_disagreement"] = disagreement
        result["pointwise"] = disagreement < 1.0 / 255.0
    else:
        result["pointwise"] = None

    # Per channel: does output channel c depend only on input channel c?
    # Group lattice entries by their value in channel c and measure how much
    # the output in c varies within a group.
    grid = lattice.reshape(SIZE, SIZE, SIZE, 3)  # [b][g][r][c]
    out = captured.reshape(SIZE, SIZE, SIZE, 3)
    spread = []
    for c, axis in ((0, 2), (1, 1), (2, 0)):
        # Move this channel's own axis to the front; the rest are "others".
        channel = np.moveaxis(out[..., c], axis, 0).reshape(SIZE, -1)
        live = unclipped(channel)
        rows = [row[m].std() for row, m in zip(channel, live) if m.sum() > 1]
        spread.append(float(np.mean(rows)) if rows else 0.0)
    result["cross_channel_spread"] = spread
    per_channel = max(spread) < 1.0 / 255.0
    result["per_channel"] = per_channel

    neutral = neutral_indices()
    x = lattice[neutral, 0]
    result["neutral_curve"] = {
        "input": x.tolist(),
        "output": captured[neutral].tolist(),
    }
    if per_channel:
        result["fit"] = [fit_curve(lattice[neutral, c], captured[neutral, c]) for c in range(3)]
    else:
        # Mixing: test y = L + k (x - L) in linear light, L a weighted mean.
        keep = unclipped(captured).all(axis=1)
        lin_in = drt.intermediate_to_linear(lattice[keep])
        lin_out = drt.intermediate_to_linear(captured[keep])
        target = captured[keep]
        best = None
        for name, w in {
            "rec709": [0.2126, 0.7152, 0.0722],
            "dwg": [0.27411851, 0.87363190, -0.14775041],
            "equal": [1 / 3, 1 / 3, 1 / 3],
        }.items():
            w = np.array(w)
            lum_in = lin_in @ w
            lum_out = lin_out @ w
            num = ((lin_out - lum_out[:, None]) * (lin_in - lum_in[:, None])).sum()
            den = ((lin_in - lum_in[:, None]) ** 2).sum()
            k = num / den
            pred = lum_in[:, None] + k * (lin_in - lum_in[:, None])
            rms = float(np.sqrt(np.mean((drt.linear_to_intermediate(np.clip(pred, -0.005, None))
                                         - target) ** 2)))
            luminance_kept = float(np.abs(lum_out - lum_in).mean())
            entry = {"weights": name, "scale": float(k), "rms": rms, "luminance_change": luminance_kept}
            if best is None or rms < best["rms"]:
                best = entry
        result["mixing_fit"] = best

    Path(args.report).write_text(json.dumps(result, indent=1) + "\n")
    if result["pointwise"] is False:
        summary = [
            f"change: up to {moved * 255:.1f}/255",
            "SPATIAL: this control looks at neighbouring pixels "
            f"(scrambled lattice disagrees by {result['spatial_disagreement'] * 255:.2f}/255). "
            "No formula or LUT from a lattice can represent it; it needs an image-based comparison.",
        ]
        Path(args.report).write_text(json.dumps(result, indent=1) + "\n")
        print("\n".join(summary))
        return
    summary = [
        f"change: up to {moved * 255:.1f}/255",
        f"pointwise: {result['pointwise']}",
        f"per channel: {per_channel} (spread {max(spread) * 255:.2f}/255)",
    ]
    if per_channel:
        fits = result["fit"]
        summary.append("neutral-axis form: " + ", ".join(
            f"{'RGB'[c]}={f['best']} (rms {f['candidates'][f['best']]['rms'] * 255:.2f}/255)"
            if f["best"] else f"{'RGB'[c]}=clipped"
            for c, f in enumerate(fits)))
    else:
        m = result["mixing_fit"]
        summary.append(f"mixing: {m['weights']} luminance, scale {m['scale']:.3f}, "
                       f"rms {m['rms'] * 255:.2f}/255")
    print("\n".join(summary))


def self_test(_: argparse.Namespace) -> None:
    """Synthetic captures with known answers, so the identification is itself tested."""
    import tempfile
    lattice = drt.lattice(SIZE)
    width, height = drt.layout(SIZE)

    def write(values: np.ndarray, name: str, directory: Path) -> str:
        path = directory / name
        image = (np.clip(values, 0, 1) * 65535 + 0.5).astype(np.uint16).reshape(height, width, 3)
        cv2.imwrite(str(path), image[:, :, ::-1])
        return str(path)

    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        perm = permutation(len(lattice))
        # Contrast 1.5 about 0.435, per channel, pointwise.
        contrast = (lattice - 0.435) * 1.5 + 0.435
        a = argparse.Namespace(capture=write(contrast, "c.tif", d),
                               scrambled=write(contrast[perm], "cs.tif", d), report=str(d / "c.json"))
        identify(a)
        r = json.loads(Path(a.report).read_text())
        assert r["pointwise"] and r["per_channel"], r
        f = r["fit"][0]["candidates"]["linear"]
        assert abs(f["slope"] - 1.5) < 0.01 and abs(f["pivot"] - 0.435) < 0.01, f
        # Saturation 1.4 in linear light about Rec.709 luminance: mixing.
        lin = drt.intermediate_to_linear(lattice)
        lum = lin @ np.array([0.2126, 0.7152, 0.0722])
        sat = drt.linear_to_intermediate(np.clip(lum[:, None] + 1.4 * (lin - lum[:, None]), 0, None))
        a = argparse.Namespace(capture=write(sat, "s.tif", d), scrambled=None, report=str(d / "s.json"))
        identify(a)
        r = json.loads(Path(a.report).read_text())
        assert not r["per_channel"], r
        assert abs(r["mixing_fit"]["scale"] - 1.4) < 0.05 and r["mixing_fit"]["weights"] == "rec709", r
        # A spatial operation: each entry nudged by its neighbour in the image.
        img = lattice.reshape(height, width, 3)
        spatial = (0.8 * img + 0.2 * np.roll(img, 1, axis=1)).reshape(-1, 3)
        img_s = lattice[perm].reshape(height, width, 3)
        spatial_s = (0.8 * img_s + 0.2 * np.roll(img_s, 1, axis=1)).reshape(-1, 3)
        a = argparse.Namespace(capture=write(spatial, "x.tif", d),
                               scrambled=write(spatial_s, "xs.tif", d), report=str(d / "x.json"))
        identify(a)
        r = json.loads(Path(a.report).read_text())
        assert r["pointwise"] is False, r
    print("\nself-test passed: contrast, saturation and a spatial operation all identified")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    m = sub.add_parser("make-scrambled")
    m.add_argument("--output", required=True)
    m.set_defaults(func=make_scrambled)
    i = sub.add_parser("identify")
    i.add_argument("--capture", required=True)
    i.add_argument("--scrambled")
    i.add_argument("--report", required=True)
    i.set_defaults(func=identify)
    t = sub.add_parser("self-test")
    t.set_defaults(func=self_test)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
