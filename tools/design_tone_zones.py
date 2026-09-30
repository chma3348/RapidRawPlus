#!/usr/bin/env python3
"""Design RapidRAW's four tone zones (Blacks, Shadows, Highlights, Whites) from
on-screen intent, and write the engine's curve tables.

Each slider is specified the way it is judged: where a grey that shows at a
given 8-bit display level should land at +100 and at -100. Levels become
positions on the engine's tonal key (DaVinci Intermediate luminance) through
the captured output transform's grey axis, so the zones sit where shadows
and highlights are actually perceived.

From those targets each zone gets a curve that:
  - is a monotone PCHIP through the targets, so it can never reverse tones;
  - is exactly "no change" outside its zone, joined with matching slope, so
    it never bleeds into the rest of the picture (padding points just outside
    the zone force the join);
  - holds its end offset beyond an end that moves (the black point for
    Blacks, the top of the range for Highlights and Whites).
The engine applies the zones one after another (Blacks, Shadows, Highlights,
Whites), each on the previous one's result: every step is monotone at any
strength, so no combination of sliders can reverse tones either.

    <python with numpy, scipy> tools/design_tone_zones.py

Reads the installed output-transform.cube; writes
src-tauri/src/color_engine/tone_zones_table.rs.
"""
import itertools
import os

import numpy as np
from scipy.interpolate import PchipInterpolator

SUP = os.path.expanduser("~/Library/Application Support/io.github.CyberTimon.RapidRAW")
OUT = os.path.join(os.path.dirname(__file__), "..", "src-tauri/src/color_engine/tone_zones_table.rs")
KNOTS = 65
ZONES = ["blacks", "shadows", "highlights", "whites"]


def read_cube(path):
    size, rows = None, []
    for line in open(path):
        t = line.strip()
        if not t or t.startswith("#"):
            continue
        if t.startswith("LUT_3D_SIZE"):
            size = int(t.split()[1]); continue
        if t[0].isalpha():
            continue
        rows.append([float(x) for x in t.split()[:3]])
    return np.array(rows).reshape(size, size, size, 3)


ODT = read_cube(os.path.join(SUP, "output-transform.cube"))
_n = ODT.shape[0]
_diag = np.array([ODT[i, i, i].mean() for i in range(_n)]) * 255
_keys = np.linspace(0, 1, 8193)
_levels = np.interp(_keys, np.linspace(0, 1, _n), _diag)
TOP = float(_levels[-1])


def key_of(level):
    return float(np.interp(level, _levels, _keys))


def level_of(key):
    return np.interp(key, _keys, _levels)


# On-screen intent at +/-100: (display level in, display level out).
INTENT = {
    # Blacks: the bottom end only. + lifts the black point to a soft matte
    # (pure black to about level 10); - crushes toward black.
    ("blacks", +1): [(0, 10), (3, 13), (8, 17), (15, 22), (25, 30), (40, 42), (60, 60)],
    ("blacks", -1): [(3, 0.3), (8, 1.5), (15, 5), (25, 14), (40, 33), (60, 57), (75, 75)],
    # Shadows: the lower range, tapering through the lower midtones to
    # nothing by level 160; middle grey moves at most ~9 levels at +/-100.
    # + opens shadows about 1.8 stops where they are deepest. Pinning middle
    # grey exactly would squeeze everything between into a flat band.
    ("shadows", +1): [(0, 0), (5, 14), (20, 68), (40, 98), (64, 110), (90, 118), (110, 124),
                      (128, 130), (145, 146), (160, 160)],
    ("shadows", -1): [(0, 0), (10, 3), (20, 5), (40, 13), (64, 30), (90, 64), (110, 100),
                      (128, 125), (145, 145)],
    # Highlights: the bright range above middle grey, including what lies
    # beyond display white (- recovers it into view).
    ("highlights", +1): [(118, 118), (128, 129), (150, 162), (180, 200), (210, 228), (235, 246),
                         (TOP, TOP)],
    ("highlights", -1): [(118, 118), (128, 127), (150, 144), (180, 162), (210, 180), (230, 190),
                         (245, 203), (250, 212), (TOP, 222)],
    # Whites: the top end only. + pushes toward clipping through the output
    # transform's soft shoulder; - lowers the white point.
    ("whites", +1): [(185, 185), (200, 207), (215, 226), (230, 242), (242, 250), (TOP, TOP)],
    ("whites", -1): [(185, 185), (200, 195), (215, 206), (230, 216), (242, 225), (250, 232),
                     (TOP, 238)],
}


def offset_curve(zone, sign):
    pts = INTENT[(zone, sign)]
    kin = [key_of(a) for a, _ in pts]
    kout = [key_of(b) for _, b in pts]
    lo, hi = kin[0], kin[-1]
    if abs(kout[0] - kin[0]) < 1e-9 and lo > 0.05:
        kin = [lo - 0.04, lo - 0.02] + kin
        kout = [lo - 0.04, lo - 0.02] + kout
    if abs(kout[-1] - kin[-1]) < 1e-9 and hi < 0.95:
        kin = kin + [hi + 0.02, hi + 0.04]
        kout = kout + [hi + 0.02, hi + 0.04]
    kin, kout = np.array(kin), np.array(kout)
    keep = np.concatenate([[True], np.diff(kin) > 1e-6])
    kin, kout = kin[keep], kout[keep]
    f = PchipInterpolator(kin, kout, extrapolate=False)

    def offset(k):
        v = f(k) - k
        v = np.where(k < kin[0], kout[0] - kin[0], v)
        v = np.where(k > kin[-1], kout[-1] - kin[-1], v)
        return np.where(np.isnan(v), 0.0, v)

    return offset


KN = np.linspace(0, 1, KNOTS)
TABLES = {(z, s): offset_curve(z, s)(KN) for z in ZONES for s in (+1, -1)}


def apply(k, sliders):
    out = k.copy()
    for z in ZONES:
        v = sliders.get(z, 0.0)
        if v:
            out = np.maximum(out + abs(v) * np.interp(out, KN, TABLES[(z, 1 if v > 0 else -1)]), 0.0)
    return out


if __name__ == "__main__":
    k = np.linspace(0, 1, 20001)
    worst = min(
        (np.diff(apply(k, dict(zip(ZONES, c)))) / np.diff(k)).min()
        for c in itertools.product([-1, -0.5, 0, 0.5, 1], repeat=4)
    )
    assert worst >= -1e-9, f"tone reversal: slope {worst}"
    print(f"monotone over every slider combination (minimum slope {worst:.3f})")
    for z in ZONES:
        for sg in (+1, -1):
            o = apply(k, {z: sg})
            mid = level_of(np.interp(key_of(118), k, o)) - 118
            print(f"{z:10} {sg:+d}: middle grey moves {mid:+.1f} levels")
    for z in ZONES:
        for s in (+1, -1):
            o = apply(k, {z: s})
            print(f"{z:10} {s:+d}: " + " ".join(
                f"{L}->{level_of(np.interp(key_of(L), k, o)):.0f}" for L in [5, 20, 40, 64, 100, 128, 160, 200, 230, 250]))
    lines = [
        "//! RapidRAW's tone zones: Blacks, Shadows, Highlights, Whites. Generated by",
        "//! tools/design_tone_zones.py from on-screen intent; do not edit.",
        "//!",
        "//! Offsets on the tonal key (DaVinci Intermediate luminance), 65 knots over",
        "//! 0..1, per zone at slider +100 (`LIFT`) and -100 (`CUT`), in the order",
        "//! [blacks, shadows, highlights, whites]. The engine applies the zones in",
        "//! that order, each on the previous result, scaled by |slider| / 100.",
        "",
        f"pub const KNOTS: usize = {KNOTS};",
    ]
    for name, sign in (("LIFT", +1), ("CUT", -1)):
        lines.append(f"pub const {name}: [[f32; 4]; KNOTS] = [")
        for i in range(KNOTS):
            lines.append("    [" + ", ".join(f"{TABLES[(z, sign)][i]:.6f}" for z in ZONES) + "],")
        lines.append("];")
    open(OUT, "w").write("\n".join(lines) + "\n")
    print("wrote", os.path.normpath(OUT))
