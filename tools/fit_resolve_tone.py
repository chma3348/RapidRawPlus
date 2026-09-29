#!/usr/bin/env python3
"""Measure a Resolve Photo-page tone slider (Shadows or Highlights) from the
chart exports and write v3's response table.

Both sliders, read off `chart-all` at -100, -50, +50 and +100 against the
neutral export, behave as one gain in linear light applied to all three
channels, keyed on a blurred luminance: flat interiors are pointwise (the
same grey lands on the same value everywhere within a level), and dark
channels of saturated colours stay dark (a log-domain lift would raise them
through the Intermediate toe; Resolve does not). Shadows' key blur is about
40 px wide at a 1450 px short edge; Highlights' is a few pixels. This script
fits the gain in stops as a function of the key's DaVinci Intermediate value
on the chart's flat greys, where key and pixel coincide, and writes 65 knots
per slider stop.

    <python with numpy, cv2, scipy> tools/fit_resolve_tone.py shadows|highlights [~/Desktop/"Davinci Test"]

Reads Originals/chart-all.tif, "No edits/chart-all plain.tif" and the
control's folder (Shadows or Hilights, files `chart-all (<control> <value>)`,
spelling as typed), plus the installed captured transforms. Writes
src-tauri/src/color_engine/resolve_<control>_table.rs.
"""
import glob, os, sys
import numpy as np, cv2
from scipy.interpolate import UnivariateSpline

CONTROL = sys.argv[1] if len(sys.argv) > 1 else "shadows"
assert CONTROL in ("shadows", "highlights"), "shadows or highlights"
FOLDER = {"shadows": "Shadows", "highlights": "Hilights"}[CONTROL]
ROOT = sys.argv[2] if len(sys.argv) > 2 else os.path.expanduser("~/Desktop/Davinci Test")
SUP = os.path.expanduser("~/Library/Application Support/io.github.CyberTimon.RapidRAW")
OUT = os.path.join(os.path.dirname(__file__), "..",
                   "src-tauri/src/color_engine/resolve_%s_table.rs" % CONTROL)
KNOTS = 65
STOPS = [-100, -50, 50, 100]


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


def tetra(cube, rgb):
    n = cube.shape[0]; p = np.clip(rgb, 0, 1) * (n - 1)
    i = np.minimum(np.floor(p).astype(int), n - 2); f = p - i
    r, g, b = i[..., 0], i[..., 1], i[..., 2]; fr, fg, fb = f[..., 0], f[..., 1], f[..., 2]
    C = lambda dr, dg, db: cube[b + db, g + dg, r + dr]
    c000, c111 = C(0, 0, 0), C(1, 1, 1); out = np.empty_like(c000)
    cases = [((fr >= fg) & (fg >= fb), C(1, 0, 0), C(1, 1, 0), fr, fg, fb),
             ((fr >= fb) & (fb > fg), C(1, 0, 0), C(1, 0, 1), fr, fb, fg),
             ((fb > fr) & (fr >= fg), C(0, 0, 1), C(1, 0, 1), fb, fr, fg),
             ((fg > fr) & (fr >= fb), C(0, 1, 0), C(1, 1, 0), fg, fr, fb),
             ((fg >= fb) & (fb > fr), C(0, 1, 0), C(0, 1, 1), fg, fb, fr),
             ((fb > fg) & (fg > fr), C(0, 0, 1), C(0, 1, 1), fb, fg, fr)]
    for m, a, bb, fa, fb_, fc in cases:
        out[m] = (c000[m] * (1 - fa[m])[..., None] + a[m] * (fa[m] - fb_[m])[..., None]
                  + bb[m] * (fb_[m] - fc[m])[..., None] + c111[m] * fc[m][..., None])
    return out


def di_dec(v):
    return np.where(v <= 0.02740668, v / 10.44426855, 2.0 ** (v / 0.07329248 - 7.0) - 0.0075)


def load16(p):
    im = cv2.imread(p, cv2.IMREAD_UNCHANGED)
    if im is None:
        raise SystemExit("cannot read " + p)
    scale = 255.0 if im.dtype == np.uint8 else 65535.0
    return im[..., :3][..., ::-1].astype(np.float64) / scale


def export(folder, value):
    """The chart export at this slider value: the value is the last word of
    the name, the control's spelling before it is whatever was typed."""
    def stem(p):
        return os.path.splitext(os.path.basename(p))[0].rstrip(")").strip()
    c = [p for p in glob.glob(os.path.join(ROOT, folder, "*.tif"))
         if stem(p).startswith("chart-all ") and stem(p).split()[-1] == value]
    assert len(c) == 1, (folder, value, c)
    return load16(c[0])


IDT = read_cube(os.path.join(SUP, "input-transform.cube"))
ODT = read_cube(os.path.join(SUP, "output-transform.cube"))
src = load16(glob.glob(os.path.join(ROOT, "Originals", "chart-all.*"))[0])
neutral = export("No edits", "plain")
h, w = neutral.shape[:2]
src = cv2.resize(src, (w, h), interpolation=cv2.INTER_AREA)
log_in = tetra(IDT, src)
key = log_in.mean(-1)

# Flat greys only: no resampled edges, no texture, so key == pixel.
grey = (src.max(-1) - src.min(-1)) < 0.002
mean = src.mean(-1)
flat = np.abs(mean - cv2.GaussianBlur(mean, (0, 0), 1.5)) < 0.002
mask = cv2.erode((grey & flat).astype(np.uint8), np.ones((9, 9), np.uint8)).astype(bool)

# The output transform along grey, to read Intermediate values back off an export.
t = np.linspace(0, 1, 16385)
curve = tetra(ODT, np.stack([t, t, t], -1)).mean(-1)
inv = lambda v: np.interp(v, curve, t)

k = key[mask]
lin_in = np.maximum(di_dec(k), 1e-6)
knots = np.linspace(0, 1, KNOTS)
table = {}
for s in STOPS:
    r = export(FOLDER, "%d" % s)
    shown = r.mean(-1)[mask]
    out_log = inv(shown)
    stops = np.log2(np.maximum(di_dec(out_log), 1e-6) / lin_in)
    # Outputs on the floor or at white say nothing about the gain, nor does
    # the output transform's toe (keys below 0.08 for Highlights, whose
    # effect there is within the toe's resolution).
    keep = (out_log > 0.02) & (shown < 0.995) & (shown > 0.006)
    if CONTROL == "highlights":
        keep &= (k > 0.08) & (shown < 0.985)
    bins = np.linspace(0, 0.95, 96)
    idx = np.clip(np.digitize(k[keep], bins) - 1, 0, 94)
    n = np.bincount(idx, minlength=95)
    total = np.bincount(idx, stops[keep], 95)
    good = n >= 40
    x, y, wgt = bins[:-1][good] + 0.005, total[good] / n[good], np.sqrt(n[good])
    spline = UnivariateSpline(x, y, w=wgt / wgt.max(), k=3, s=len(x) * 4e-4)
    f = spline(knots)
    lo, hi = x.min(), x.max()
    if CONTROL == "shadows":
        # Below the darkest measured key the gain holds: the step chart's
        # near-black steps lift by the same stops as the darkest flat greys.
        f[knots < lo] = spline(lo)
        # Above the brightest measured key the curve carries on at its last
        # slope toward zero rather than lifting whites for ever (unmeasured:
        # the chart has no scene values above display white).
        slope = (spline(hi) - spline(hi - 0.1)) / 0.1
        tail = knots > hi
        f[tail] = spline(hi) + slope * (knots[tail] - hi)
    else:
        # Highlights leaves black alone: the gain runs to zero at key 0.
        f[knots < lo] = spline(lo) * knots[knots < lo] / lo
        tail = knots > hi
        if s > 0:
            # A positive lift saturates the export to white above this key,
            # so nothing more can be measured; the gain holds.
            f[tail] = spline(hi)
        else:
            # Pulling highlights continues into scene values above display
            # white, the slider's reason to exist on RAW: carry the last slope.
            slope = (spline(hi) - spline(hi - 0.1)) / 0.1
            f[tail] = spline(hi) + slope * (knots[tail] - hi)
    f = np.maximum(f, 0) if s > 0 else np.minimum(f, 0)
    table[s] = f
    resid = stops[keep] - spline(k[keep])
    print("%+4d: keys %.3f..%.3f, %d bins, residual %.3f stops; stops at 0/0.2/0.4/0.6/0.8/1.0: %s"
          % (s, lo, hi, good.sum(), resid.std(),
             " ".join("%+.2f" % v for v in np.interp([0, .2, .4, .6, .8, 1.0], knots, f))))

lines = ["//! Resolve's Photo-page %s slider, measured. Generated by" % CONTROL.capitalize(),
         "//! tools/fit_resolve_tone.py from the chart exports; do not edit.",
         "//!",
         "//! Gain in stops applied to all channels in linear light, indexed by the",
         "//! blurred key's DaVinci Intermediate value (65 knots over 0..1), at",
         "//! slider -100, -50, +50 and +100. Zero at slider 0; linear between stops.",
         "",
         "pub const KNOTS: usize = %d;" % KNOTS,
         "pub const STOPS: [f32; 4] = [-100., -50., 50., 100.];",
         "/// Per knot: the gain at slider -100, -50, +50, +100.",
         "pub const GAIN_STOPS: [[f32; 4]; KNOTS] = ["]
for i in range(KNOTS):
    lines.append("    [%s]," % ", ".join("%.5f" % table[s][i] for s in STOPS))
lines.append("];")
open(OUT, "w").write("\n".join(lines) + "\n")
print("wrote", os.path.normpath(OUT))
