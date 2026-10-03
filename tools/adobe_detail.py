#!/usr/bin/env python3
"""Measure what Lightroom's detail and effect sliders (Clarity, Texture,
Dehaze, Grain) do, and what RapidRAW's do, so ours can be fitted to them.

As with the lighting sliders (adobe_lighting.py), only *changes* are
measured: every edited picture against the same engine's own unedited
picture of the same photo. Lightroom's exports are resized onto RapidRAW's
render by area.

For each photo and setting, in CIE L*a*b*:

- detail gain by scale: the picture's lightness split into bands (difference
  of Gaussians, sigma doubling from about one pixel at full size); for each
  band, how much of the unedited picture's detail the edited one has (the
  regression slope of edited band on unedited band). 1 = unchanged, above
  1 = boosted, below = softened. This is what tells Clarity from Texture.
- tone: the change in L* by unedited L* (median per bin);
- colour: the edited chroma over the unedited, by unedited L*;
- added noise (for Grain): the standard deviation of the edited picture's
  fine detail that is not in the unedited picture, by L*, in lightness and in
  colour, and its typical size.

    uv run --with numpy --with scipy --with tifffile --with imagecodecs --with pillow \\
        tools/adobe_detail.py measure ADOBE2_DIR NO_EDITS_DIR OURS_DIR OUT.json
    ... tools/adobe_detail.py report OUT.json
"""
import json
import math
import os
import re
import sys
from concurrent.futures import ProcessPoolExecutor

import numpy as np

CONTROLS = {"Clarity2012": "clarity", "Texture": "texture", "Dehaze": "dehaze", "GrainAmount": "grain"}
ADOBE_RGB_TO_XYZ = np.array([[0.5767309, 0.1855540, 0.1881852],
                             [0.2973769, 0.6273491, 0.0752741],
                             [0.0270343, 0.0706872, 0.9911085]])
SRGB_TO_XYZ = np.array([[0.4124564, 0.3575761, 0.1804375],
                        [0.2126729, 0.7151522, 0.0721750],
                        [0.0193339, 0.1191920, 0.9503041]])
D65 = np.array([0.95047, 1.0, 1.08883])
BINS = np.linspace(0, 100, 21)
# Band sigmas in full-resolution pixels; scaled to the render's size.
FULL_SIGMAS = [0.75, 1.5, 3, 6, 12, 24, 48, 96, 192]


def lab(xyz):
    t = xyz / D65
    f = np.where(t > (6 / 29) ** 3, np.cbrt(np.clip(t, 0, None)), t / (3 * (6 / 29) ** 2) + 4 / 29)
    L = 116 * f[..., 1] - 16
    return np.stack([L, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], -1)


def adobe(path, shape):
    import tifffile
    from PIL import Image
    raw = tifffile.imread(path)[..., :3].astype(np.float32) / 65535
    xyz = (raw ** (563 / 256)) @ ADOBE_RGB_TO_XYZ.T
    return lab(np.stack([np.array(Image.fromarray(xyz[..., c].astype(np.float32)).resize((shape[1], shape[0]), Image.BOX))
                         for c in range(3)], -1))


def ours(path):
    meta = json.load(open(path[:-4] + ".json"))
    enc = np.fromfile(path, dtype="<f4").reshape(meta["height"], meta["width"], 3)
    lin = np.where(enc <= 0.04045, enc / 12.92, ((np.clip(enc, 0, None) + 0.055) / 1.055) ** 2.4)
    return lab(lin @ SRGB_TO_XYZ.T)


# Sliders that would change the picture if left set alongside the one measured.
OTHERS = ["Exposure2012", "Contrast2012", "Highlights2012", "Shadows2012", "Whites2012", "Blacks2012",
          "Vibrance", "Saturation", "Sharpness", "LuminanceSmoothing", "ColorNoiseReduction"]


# Sliders the base pictures share with the edited ones (ADOBE_DETAIL_SHARED,
# comma-separated, e.g. Sharpness when the base is Lightroom's Sharpening 40
# export), so they cancel in the comparison and are not a reason to skip.
SHARED = [k for k in os.environ.get("ADOBE_DETAIL_SHARED", "").split(",") if k]


def setting(path):
    head = open(path, "rb").read(1_000_000).decode("latin-1")
    values = {k: float(v) for k, v in re.findall(r'crs:(\w+)="([+-]?[\d.]+)"', head)}
    for k in SHARED:
        values.pop(k, None)
    found = [(CONTROLS[k], v) for k, v in values.items() if k in CONTROLS and abs(v) > 1e-9]
    others = [k for k in OTHERS + list(CONTROLS) if abs(values.get(k, 0.0)) > 1e-9 and k not in dict(
        (key, 1) for key in CONTROLS if any(CONTROLS[key] == f[0] for f in found))]
    return found, others


def bands(L, scale):
    from scipy.ndimage import gaussian_filter
    sig = [max(s * scale, 0.35) for s in FULL_SIGMAS]
    blurred = [L] + [gaussian_filter(L, s, mode="reflect") for s in sig]
    return [blurred[i] - blurred[i + 1] for i in range(len(sig))]


def measure_pair(base, edit, scale):
    """One engine, one photo, one setting: its change against its own base."""
    from scipy.ndimage import gaussian_filter
    L0, L1 = base[..., 0], edit[..., 0]
    gains = []
    for b0, b1 in zip(bands(L0, scale), bands(L1, scale)):
        den = float((b0 * b0).sum())
        gains.append(float((b0 * b1).sum() / den) if den > 1e-6 else None)
    idx = np.clip(np.digitize(L0, BINS) - 1, 0, len(BINS) - 2)
    c0 = np.hypot(base[..., 1], base[..., 2])
    c1 = np.hypot(edit[..., 1], edit[..., 2])
    # Added fine noise: what the edited picture has at fine scales that the
    # unedited one's fine detail does not account for.
    s = max(1.5 * scale, 0.5)
    fine = lambda x: x - gaussian_filter(x, s, mode="reflect")
    resid_L = fine(L1) - fine(L0)
    resid_ab = np.hypot(fine(edit[..., 1]) - fine(base[..., 1]), fine(edit[..., 2]) - fine(base[..., 2]))
    tone, chroma, noise, noise_ab = [], [], [], []
    for i in range(len(BINS) - 1):
        m = idx == i
        if m.sum() < 400:
            tone.append(None); chroma.append(None); noise.append(None); noise_ab.append(None)
            continue
        tone.append(float(np.median(L1[m] - L0[m])))
        chroma.append(float(c1[m].mean() / max(c0[m].mean(), 1e-6)))
        noise.append(float(resid_L[m].std()))
        noise_ab.append(float(resid_ab[m].mean()))
    # Grain size: where the added noise's spectrum peaks (cycles per pixel at
    # full resolution).
    h, w = resid_L.shape
    crop = resid_L[h // 4:h // 4 + 512, w // 4:w // 4 + 512]
    P = np.abs(np.fft.fftshift(np.fft.fft2(crop * np.outer(np.hanning(crop.shape[0]), np.hanning(crop.shape[1]))))) ** 2
    yy, xx = np.indices(P.shape)
    r = np.hypot(yy - P.shape[0] / 2, xx - P.shape[1] / 2) / P.shape[0]
    radial = [float(P[(r >= a) & (r < a + 0.025)].mean()) for a in np.arange(0.0, 0.5, 0.025)]
    return {"gain": gains, "tone": tone, "chroma": chroma, "noise": noise, "noise_ab": noise_ab, "spectrum": radial}


def job(args):
    adobe2, no_edits, ours_dir, photo, folder, control, value = args
    o0p = f"{ours_dir}/{photo}__neutral_0.f32"
    o1p = f"{ours_dir}/{photo}__{control}_{value:g}.f32"
    if not (os.path.exists(o0p) and os.path.exists(o1p)):
        return None
    o0, o1 = ours(o0p), ours(o1p)
    full_w = None
    import tifffile
    with tifffile.TiffFile(f"{no_edits}/{photo}.tif") as t:
        full_w = t.pages[0].shape[1]
    scale = o0.shape[1] / full_w
    a0 = adobe(f"{no_edits}/{photo}.tif", o0.shape)
    a1 = adobe(f"{adobe2}/{folder}/{photo}.tif", o0.shape)
    return photo, control, value, {"adobe": measure_pair(a0, a1, scale), "ours": measure_pair(o0, o1, scale), "scale": scale}


def measure(adobe2, no_edits, ours_dir, out):
    jobs = []
    for folder in sorted(os.listdir(adobe2)):
        d = os.path.join(adobe2, folder)
        if not os.path.isdir(d):
            continue
        for f in sorted(os.listdir(d)):
            if not f.endswith(".tif"):
                continue
            found, others = setting(os.path.join(d, f))
            if len(found) != 1 or others:
                print(f"skip {folder}/{f}: {found} {others}")
                continue
            control, value = found[0]
            jobs.append((adobe2, no_edits, ours_dir, f[:-4], folder, control, value))
    result = {"bins": BINS.tolist(), "sigmas": FULL_SIGMAS, "photos": {}}
    with ProcessPoolExecutor(6) as ex:
        for r in ex.map(job, jobs):
            if r:
                photo, control, value, m = r
                result["photos"].setdefault(photo, {}).setdefault(control, {})[f"{value:g}"] = m
    json.dump(result, open(out, "w"))
    print("wrote", out, sum(len(v) for p in result["photos"].values() for v in p.values()), "measurements")


def pooled(result, control, value, engine, key):
    rows = [p[control][f"{value:g}"][engine][key] for p in result["photos"].values()
            if control in p and f"{value:g}" in p[control]]
    if not rows:
        return None
    return np.nanmedian(np.array([[np.nan if x is None else x for x in r] for r in rows], dtype=float), axis=0)


def report(path):
    r = json.load(open(path))
    sig = r["sigmas"]
    keys = sorted({(c, float(v)) for p in r["photos"].values() for c in p for v in p[c]})
    print("detail gain by scale (sigma, full-size px): " + " ".join(f"{s:>6g}" for s in sig))
    for c, v in keys:
        for eng in ("adobe", "ours"):
            g = pooled(r, c, v, eng, "gain")
            print(f"{c:8} {v:+5g} {eng:5}  " + " ".join(f"{x:6.2f}" for x in g))
    print("\ntone change dL* at L* 10/30/50/70/90, chroma ratio at the same:")
    for c, v in keys:
        for eng in ("adobe", "ours"):
            t = pooled(r, c, v, eng, "tone")
            ch = pooled(r, c, v, eng, "chroma")
            pick = lambda a: " ".join(f"{a[i]:+5.1f}" if np.isfinite(a[i]) else "   - " for i in (2, 6, 10, 14, 18))
            pickc = lambda a: " ".join(f"{a[i]:5.2f}" if np.isfinite(a[i]) else "   - " for i in (2, 6, 10, 14, 18))
            print(f"{c:8} {v:+5g} {eng:5}  dL {pick(t)}   chroma {pickc(ch)}")
    print("\nadded fine noise (L* std) at L* 10/30/50/70/90, colour noise (ab), spectrum peak (cycles/px at render size):")
    for c, v in keys:
        if c != "grain":
            continue
        for eng in ("adobe", "ours"):
            n = pooled(r, c, v, eng, "noise")
            nab = pooled(r, c, v, eng, "noise_ab")
            spec = pooled(r, c, v, eng, "spectrum")
            peak = 0.025 * (int(np.nanargmax(spec[1:])) + 1) + 0.0125
            print(f"{c:8} {v:+5g} {eng:5}  L " + " ".join(f"{n[i]:4.2f}" for i in (2, 6, 10, 14, 18))
                  + "   ab " + " ".join(f"{nab[i]:4.2f}" for i in (2, 6, 10, 14, 18)) + f"   peak {peak:.3f}")


if __name__ == "__main__":
    if len(sys.argv) >= 6 and sys.argv[1] == "measure":
        measure(*sys.argv[2:6])
    elif len(sys.argv) >= 3 and sys.argv[1] == "report":
        report(sys.argv[2])
    else:
        print(__doc__)
