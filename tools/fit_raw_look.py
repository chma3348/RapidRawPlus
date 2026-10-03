#!/usr/bin/env python3
"""Fit Darkroom Index's RAW starting look (color_engine/raw_look.rs) to Lightroom's
default rendering of the same RAWs, and write its table
(src-tauri/src/color_engine/raw_look_table.rs).

Measured pixel by pixel on pairs: Darkroom Index's render of a RAW and Lightroom's
unedited export of it ("Adobe no edits"), Lightroom's resized onto ours by
area. Only RAWs: rendered photographs already open alike in both.

- Tone: where a tone of ours (L*, before the look) lands in Lightroom's,
  pooled over every RAW, as grey targets on the tonal key (the same smoothed,
  never-reversing curves as the lighting sliders; fit_lighting.py).
- Colour: Lightroom's chroma over ours, and its hue turn, by hue and by
  developed tone, in Oklab; smoothed around the hue circle.

    uv run --with numpy --with scipy --with tifffile --with imagecodecs --with pillow \\
        tools/fit_raw_look.py seed BASE_DIR
    ... tools/fit_raw_look.py correct BASE_DIR CURRENT_DIR [DAMPING]

BASE_DIR holds examples/adobe_sweep renders of the RAWs without the look
(their `__neutral_0` files); CURRENT_DIR the same with the current look.
`seed` starts the tone from BASE alone (colour left as is); `correct` moves
tone and colour by what CURRENT still misses. Targets are kept beside the
table (raw_look_targets.json).
"""
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fit_lighting as F  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE = os.path.join(HERE, "..", "src-tauri/src/color_engine/raw_look_table.rs")
TARGETS = os.path.join(HERE, "raw_look_targets.json")
ADOBE = os.path.expanduser("~/Desktop/Adobe references/Adobe no edits")
RAW = ["DSC03453", "DSC03458", "DSC03532", "DSC03545", "DSC07955", "_AAF8641", "_AAF8720"]
HUES, BANDS = 16, 9
# Lightroom's tone curve is steep in the shadows (it lifts ours a stop and
# more) and flattens toward white, so the slopes allowed are wider than
# for the sliders.
TONE_SLOPE_RANGE = (0.1, 4.0)

ADOBE_RGB_TO_XYZ = np.array([[0.5767309, 0.1855540, 0.1881852],
                             [0.2973769, 0.6273491, 0.0752741],
                             [0.0270343, 0.0706872, 0.9911085]])
SRGB_TO_XYZ = np.array([[0.4124564, 0.3575761, 0.1804375],
                        [0.2126729, 0.7151522, 0.0721750],
                        [0.0193339, 0.1191920, 0.9503041]])
XYZ_TO_LMS = np.array([[0.8189330101, 0.3618667424, -0.1288597137],
                       [0.0329845436, 0.9293118715, 0.0361456387],
                       [0.0482003018, 0.2643662691, 0.6338517070]])
LMS_TO_LAB = np.array([[0.2104542553, 0.7936177850, -0.0040720468],
                       [1.9779984951, -2.4285922050, 0.4505937099],
                       [0.0259040371, 0.7827717662, -0.8086757660]])


def oklab(xyz):
    return np.cbrt(np.clip(xyz @ XYZ_TO_LMS.T, 0, None)) @ LMS_TO_LAB.T


def lightness(Y):
    Y = np.clip(Y, 0, None)
    f = np.where(Y > (6 / 29) ** 3, np.cbrt(Y), Y / (3 * (6 / 29) ** 2) + 4 / 29)
    return 116 * f - 16


def ours_xyz(d, photo):
    meta = json.load(open(f"{d}/{photo}__neutral_0.json"))
    enc = np.fromfile(f"{d}/{photo}__neutral_0.f32", dtype="<f4").reshape(meta["height"], meta["width"], 3)
    lin = np.where(enc <= 0.04045, enc / 12.92, ((enc + 0.055) / 1.055) ** 2.4)
    return lin @ SRGB_TO_XYZ.T


def adobe_xyz(photo, shape):
    import tifffile
    from PIL import Image
    raw = tifffile.imread(f"{ADOBE}/{photo}.tif")[..., :3].astype(np.float32) / 65535
    xyz = (raw ** (563 / 256)) @ ADOBE_RGB_TO_XYZ.T
    return np.stack([np.array(Image.fromarray(xyz[..., c].astype(np.float32)).resize((shape[1], shape[0]), Image.BOX))
                     for c in range(3)], -1)


def pairs(base, current=None):
    """Per RAW: (L* before the look, current L*, Lightroom's L*, current Oklab, Lightroom's Oklab)."""
    out = []
    for p in RAW:
        b = ours_xyz(base, p)
        a = adobe_xyz(p, b.shape)
        c = ours_xyz(current, p) if current else b
        out.append((lightness(b[..., 1]), lightness(c[..., 1]), lightness(a[..., 1]), oklab(c), oklab(a)))
    return out


def by_tone(xs, ys):
    """Median of ys for xs at each L* of the grid (within 1 L*), pooled."""
    x = np.concatenate([v.ravel() for v in xs])
    y = np.concatenate([v.ravel() for v in ys])
    order = np.argsort(x)
    x, y = x[order], y[order]
    out = np.full(len(F.GRID), np.nan)
    for i, g in enumerate(F.GRID):
        lo, hi = np.searchsorted(x, [g - 1, g + 1])
        if hi - lo >= 200:
            out[i] = np.median(y[lo:hi])
    ok = ~np.isnan(out)
    return np.interp(F.GRID, F.GRID[ok], out[ok])


def tone_table(targets):
    return F.table_from_targets(np.array(targets), TONE_SLOPE_RANGE)


def smooth_hue(t, passes=2):
    for _ in range(passes):
        t = 0.25 * np.roll(t, 1, axis=-1) + 0.5 * t + 0.25 * np.roll(t, -1, axis=-1)
    return t


def write(state):
    tone = tone_table(state["tone"])
    colour = np.array(state["colour"])
    lines = [
        "//! Darkroom Index's RAW starting look, fitted to Lightroom's default rendering",
        "//! by tools/fit_raw_look.py; do not edit.",
        "//!",
        "//! `TONE`: offsets on the tonal key (DaVinci Intermediate), 65 knots over",
        "//! 0..1. `COLOUR[band][hue]`: [Oklab chroma scale, hue turn in radians],",
        "//! bands over the developed tonal key 0..1, hues around the circle from",
        "//! Oklab's +a axis.",
        "",
        f"pub const KNOTS: usize = {F.KNOTS};",
        f"pub const HUES: usize = {HUES};",
        f"pub const BANDS: usize = {BANDS};",
        "pub const TONE: [f32; KNOTS] = [",
    ]
    lines += [f"    {v:.6f}," for v in tone]
    lines.append("];")
    lines.append("pub const COLOUR: [[[f32; 2]; HUES]; BANDS] = [")
    for band in colour:
        lines.append("    [" + ", ".join(f"[{s:.4f}, {h:.4f}]" for s, h in band) + "],")
    lines.append("];")
    open(TABLE, "w").write("\n".join(lines) + "\n")
    # CI checks formatting (cargo fmt --check); write what rustfmt would.
    import subprocess
    subprocess.run(["rustfmt", "--edition", "2024", TABLE], check=False)
    json.dump(state, open(TARGETS, "w"), indent=1)
    print("wrote", os.path.normpath(TABLE))


def identity_colour():
    return [[[1.0, 0.0] for _ in range(HUES)] for _ in range(BANDS)]


def seed(base):
    ps = pairs(base)
    targets = np.clip(np.maximum.accumulate(by_tone([p[0] for p in ps], [p[2] for p in ps])), 0, F.TOP)
    state = json.load(open(TARGETS)) if os.path.exists(TARGETS) else {}
    state["tone"] = targets.tolist()
    state.setdefault("colour", identity_colour())
    write(state)


def colour_misses(ps):
    """Per (band, hue): Lightroom's chroma over ours and its hue turn, with
    the pixel count, from the current renders."""
    ratio = np.ones((BANDS, HUES))
    turn = np.zeros((BANDS, HUES))
    count = np.zeros((BANDS, HUES))
    cols = {k: [] for k in ("band", "hue", "co", "ca", "dh")}
    for _, Lc, La, lc, la in ps:
        co = np.hypot(lc[..., 1], lc[..., 2])
        ca = np.hypot(la[..., 1], la[..., 2])
        ok = (co > 0.02) & (ca > 0.01) & (Lc > 3) & (Lc < 97) & (La < 99)
        hue = np.arctan2(lc[..., 2], lc[..., 1])[ok]
        dh = np.angle(np.exp(1j * (np.arctan2(la[..., 2], la[..., 1])[ok] - hue)))
        key = F.key_of(Lc[ok])
        cols["band"].append(np.clip(np.rint(key * (BANDS - 1)), 0, BANDS - 1).astype(int))
        cols["hue"].append((np.floor(np.mod(hue, 2 * np.pi) / (2 * np.pi) * HUES + 0.5) % HUES).astype(int))
        cols["co"].append(co[ok])
        cols["ca"].append(ca[ok])
        cols["dh"].append(dh)
    c = {k: np.concatenate(v) for k, v in cols.items()}
    for b in range(BANDS):
        for h in range(HUES):
            m = (c["band"] == b) & (c["hue"] == h)
            n = int(m.sum())
            count[b, h] = n
            if n >= 300:
                ratio[b, h] = np.median(c["ca"][m]) / np.median(c["co"][m])
                w = c["co"][m]
                turn[b, h] = np.angle(np.sum(w * np.exp(1j * c["dh"][m])))
    return ratio, turn, count


def correct(base, current, damping=0.8):
    state = json.load(open(TARGETS))
    ps = pairs(base, current)
    # Tone: by the tone before the look, what Lightroom shows less what we do.
    want = by_tone([p[0] for p in ps], [p[2] for p in ps])
    got = by_tone([p[0] for p in ps], [p[1] for p in ps])
    miss = want - got
    t = np.array(state["tone"]) + damping * miss
    state["tone"] = np.clip(np.maximum.accumulate(t), 0, F.TOP).tolist()
    print(f"tone: still missing {np.sqrt(np.mean(miss ** 2)):.2f} L* rms, worst {np.abs(miss).max():.2f}")
    # Colour: multiply the scale, add the turn, where there is enough to go
    # on; a cell with few pixels leans on its neighbours around the circle.
    ratio, turn, count = colour_misses(ps)
    weight = np.clip(count / 2000.0, 0, 1)
    colour = np.array(state["colour"])
    step_s = np.exp(damping * weight * np.log(np.clip(ratio, 0.5, 2.0)))
    step_h = damping * weight * np.clip(turn, -0.3, 0.3)
    scale = smooth_hue(np.clip(colour[..., 0] * step_s, 0.4, 2.5))
    hue = smooth_hue(np.clip(colour[..., 1] + step_h, -0.4, 0.4))
    # Across bands too, gently.
    scale[1:-1] = 0.25 * scale[:-2] + 0.5 * scale[1:-1] + 0.25 * scale[2:]
    hue[1:-1] = 0.25 * hue[:-2] + 0.5 * hue[1:-1] + 0.25 * hue[2:]
    state["colour"] = np.stack([scale, hue], -1).tolist()
    big = count >= 300
    print(f"colour: chroma off by {np.exp(np.sqrt(np.mean(np.log(ratio[big]) ** 2))) - 1:+.1%} rms, "
          f"hue by {np.degrees(np.sqrt(np.mean(turn[big] ** 2))):.1f} deg rms, over {big.sum()} cells")
    write(state)


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "seed":
        seed(sys.argv[2])
    elif len(sys.argv) >= 4 and sys.argv[1] == "correct":
        correct(sys.argv[2], sys.argv[3], float(sys.argv[4]) if len(sys.argv) > 4 else 0.8)
    else:
        print(__doc__)
