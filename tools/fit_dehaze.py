#!/usr/bin/env python3
"""Fit negative Dehaze (adding haze) to Lightroom's, and write its table
(src-tauri/src/color_engine/dehaze_table.rs).

Lightroom's negative Dehaze is, measured on 16 photos, much the same on every
photo: a veil curve (black lifted to about L* 30 at -100, the rest
compressed toward white, detail kept in proportion to the curve's slope),
colour pulled toward the veil (about 35-45% kept at -100, 60-80% at -50),
and the veil taking the photo's own haze colour (the brightest of its darkest
channel). So it is fitted as those three things, from adobe_detail.py's
measurements:

- the veil curve as grey targets on the tonal key, at -50 and -100, smoothed
  and never reversing (fit_lighting.py's tables);
- the colour kept, at -50 and -100;
- how strongly the veil takes the haze colour.

    uv run --with numpy --with scipy tools/fit_dehaze.py seed MEASURE.json
    uv run --with numpy --with scipy tools/fit_dehaze.py correct MEASURE.json [DAMPING]
"""
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fit_lighting as F  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE = os.path.join(HERE, "..", "src-tauri/src/color_engine/dehaze_table.rs")
TARGETS = os.path.join(HERE, "dehaze_targets.json")
VALUES = (-50, -100, 50, 100)
SLOPE_RANGE = (0.05, 2.2)


def pooled(r, engine, key, value):
    rows = [p["dehaze"][f"{value:g}"][engine][key] for p in r["photos"].values()
            if "dehaze" in p and f"{value:g}" in p["dehaze"]]
    return np.nanmedian(np.array([[np.nan if x is None else x for x in row] for row in rows], dtype=float), axis=0)


def on_grid(r, engine, value):
    """The change in L* by unedited L*, on fit_lighting's grid."""
    bins = np.array(r["bins"])
    centres = (bins[:-1] + bins[1:]) / 2
    d = pooled(r, engine, "tone", value)
    ok = np.isfinite(d)
    return np.interp(F.GRID, centres[ok], d[ok])


def chroma_keep(r, engine, value):
    """Colour kept, pooled over the midtones (L* 20-90)."""
    ch = pooled(r, engine, "chroma", value)
    return float(np.nanmedian(ch[4:18]))


def write(state):
    tables = [F.table_from_targets(np.array(state["tone"][str(v)]), SLOPE_RANGE) for v in VALUES]
    for name in ("50", "100"):
        state["keep"].setdefault(name, 1.0)
    lines = [
        "//! RapidRAW's negative Dehaze (adding haze), fitted to Lightroom's by",
        "//! tools/fit_dehaze.py from tools/adobe_detail.py's measurements; do",
        "//! not edit.",
        "//!",
        "//! `VEIL`: offsets on the tonal key, 65 knots over 0..1, at -50, -100,",
        "//! +50 and +100 (for positive Dehaze, on top of the detail stage's haze",
        "//! removal). `KEEP`: the colour kept at -50 and -100 (Oklab chroma, the",
        "//! rest pulled to the veil), and the colour scale at +50 and +100.",
        "//! `TINT`: how strongly the veil takes the photo's haze colour.",
        "",
        f"pub const KNOTS: usize = {F.KNOTS};",
        "pub const VEIL: [[f32; 4]; KNOTS] = [",
    ]
    lines += [f"    [{a:.6f}, {b:.6f}, {c:.6f}, {d:.6f}]," for a, b, c, d in zip(*tables)]
    lines.append("];")
    k = state["keep"]
    lines.append(f"pub const KEEP: [f32; 4] = [{k['-50']:.4f}, {k['-100']:.4f}, {k['50']:.4f}, {k['100']:.4f}];")
    lines.append(f"pub const TINT: f32 = {state['tint']:.4f};")
    lines.append("/// How much light scatters (mixed toward a fine and a broad blur) at -50")
    lines.append("/// and -100: [fine, broad].")
    b = state["scatter"]
    lines.append(f"pub const SCATTER: [[f32; 2]; 2] = [[{b['-50'][0]:.4f}, {b['-50'][1]:.4f}], [{b['-100'][0]:.4f}, {b['-100'][1]:.4f}]];")
    open(TABLE, "w").write("\n".join(lines) + "\n")
    import subprocess
    subprocess.run(["rustfmt", "--edition", "2024", TABLE], check=False)
    json.dump(state, open(TARGETS, "w"), indent=1)
    print("wrote", os.path.normpath(TABLE))


def seed(path):
    r = json.load(open(path))
    state = {"tone": {}, "keep": {}, "tint": 1.6, "scatter": {"-50": [0.04, 0.12], "-100": [0.16, 0.32]}}
    for v in VALUES:
        state["tone"][str(v)] = np.clip(np.maximum.accumulate(F.GRID + on_grid(r, "adobe", v)), 0, F.TOP).tolist()
        state["keep"][str(v)] = chroma_keep(r, "adobe", v)
    write(state)


def correct(path, damping=0.8):
    r = json.load(open(path))
    state = json.load(open(TARGETS))
    for v in VALUES:
        miss = on_grid(r, "adobe", v) - on_grid(r, "ours", v)
        t = np.array(state["tone"][str(v)]) + damping * miss
        state["tone"][str(v)] = np.clip(np.maximum.accumulate(t), 0, F.TOP).tolist()
        want, got = chroma_keep(r, "adobe", v), chroma_keep(r, "ours", v)
        state["keep"][str(v)] = float(np.clip(state["keep"][str(v)] * (want / max(got, 1e-3)) ** damping, 0.02, 1.0 if v < 0 else 2.0))
        if v > 0:
            print(f"dehaze {v:+}: tone still missing {np.sqrt(np.mean(miss ** 2)):.2f} L* rms (worst {np.abs(miss).max():.1f}); "
                  f"colour {got:.2f} vs Lightroom {want:.2f}")
            continue
        # Scattering: the fine bands' and the middle bands' detail against
        # Lightroom's (the broad blur takes everything below its size, the
        # fine one the finest only).
        ga, go = pooled(r, "adobe", "gain", v), pooled(r, "ours", "gain", v)
        # Measured: each 0.01 of scattering costs about 0.03 of the detail
        # kept (more in the shadows, where linear-light blur fills in).
        sensitivity = 3.0
        f_fine, f_broad = state["scatter"][str(v)]
        f_broad = float(np.clip(f_broad + damping * (go[3:6].mean() - ga[3:6].mean()) / sensitivity, 0, 0.8))
        total = float(np.clip(f_fine + state["scatter"][str(v)][1] + damping * (go[:2].mean() - ga[:2].mean()) / sensitivity, f_broad, 0.9))
        f_fine = total - f_broad
        state["scatter"][str(v)] = [f_fine, f_broad]
        print(f"   detail kept fine/mid ours {go[:2].mean():.2f}/{go[3:6].mean():.2f} vs Lightroom {ga[:2].mean():.2f}/{ga[3:6].mean():.2f} -> scatter {f_fine:.3f}/{f_broad:.3f}")
        print(f"dehaze {v}: tone still missing {np.sqrt(np.mean(miss ** 2)):.2f} L* rms (worst {np.abs(miss).max():.1f}); "
              f"colour kept {got:.2f} vs Lightroom {want:.2f}")
    write(state)


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "seed":
        seed(sys.argv[2])
    elif len(sys.argv) >= 3 and sys.argv[1] == "correct":
        correct(sys.argv[2], float(sys.argv[3]) if len(sys.argv) > 3 else 0.8)
    else:
        print(__doc__)
