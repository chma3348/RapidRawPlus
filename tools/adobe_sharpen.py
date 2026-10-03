#!/usr/bin/env python3
"""Measure what Lightroom's Sharpening (Amount, Radius, Detail, Masking) and
noise reduction (Color, Luminance and their Detail/Smoothness/Contrast) do,
and what RapidRAW's do, at full resolution: sharpening works on single
pixels, so it cannot be judged on a shrunk picture.

As with the other sliders, only *changes* are measured: each edited picture
against the same engine's unedited one. Lightroom's exports are compared at
their own size, so ours are rendered at full size too.

Per photo and setting, on a central crop, in CIE L*a*b*:

- lightness gain by scale: difference-of-Gaussian bands (sigma 0.5 to 8
  px); per band the regression slope of edited on unedited (1 = unchanged);
- the same, split by how strong the local edges are (thirds of the unedited
  picture's gradient), which is what Masking and Detail change: Masking
  spares flat areas, low Detail damps the strong edges' halos;
- the edge overshoot: around strong edges, how far the edited picture goes
  beyond the unedited one's range (the halo), in L*;
- colour gain by scale: the same bands of a* and b* (for Color NR);
- tone: the median change in L* (sharpening should not shift it).

    uv run --with numpy --with scipy --with tifffile --with imagecodecs \\
        tools/adobe_sharpen.py measure ADOBE2_DIR NO_EDITS_DIR [OURS_DIR] OUT.json
    ... tools/adobe_sharpen.py report OUT.json
"""
import json
import os
import re
import sys
from concurrent.futures import ProcessPoolExecutor

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import adobe_detail as D  # noqa: E402

SIGMAS = [0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.5, 6.0, 8.0]
CROP = 2400
# The settings that make a folder; anything else non-default skips the file.
KEYS = {
    "Sharpness": 0, "SharpenRadius": 1.0, "SharpenDetail": 25, "SharpenEdgeMasking": 0,
    "LuminanceSmoothing": 0, "LuminanceNoiseReductionDetail": 50, "LuminanceNoiseReductionContrast": 0,
    "ColorNoiseReduction": 0, "ColorNoiseReductionDetail": 50, "ColorNoiseReductionSmoothness": 50,
}
OTHERS = ["Exposure2012", "Contrast2012", "Highlights2012", "Shadows2012", "Whites2012", "Blacks2012",
          "Texture", "Clarity2012", "Dehaze", "Vibrance", "Saturation", "GrainAmount"]


def settings(path):
    head = open(path, "rb").read(1_000_000).decode("latin-1")
    v = {k: float(x) for k, x in re.findall(r'crs:(\w+)="([+-]?[\d.]+)"', head)}
    return {k: v.get(k, d) for k, d in KEYS.items()}, [k for k in OTHERS if abs(v.get(k, 0)) > 1e-9]


def label(s):
    """A setting's name, e.g. 'sharp40 r1.0 d25 m0 | cnr0'."""
    parts = []
    if s["Sharpness"]:
        parts.append(f"sharp{s['Sharpness']:g} r{s['SharpenRadius']:g} d{s['SharpenDetail']:g} m{s['SharpenEdgeMasking']:g}")
    if s["LuminanceSmoothing"]:
        parts.append(f"lnr{s['LuminanceSmoothing']:g} d{s['LuminanceNoiseReductionDetail']:g} c{s['LuminanceNoiseReductionContrast']:g}")
    if s["ColorNoiseReduction"]:
        parts.append(f"cnr{s['ColorNoiseReduction']:g} d{s['ColorNoiseReductionDetail']:g} s{s['ColorNoiseReductionSmoothness']:g}")
    return " | ".join(parts) or "none"


def crop(a):
    h, w = a.shape[:2]
    c = min(CROP, h, w)
    y, x = (h - c) // 2, (w - c) // 2
    return a[y:y + c, x:x + c]


def adobe_full(path):
    import tifffile
    raw = crop(tifffile.imread(path)[..., :3]).astype(np.float32) / 65535
    return D.lab((raw ** (563 / 256)) @ D.ADOBE_RGB_TO_XYZ.T)


def ours_full(path):
    return crop(D.ours(path))


def bands(x, sig):
    from scipy.ndimage import gaussian_filter
    blurred = [x] + [gaussian_filter(x, s, mode="reflect") for s in sig]
    return [blurred[i] - blurred[i + 1] for i in range(len(sig))]


def slope(b0, b1, m=None):
    if m is not None:
        b0, b1 = b0[m], b1[m]
    den = float((b0 * b0).sum())
    return float((b0 * b1).sum() / den) if den > 1e-6 else None


def measure_pair(base, edit):
    from scipy.ndimage import gaussian_filter, maximum_filter, minimum_filter, sobel
    L0, L1 = base[..., 0].astype(np.float64), edit[..., 0].astype(np.float64)
    # Edge strength: gradient of the lightly smoothed unedited picture.
    s = gaussian_filter(L0, 1.0)
    grad = np.hypot(sobel(s, 0), sobel(s, 1)) / 8
    q = np.quantile(grad, [1 / 3, 2 / 3])
    groups = [grad < q[0], (grad >= q[0]) & (grad < q[1]), grad >= q[1]]
    b0, b1 = bands(L0, SIGMAS), bands(L1, SIGMAS)
    gain = [slope(x, y) for x, y in zip(b0, b1)]
    by_edge = [[slope(x, y, g) for x, y in zip(b0, b1)] for g in groups]
    # Halo: around the strongest 2% of edges, how far the edited picture
    # leaves the unedited one's local range (5x5), in L*.
    strong = grad >= np.quantile(grad, 0.98)
    hi, lo = maximum_filter(L0, 5), minimum_filter(L0, 5)
    over = np.maximum(L1 - hi, 0) + np.maximum(lo - L1, 0)
    halo = float(over[strong].mean())
    flat_noise = float((L1 - L0)[groups[0]].std())
    colour = []
    for c in (1, 2):
        a0, a1 = bands(base[..., c].astype(np.float64), SIGMAS), bands(edit[..., c].astype(np.float64), SIGMAS)
        colour.append([slope(x, y) for x, y in zip(a0, a1)])
    colour_gain = [None if a is None or b is None else (a + b) / 2 for a, b in zip(*colour)]
    # The finest bands' gain by tone (L* bins), where shadow noise shows.
    tbins = [0, 12, 25, 45, 70, 101]
    fine = lambda m: [slope(x, y, m) for x, y in zip(b0[:3], b1[:3])]
    by_tone = []
    for lo, hi in zip(tbins[:-1], tbins[1:]):
        m = (L0 >= lo) & (L0 < hi)
        g = fine(m) if m.sum() > 2000 else [None] * 3
        by_tone.append(None if None in g else float(np.mean(g)))
    return {"gain": gain, "by_edge": by_edge, "by_tone": by_tone, "halo": halo, "flat_change": flat_noise,
            "colour_gain": colour_gain, "tone": float(np.median(L1 - L0))}


def job(args):
    adobe2, no_edits, ours_dir, folder, photo, lab_ = args
    out = {}
    a0 = adobe_full(f"{no_edits}/{photo}.tif")
    out["adobe"] = measure_pair(a0, adobe_full(f"{adobe2}/{folder}/{photo}.tif"))
    if ours_dir:
        tag = folder.replace(" ", "_")
        o0p, o1p = f"{ours_dir}/{photo}__neutral_0.f32", f"{ours_dir}/{photo}__{tag}.f32"
        if os.path.exists(o0p) and os.path.exists(o1p):
            out["ours"] = measure_pair(ours_full(o0p), ours_full(o1p))
    return lab_, folder, photo, out


def measure(adobe2, no_edits, ours_dir, path):
    jobs = []
    for folder in sorted(os.listdir(adobe2)):
        if not re.search(r"sharp|noise|nr", folder):
            continue
        for f in sorted(os.listdir(os.path.join(adobe2, folder))):
            if not f.endswith(".tif"):
                continue
            s, others = settings(os.path.join(adobe2, folder, f))
            if others:
                print(f"skip {folder}/{f}: {others}")
                continue
            jobs.append((adobe2, no_edits, ours_dir, folder, f[:-4], label(s)))
    # A folder is one setting: files whose settings disagree with the
    # folder's majority were exported wrongly.
    from collections import Counter
    major = {f: Counter(j[5] for j in jobs if j[3] == f).most_common(1)[0][0] for f in {j[3] for j in jobs}}
    for j in [j for j in jobs if j[5] != major[j[3]]]:
        print(f"skip {j[3]}/{j[4]}: {j[5]} (folder is {major[j[3]]})")
    jobs = [j for j in jobs if j[5] == major[j[3]]]
    result = {"sigmas": SIGMAS, "settings": {}}
    with ProcessPoolExecutor(6) as ex:
        for lab_, folder, photo, m in ex.map(job, jobs):
            entry = result["settings"].setdefault(folder, {"label": lab_, "photos": {}})
            entry["photos"][photo] = m
    json.dump(result, open(path, "w"))
    print("wrote", path)


def pooled(entry, engine, key):
    rows = [p[engine][key] for p in entry["photos"].values() if engine in p]
    if not rows:
        return None
    return np.nanmedian(np.array(rows, dtype=float), axis=0)


def report(path):
    r = json.load(open(path))
    print("lightness gain by scale (sigma px): " + " ".join(f"{s:>5g}" for s in r["sigmas"]))
    for folder, e in sorted(r["settings"].items()):
        print(f"\n{folder}  [{e['label']}]  ({len(e['photos'])} photos)")
        for eng in ("adobe", "ours"):
            g = pooled(e, eng, "gain")
            if g is None:
                continue
            be = pooled(e, eng, "by_edge")
            cg = pooled(e, eng, "colour_gain")
            print(f"  {eng:5} all    " + " ".join(f"{x:5.2f}" for x in g))
            for name, row in zip(("flat", "mid", "edges"), be):
                print(f"  {eng:5} {name:6} " + " ".join(f"{x:5.2f}" for x in row))
            print(f"  {eng:5} colour " + " ".join(f"{x:5.2f}" for x in cg))
            bt = pooled(e, eng, "by_tone")
            print(f"  {eng:5} fine gain at L* <12/12-25/25-45/45-70/>70: " + " ".join(f"{x:5.2f}" for x in bt))
            print(f"  {eng:5} halo {pooled(e, eng, 'halo'):.2f} L*   flat change {pooled(e, eng, 'flat_change'):.2f}   tone {pooled(e, eng, 'tone'):+.2f}")


if __name__ == "__main__":
    if len(sys.argv) >= 5 and sys.argv[1] == "measure":
        a = sys.argv[2:]
        measure(a[0], a[1], a[2] if len(a) == 4 else None, a[-1])
    elif len(sys.argv) >= 3 and sys.argv[1] == "report":
        report(sys.argv[2])
    else:
        print(__doc__)
