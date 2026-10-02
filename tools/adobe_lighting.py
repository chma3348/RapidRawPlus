#!/usr/bin/env python3
"""Measure what the lighting sliders do, in Lightroom and in RapidRAW, so ours
can be given Lightroom's strength while keeping RapidRAW's (Resolve's) colour.

Only *changes* are measured: every edited picture is compared with the same
engine's unedited picture of the same photo, never with the other engine's.
So Lightroom's base look (its Adobe Color profile, its contrast) is left out,
and neither engine's colour enters the numbers: just how far each tone moves.

A tone's movement is its change in CIE lightness (L*, 0-100), grouped by
where it sits in the unedited picture. Two groupings are kept: by the pixel's
own lightness, and by its region's (a box mean over 2% of the short edge,
like the regional key RapidRAW's tone zones use), which shows how local a
slider is.

    uv run --with numpy --with scipy --with tifffile --with imagecodecs \\
        tools/adobe_lighting.py measure ADOBE_DIR OURS_DIR OUT.json
    ... tools/adobe_lighting.py report OUT.json

ADOBE_DIR holds one folder per Lightroom export set (each file's own XMP says
which slider and value it is; the folder names are not trusted); OURS_DIR is
examples/adobe_sweep's output.
"""
import json
import math
import os
import re
import sys
from concurrent.futures import ProcessPoolExecutor

import numpy as np

MAX = 1752
BINS = np.linspace(0, 100, 51)
CENTRES = (BINS[:-1] + BINS[1:]) / 2
CONTROLS = {
    "Exposure2012": "exposure",
    "Contrast2012": "contrast",
    "Highlights2012": "highlights",
    "Shadows2012": "shadows",
    "Whites2012": "whites",
    "Blacks2012": "blacks",
}
RAW = {"DSC03453", "DSC03458", "DSC03532", "DSC03545", "DSC07955", "_AAF8641", "_AAF8720"}

ADOBE_RGB_TO_XYZ = np.array([[0.5767309, 0.1855540, 0.1881852],
                             [0.2973769, 0.6273491, 0.0752741],
                             [0.0270343, 0.0706872, 0.9911085]])
SRGB_TO_XYZ = np.array([[0.4124564, 0.3575761, 0.1804375],
                        [0.2126729, 0.7151522, 0.0721750],
                        [0.0193339, 0.1191920, 0.9503041]])


def lightness(Y):
    """CIE L* of relative luminance (display white = 1)."""
    Y = np.clip(Y, 0, None)
    f = np.where(Y > (6 / 29) ** 3, np.cbrt(Y), Y / (3 * (6 / 29) ** 2) + 4 / 29)
    return 116 * f - 16


def shrink(img, factor):
    if factor == 1:
        return img
    h, w = (img.shape[0] // factor) * factor, (img.shape[1] // factor) * factor
    return img[:h, :w].reshape(h // factor, factor, w // factor, factor, -1).mean(axis=(1, 3))


def adobe_lightness(path):
    """L* of a Lightroom export (16-bit Adobe RGB TIFF), at most MAX pixels on its long side."""
    import tifffile
    raw = tifffile.imread(path)[..., :3]
    factor = math.ceil(max(raw.shape[:2]) / MAX)
    linear = (raw.astype(np.float32) / 65535.0) ** (563 / 256)
    linear = shrink(linear, factor)
    return lightness(linear @ ADOBE_RGB_TO_XYZ[1])


def ours_lightness(path):
    meta = json.load(open(path[:-4] + ".json"))
    enc = np.fromfile(path, dtype="<f4").reshape(meta["height"], meta["width"], 3)
    assert meta["space"] == "Srgb", meta["space"]
    linear = np.where(enc <= 0.04045, enc / 12.92, ((enc + 0.055) / 1.055) ** 2.4)
    return lightness(linear @ SRGB_TO_XYZ[1])


def regional(L):
    from scipy.ndimage import uniform_filter
    size = max(3, int(round(0.04 * min(L.shape))) | 1)
    return uniform_filter(L, size=size, mode="reflect")


def changes(base, edited):
    """How far tones moved, grouped by the pixel's own and its region's unedited lightness."""
    if edited.shape != base.shape:
        h, w = min(base.shape[0], edited.shape[0]), min(base.shape[1], edited.shape[1])
        base, edited = base[:h, :w], edited[:h, :w]
    d = edited - base
    out = {}
    for name, key in (("pixel", base), ("region", regional(base))):
        idx = np.clip(np.digitize(key.ravel(), BINS) - 1, 0, len(CENTRES) - 1)
        med, n = [], np.bincount(idx, minlength=len(CENTRES))
        dv = d.ravel()
        order = np.argsort(idx, kind="stable")
        splits = np.split(dv[order], np.cumsum(n)[:-1])
        for part in splits:
            med.append(float(np.median(part)) if len(part) >= 50 else None)
        out[name] = {"median": med, "count": n.tolist()}
    # How much of the change a pixel's own lightness explains versus its region's.
    out["spread_pixel"] = float(np.std(d - np.interp(base, CENTRES, [m or 0 for m in out["pixel"]["median"]])))
    out["spread_region"] = float(np.std(d - np.interp(regional(base), CENTRES, [m or 0 for m in out["region"]["median"]])))
    return out


def adobe_setting(path):
    head = open(path, "rb").read(1_000_000).decode("latin-1")
    found = {k: float(v) for k, v in re.findall(r'crs:(\w+)="([+-]?[\d.]+)"', head) if k in CONTROLS}
    moved = [(CONTROLS[k], v) for k, v in found.items() if abs(v) > 1e-9]
    return moved[0] if len(moved) == 1 else ("neutral", 0.0) if not moved else None


def measure_adobe(adobe_dir, photo):
    files = []
    for folder in sorted(os.listdir(adobe_dir)):
        p = os.path.join(adobe_dir, folder, photo + ".tif")
        if os.path.exists(p):
            s = adobe_setting(p)
            if s:
                files.append((s, p))
    base = next((p for s, p in files if s[0] == "neutral"), None)
    if not base:
        return photo, {}
    L0 = adobe_lightness(base)
    out = {}
    for (control, value), p in files:
        if control == "neutral":
            continue
        out.setdefault(control, {})[f"{value:g}"] = changes(L0, adobe_lightness(p))
    return photo, out


def measure_ours(ours_dir, photo):
    names = [f for f in os.listdir(ours_dir) if f.startswith(photo + "__") and f.endswith(".f32")]
    base = os.path.join(ours_dir, f"{photo}__neutral_0.f32")
    if not os.path.exists(base):
        return photo, {}
    L0 = ours_lightness(base)
    out = {}
    for f in sorted(names):
        control, value = f[len(photo) + 2:-4].rsplit("_", 1)
        if control == "neutral":
            continue
        # Where the engine found the photo's tones, as it adapts to them.
        tones = json.load(open(os.path.join(ours_dir, f[:-4] + ".json"))).get("tones")
        if tones and "_tones" not in out:
            out["_tones"] = tones
        out.setdefault(control, {})[f"{float(value):g}"] = changes(L0, ours_lightness(os.path.join(ours_dir, f)))
    return photo, out


def photos_in(originals):
    return sorted(os.path.splitext(f)[0] for f in os.listdir(originals) if not f.startswith("."))


def measure(adobe_dir, ours_dir, out_path, originals):
    photos = photos_in(originals)
    result = {"bins": CENTRES.tolist(), "raw": sorted(RAW), "adobe": {}, "ours": {}}
    with ProcessPoolExecutor(max_workers=4) as pool:
        for photo, data in pool.map(measure_adobe, [adobe_dir] * len(photos), photos):
            result["adobe"][photo] = data
            print("adobe", photo, {c: sorted(v, key=float) for c, v in data.items()}, flush=True)
        for photo, data in pool.map(measure_ours, [ours_dir] * len(photos), photos):
            result["ours"][photo] = data
            print("ours ", photo, len(data), "controls", flush=True)
    json.dump(result, open(out_path, "w"))
    print("wrote", out_path)


# ---------------------------------------------------------------------------
# Pooling and reporting
# ---------------------------------------------------------------------------

def pooled(result, engine, control, value, key="region", photos=None):
    """The typical change per tone across photos: the median of the photos' medians."""
    rows = []
    for photo, data in result[engine].items():
        if photos is not None and photo not in photos:
            continue
        m = data.get(control, {}).get(f"{value:g}")
        if m:
            rows.append([np.nan if x is None else x for x in m[key]["median"]])
    if not rows:
        return None
    return np.nanmedian(np.array(rows), axis=0)


def report(path):
    r = json.load(open(path))
    bins = np.array(r["bins"])
    show = [5, 15, 25, 35, 45, 55, 65, 75, 85, 95]
    cols = [int(np.argmin(abs(bins - s))) for s in show]
    for control in ["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]:
        values = sorted({float(v) for d in r["adobe"].values() for v in d.get(control, {})})
        if not values:
            continue
        print(f"\n== {control}: change in L* by regional lightness " + " ".join(f"{s:>5}" for s in show))
        for v in values:
            a = pooled(r, "adobe", control, v)
            ours_v = v * 0.8 if control == "exposure" else v
            o = pooled(r, "ours", control, ours_v)
            print(f"  Lightroom {v:+7g}           " + " ".join("   --" if np.isnan(a[c]) else f"{a[c]:+5.1f}" for c in cols))
            if o is not None:
                print(f"  ours      {ours_v:+7g}           " + " ".join("   --" if np.isnan(o[c]) else f"{o[c]:+5.1f}" for c in cols))


def weights(result, engine, key="region", photos=None):
    """Pixels per tone, pooled over photos: where the error matters most."""
    total = np.zeros(len(result["bins"]))
    for photo, data in result[engine].items():
        if photos is not None and photo not in photos:
            continue
        for name, control in data.items():
            if name.startswith("_"):
                continue
            for m in control.values():
                total += np.array(m[key]["count"], dtype=float)
                break
            break
    return total / max(total.sum(), 1)


def best_match(result, control, adobe_value, photos=None):
    """Our slider value whose change matches Lightroom's at `adobe_value` best,
    and how close it gets (weighted RMS, L* units)."""
    a = pooled(result, "adobe", control, adobe_value, photos=photos)
    w = weights(result, "adobe", photos=photos)
    ours = sorted({float(v) for d in result["ours"].values() for v in d.get(control, {})})
    curves = {u: pooled(result, "ours", control, u, photos=photos) for u in ours}
    curves[0.0] = np.zeros_like(a)
    grid = sorted(curves)
    best = (None, float("inf"))
    for u in np.linspace(grid[0], grid[-1], 401):
        i = max(0, min(len(grid) - 2, int(np.searchsorted(grid, u)) - 1))
        lo, hi = grid[i], grid[i + 1]
        t = (u - lo) / (hi - lo)
        o = (1 - t) * curves[lo] + t * curves[hi]
        ok = ~np.isnan(a) & ~np.isnan(o)
        err = math.sqrt(np.sum(w[ok] * (a[ok] - o[ok]) ** 2) / max(np.sum(w[ok]), 1e-9))
        if err < best[1]:
            best = (float(u), err)
    return best


def fit(path):
    r = json.load(open(path))
    raw = set(r["raw"])
    groups = {"all": None, "display": {p for p in r["adobe"] if p not in raw}, "raw": raw}
    for control in ["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]:
        values = sorted({float(v) for d in r["adobe"].values() for v in d.get(control, {})})
        for v in values:
            same = v * 0.8 if control == "exposure" else v
            parts = []
            for name, photos in groups.items():
                u, err = best_match(r, control, v, photos)
                a = pooled(r, "adobe", control, v, photos=photos)
                o = pooled(r, "ours", control, same, photos=photos)
                w = weights(r, "adobe", photos=photos)
                ok = ~np.isnan(a) & ~np.isnan(o)
                now = math.sqrt(np.sum(w[ok] * (a[ok] - o[ok]) ** 2) / np.sum(w[ok]))
                parts.append(f"{name}: same value off by {now:4.1f}, best ours {u:+7.1f} (off {err:4.1f})")
            print(f"{control:10} LR {v:+6g} | " + " | ".join(parts))


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "measure" and len(sys.argv) == 5:
        originals = os.environ.get("ORIGINALS", os.path.expanduser("~/Desktop/Davinci Test/Originals"))
        measure(sys.argv[2], sys.argv[3], sys.argv[4], originals)
    elif cmd == "report" and len(sys.argv) == 3:
        report(sys.argv[2])
    elif cmd == "measure-ours" and len(sys.argv) == 5:
        # Our renders only, alongside Lightroom's measurements from an earlier run.
        originals = os.environ.get("ORIGINALS", os.path.expanduser("~/Desktop/Davinci Test/Originals"))
        result = json.load(open(sys.argv[3]))
        result["ours"] = {}
        photos = photos_in(originals)
        with ProcessPoolExecutor(max_workers=6) as pool:
            for photo, data in pool.map(measure_ours, [sys.argv[2]] * len(photos), photos):
                result["ours"][photo] = data
        json.dump(result, open(sys.argv[4], "w"))
        print("wrote", sys.argv[4])
    elif cmd == "fit" and len(sys.argv) == 3:
        fit(sys.argv[2])
    else:
        print(__doc__)
