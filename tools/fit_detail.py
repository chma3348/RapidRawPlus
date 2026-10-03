#!/usr/bin/env python3
"""Fit Clarity and Texture to Lightroom's, and write their table
(src-tauri/src/color_engine/detail_table.rs).

Lightroom's Clarity and Texture each change detail across a range of sizes,
not at one: Clarity +100 boosts the finest detail 1.55x and still the
broadest (200 px) 1.13x; Texture +100 1.32x the finest, fading out by 24 px
(adobe_detail.py). So each is a set of bands, local contrast in log
luminance at several sizes, every band with its own amount at +-50 and +-100.

Amounts are found by simulation (band_design: our unedited renders mapped
back to scene light through the output curve, each candidate applied as the
detail stage applies it, and measured as Lightroom's are), then corrected
from real renders: what the real render misses beyond what the simulation
predicted is added to the simulation's target, and the amounts re-solved.

    uv run --with numpy --with scipy --with tifffile --with imagecodecs --with pillow \\
        tools/fit_detail.py write
    ... tools/fit_detail.py correct CONTROL MEASURE.json OURS_DIR [DAMPING]
"""
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE = os.path.join(HERE, "..", "src-tauri/src/color_engine/detail_table.rs")
TARGETS = os.path.join(HERE, "detail_targets.json")
VALUES = ("50", "100", "-50", "-100")


def write(state):
    lines = [
        "//! Darkroom Index's Clarity and Texture bands, fitted to Lightroom's by",
        "//! tools/fit_detail.py from tools/adobe_detail.py's measurements; do not",
        "//! edit.",
        "//!",
        "//! Each control is local contrast at several sizes (`*_SIGMAS`, Gaussian",
        "//! sigma in full-resolution pixels), every band with its own amount at",
        "//! slider +50, +100, -50 and -100 (`CLARITY[band]`, `TEXTURE[band]`).",
        "",
    ]
    for name in ("clarity", "texture", "dehaze"):
        c = state[name]
        n = len(c["sigmas"])
        lines.append(f"pub const {name.upper()}_SIGMAS: [f32; {n}] = [" + ", ".join(f"{s:.1f}" for s in c["sigmas"]) + "];")
        lines.append(f"pub const {name.upper()}: [[f32; 4]; {n}] = [")
        for b in range(n):
            lines.append("    [" + ", ".join(f"{c['amounts'][v][b]:.4f}" for v in VALUES) + "],")
        lines.append("];")
    open(TABLE, "w").write("\n".join(lines) + "\n")
    import subprocess
    subprocess.run(["rustfmt", "--edition", "2024", TABLE], check=False)
    json.dump(state, open(TARGETS, "w"), indent=1)
    print("wrote", os.path.normpath(TABLE))


PHOTOS = ["DSC07955", "_AAF8641", "DSC08197", "DSC08016", "1C408836-F65A-407A-9B8E-308947EFC48A_1_105_c", "DSC03532"]
MID_GREY_LOG = -2.4739312
SIM_SCALE = 0.25


def simulation(base_dir, control, sigmas):
    """Our unedited renders as scene log luminance, and a function giving the
    detail gains a set of band amounts would produce (measured as Lightroom's
    are)."""
    import fit_lighting as F
    import adobe_detail as D
    from scipy.ndimage import gaussian_filter
    Ls = [D.ours(f"{base_dir}/{p}__neutral_0.f32")[::2, ::2, 0] for p in PHOTOS]
    logs = [np.log2(np.maximum(F.decode_di(F.key_of(L)), 1e-6)) for L in Ls]
    blurs = [[gaussian_filter(lg, max(s * SIM_SCALE, 0.35)) for s in sigmas] for lg in logs]
    midtones = control == "clarity"
    limit = 0.5 if control == "texture" else 1.0

    def gains(amounts):
        out = []
        for L, lg, bl in zip(Ls, logs, blurs):
            w = np.exp(-((lg - MID_GREY_LOG) / 3.0) ** 2) if midtones else 1.0
            o = lg.copy()
            for a, b in zip(amounts, bl):
                o += a * limit * np.tanh((lg - b) / limit) * w
            L1 = F.L_of_key(F.encode_di(2.0 ** o))
            z = 0 * L
            out.append(D.measure_pair(np.stack([L, z, z], -1), np.stack([L1, z, z], -1), SIM_SCALE)["gain"])
        return np.mean(np.array(out, dtype=float), 0)
    return gains


def correct(control, measure, base_dir, damping=0.8):
    import adobe_detail as D
    state = json.load(open(TARGETS))
    c = state[control]
    r = json.load(open(measure))
    gains = simulation(base_dir, control, c["sigmas"])
    for v in VALUES:
        want = D.pooled(r, control, float(v), "adobe", "gain")
        got = D.pooled(r, control, float(v), "ours", "gain")
        if want is None or got is None or not np.all(np.isfinite(want)):
            continue
        # Dehaze's bands are positive Dehaze's only; negative is a veil.
        if control == "dehaze" and float(v) < 0:
            continue
        a = np.array(c["amounts"][v], dtype=float)
        g0 = gains(a)
        J = np.stack([(gains(a + 0.03 * np.eye(len(a))[k]) - g0) / 0.03 for k in range(len(a))], 1)
        step, *_ = np.linalg.lstsq(J, want - got, rcond=None)
        c["amounts"][v] = (a + damping * step).tolist()
        print(f"{control} {v:>4}: gains miss {np.sqrt(np.mean((want - got) ** 2)):.3f} rms -> amounts "
              + " ".join(f"{x:+.3f}" for x in c["amounts"][v]))
    write(state)


if __name__ == "__main__":
    if len(sys.argv) >= 2 and sys.argv[1] == "write":
        write(json.load(open(TARGETS)))
    elif len(sys.argv) >= 5 and sys.argv[1] == "correct":
        correct(sys.argv[2], sys.argv[3], sys.argv[4], float(sys.argv[5]) if len(sys.argv) > 5 else 0.8)
    else:
        print(__doc__)
