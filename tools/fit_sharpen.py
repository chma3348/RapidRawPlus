#!/usr/bin/env python3
"""Fit Darkroom Index's Sharpening (Amount, Radius, Detail, Masking) and Color
Noise Reduction to Lightroom's, and write their table
(src-tauri/src/color_engine/sharpen_table.rs).

The model is the detail stage's (detail.rs), mirrored here exactly:

Sharpening works on the log luminance. Three bands, the luminance less its
Gaussian blur at sigma SIGMAS x Radius, each with its own amount, every band
soft-limited (tanh) at `limit` stops so a strong edge's halo stays small
while fine texture is boosted in full. That one limit is what Lightroom's
Detail mostly moves (measured: Detail 75 boosts flat texture far more than
it does edges). Masking multiplies the result by an edge mask: a smoothstep
of the local gradient (log2 per full-resolution pixel) between two
thresholds that grow with Masking.

Each measured Lightroom setting is fitted on its own (band amounts and limit
at Sharpening 40 and 80 with Detail 25, and 40 with Detail 75; mask
thresholds at Masking 50), then the engine interpolates between them.

Color NR smooths chromaticity (colour with luminance divided out) with a
guided filter steered by luminance, mixed in by `mix`, plus a broader
Gaussian for the mottling Smoothness removes.

Amounts are found by simulation (our unedited full-resolution renders mapped
back to scene light through the output curve, as fit_detail.py does), then
corrected from real renders.

    uv run --with numpy --with scipy --with tifffile --with imagecodecs --with pillow \\
        tools/fit_sharpen.py fit LR.json BASE_DIR
    ... tools/fit_sharpen.py correct LR.json OURS.json BASE_DIR [DAMPING]
    ... tools/fit_sharpen.py write
"""
import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from scipy.ndimage import gaussian_filter, sobel, uniform_filter
from scipy.optimize import least_squares

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import adobe_detail as D  # noqa: E402
import adobe_sharpen as S  # noqa: E402
import fit_lighting as F  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE = os.path.join(HERE, "..", "src-tauri/src/color_engine/sharpen_table.rs")
TARGETS = os.path.join(HERE, "sharpen_targets.json")
SIGMAS = [0.5, 1.0, 2.0]
# The tone weighting's knots, on the tonal key (DaVinci Intermediate, as the
# tone zones judge tone): at the keys of L* 6, 18, 35, 57 and 85, the middles
# of the L* bands it is measured in (Lightroom sharpens the shadows, and a
# little the highlights, less than the midtones).
TONE_KNOTS = [round(float(F.key_of(np.array(L))), 4) for L in (6, 18, 35, 57, 85)]
TONE_SMOOTHING = 1.5
MID_GREY_LOG = -2.4739312
# The Lightroom folders each fitted setting comes from.
SETTINGS = {
    "s40": "adobe sharpening 40",
    "s80": "adobe sharpening 80",
    "d75": "adobe sharpening detail 75",
}
MASKING = "adobe sharpening masking 50"
COLOUR = "adobe colour nr 25"
FIT_CROP = 1600
COLOUR_PHOTOS = 8
COLOUR_CROP = 800
# Gains at sigma 0.5..8 px; the finest few matter most.
WEIGHTS = np.array([2.0, 2.0, 1.5, 1.2, 1.0, 0.8, 0.5, 0.3, 0.3])


def gauss(x, s):
    return gaussian_filter(x, s, mode="nearest", truncate=3.0)


def edge_strength(log):
    """Local gradient of the log luminance (log2 per pixel), as the engine
    computes it for Masking: Sobel of a sigma-1 blur."""
    b = gauss(log, 1.0)
    return np.hypot(sobel(b, 0, mode="nearest"), sobel(b, 1, mode="nearest")) / 8


def tone_weight(log, knots):
    return np.interp(F.encode_di(2.0 ** log), TONE_KNOTS, knots)


def sharpen(log, amounts, limit, radius=1.0, mask=None, tones=None):
    """The bands' sum, soft-limited as a whole at `limit` stops, weighted by
    tone (of the blurred luminance, so the weight itself is smooth)."""
    out = log.copy()
    total = np.zeros_like(log)
    for a, s in zip(amounts, SIGMAS):
        total += a * (log - gauss(log, s * radius))
    total = limit * np.tanh(total / limit)
    if tones is not None:
        total *= tone_weight(gauss(log, 2.0), tones)
    if mask is not None:
        lo, hi = mask
        e = edge_strength(log)
        t = np.clip((e - lo) / max(hi - lo, 1e-6), 0, 1)
        total *= t * t * (3 - 2 * t)
    return out + total


class Photo:
    """One unedited render, cropped, with what the measurement needs of it
    precomputed (its bands and edge groups never change)."""

    def __init__(self, path):
        lab = D.ours(path)
        h, w = lab.shape[:2]
        c = min(FIT_CROP, h, w)
        y, x = (h - c) // 2, (w - c) // 2
        self.lab = lab[y:y + c, x:x + c].astype(np.float64)
        L = self.lab[..., 0]
        self.log = np.log2(np.maximum(F.decode_di(F.key_of(L)), 1e-6))
        s = gaussian_filter(L, 1.0)
        grad = np.hypot(sobel(s, 0), sobel(s, 1)) / 8
        q = np.quantile(grad, [1 / 3, 2 / 3])
        self.groups = [grad < q[0], (grad >= q[0]) & (grad < q[1]), grad >= q[1]]
        self.b0 = S.bands(L, S.SIGMAS)
        tb = [0, 12, 25, 45, 70, 101]
        self.tones = [(L >= lo) & (L < hi) for lo, hi in zip(tb[:-1], tb[1:])]

    def gains(self, log1):
        L1 = F.L_of_key(F.encode_di(2.0 ** log1))
        b1 = S.bands(L1, S.SIGMAS)
        allg = [S.slope(x, y) for x, y in zip(self.b0, b1)]
        by = [[S.slope(x, y, g) for x, y in zip(self.b0, b1)] for g in self.groups]
        tone = [np.mean([S.slope(x, y, m) for x, y in zip(self.b0[:3], b1[:3])]) if m.sum() > 2000 else np.nan
                for m in self.tones]
        return np.concatenate([np.array([allg] + by, dtype=float).ravel(), tone])


def load_photos(base_dir, names):
    with ThreadPoolExecutor(8) as ex:
        return list(ex.map(lambda n: Photo(f"{base_dir}/{n}__neutral_0.f32"), names))


def target(lr, folder, names):
    e = lr["settings"][folder]["photos"]
    rows = [np.concatenate([np.array([e[n]["adobe"]["gain"]] + e[n]["adobe"]["by_edge"], dtype=float).ravel(),
                            np.array([np.nan if v is None else v for v in e[n]["adobe"]["by_tone"]], dtype=float)])
            for n in names if n in e]
    return np.nanmedian(np.array(rows, dtype=float), 0)


def simulate(photos, fn):
    with ThreadPoolExecutor(8) as ex:
        rows = list(ex.map(lambda p: p.gains(fn(p)), photos))
    return np.nanmedian(np.array(rows), 0)


# Bands (all, flat, mid, edges) by scale, then the five tones.
RESIDUAL_WEIGHTS = np.concatenate([np.tile(WEIGHTS, 4), np.full(5, 2.5)])


def residual(sim, want):
    return np.nan_to_num((sim - want) * RESIDUAL_WEIGHTS)


def common(lr, base_dir):
    names = sorted({n for f in SETTINGS.values() for n in lr["settings"][f]["photos"]}
                   & {f[:-len("__neutral_0.f32")] for f in os.listdir(base_dir) if f.endswith("__neutral_0.f32")})
    return names


def fit(lr_path, base_dir):
    lr = json.load(open(lr_path))
    names = common(lr, base_dir)
    photos = load_photos(base_dir, names)
    state = {"sigmas": SIGMAS, "sharpen": {}, "colour": {}}
    # Detail 25 at Sharpening 40 and 80 together: one kernel shape, scaled
    # by (amount / 40) ** power, and one limit.
    w40, w80 = target(lr, SETTINGS["s40"], names), target(lr, SETTINGS["s80"], names)

    nt = len(TONE_KNOTS)
    old = json.load(open(TARGETS)) if os.path.exists(TARGETS) else {}
    o = old.get("sharpen", {}).get("d25")
    x0 = (list(o["amounts"]) + [o["limit"], old["sharpen"]["power"]] if o else [0.4, 0.9, -0.1, 0.9, 1.3]) \
        + [0.4, 0.7, 1.1, 1.2, 1.0]

    def f25(p):
        a, lim, power, tones = np.array(p[:3]), p[3], p[4], np.array(p[5:])
        return np.concatenate([
            residual(simulate(photos, lambda ph: sharpen(ph.log, a, lim, tones=tones)), w40),
            residual(simulate(photos, lambda ph: sharpen(ph.log, a * 2.0 ** power, lim, tones=tones)), w80),
            # The weighting is a gentle curve, not a fit to every bin's quirks.
            TONE_SMOOTHING * np.diff(tones, 2)])
    r = least_squares(f25, x0, bounds=([-3, -3, -3, 0.01, 0.5] + [0] * nt, [6, 6, 6, 3, 2.5] + [3] * nt),
                      diff_step=0.05, max_nfev=30)
    a25, lim25, power, tones = r.x[:3], float(r.x[3]), float(r.x[4]), r.x[5:]
    print(f"detail 25: amounts {a25.round(3)} limit {lim25:.3f} power {power:.3f} tones {tones.round(2)}  rms miss {np.sqrt(np.mean(r.fun ** 2)):.3f}")
    # Detail 75 at 40: its own shape and limit, the same tone weighting.
    w75 = target(lr, SETTINGS["d75"], names)
    f75 = lambda p: residual(simulate(photos, lambda ph: sharpen(ph.log, p[:3], p[3], tones=tones)), w75)
    r = least_squares(f75, list(a25) + [lim25], bounds=([-3, -3, -3, 0.01], [6, 6, 6, 3]), diff_step=0.05, max_nfev=30)
    print(f"detail 75: amounts {r.x[:3].round(3)} limit {r.x[3]:.3f}  rms miss {np.sqrt(np.mean(r.fun ** 2)):.3f}")
    # Amounts and limit trade exactly against the weighting's scale; state
    # them with the midtones (L* 57) weighted 1.
    c = float(tones[3])
    a25, lim25, tones, r.x[:4] = a25 * c, lim25 * c, tones / c, r.x[:4] * c
    state["sharpen"] = {"d25": {"amounts": a25.tolist(), "limit": lim25}, "power": power, "tones": tones.tolist(),
                        "d75": {"amounts": r.x[:3].tolist(), "limit": float(r.x[3])}}
    state["colour"] = old.get("colour", {})
    # Masking 50, on Sharpening 40's bands.
    s40 = state["sharpen"]["d25"]
    want = target(lr, MASKING, names)
    f = lambda p: residual(simulate(photos, lambda ph: sharpen(ph.log, s40["amounts"], s40["limit"], tones=tones,
                                                              mask=(p[0], p[0] + abs(p[1])))), want)
    r = least_squares(f, [0.01, 0.02], bounds=([0, 1e-4], [0.5, 0.5]), diff_step=0.05, max_nfev=40)
    state["masking50"] = [float(r.x[0]), float(r.x[0] + abs(r.x[1]))]
    print(f"masking 50: thresholds {state['masking50']}  rms miss {np.sqrt(np.mean(r.fun ** 2)):.3f}")
    json.dump(state, open(TARGETS, "w"), indent=1)
    write(state)


def box(x, r):
    return uniform_filter(x, 2 * r + 1, mode="nearest") if r > 0 else x


def guided(guide, x, r, eps):
    """detail.rs's guided filter: box means of radius r."""
    mg, mx = box(guide, r), box(x, r)
    a = (box(guide * x, r) - mg * mx) / (np.maximum(box(guide * guide, r) - mg * mg, 0) + eps)
    b = mx - a * mg
    return box(a, r) * guide + box(b, r)


def colour_nr(rgb, radius, eps, mix, broad, broad_mix):
    """Chromaticity smoothed by a luminance-steered guided filter, then a
    broad Gaussian, mixed in; luminance untouched."""
    Y = np.maximum(rgb @ np.array([0.2126, 0.7152, 0.0722]), 1e-6)
    log = np.log2(Y + 1 / 16384)
    out = np.empty_like(rgb)
    for c in range(3):
        q = rgb[..., c] / Y
        sm = guided(log, q, radius, eps)
        if broad_mix > 0:
            sm = sm + broad_mix * (gauss(sm, broad) - sm)
        out[..., c] = (q + mix * (sm - q)) * Y
    return out


def chroma_guide(rgb):
    """Opponent colour in cube-root light (roughly as Lab sees it, so shadow
    noise is not exaggerated), lightly blurred: what the adaptive stage
    judges colour edges by."""
    F = np.cbrt(np.maximum(rgb, 0))
    return [gauss(F[..., 0] - F[..., 1], 1.0), gauss(F[..., 2] - F[..., 1], 1.0)]


NOISE_RADIUS = 4


def chroma_noise(guide, radius=None):
    """The photo's colour noise: a robust spread (median absolute residual
    from a local mean of `radius`) of the guide, over the frame. High-ISO
    colour noise comes in blotches several pixels across, so the mean is
    wider than a pixel's neighbours."""
    radius = radius or NOISE_RADIUS
    r = np.concatenate([np.abs(g - box(g, radius))[::3, ::3].ravel() for g in guide])
    return 1.4826 * float(np.median(r))


def guided2(g1, g2, x, r, eps):
    """A guided filter steered by a two-channel guide (He, Sun and Tang):
    linear in `x`, so a constant stays constant."""
    m1, m2 = box(g1, r), box(g2, r)
    mx = box(x, r)
    v11 = box(g1 * g1, r) - m1 * m1 + eps
    v22 = box(g2 * g2, r) - m2 * m2 + eps
    v12 = box(g1 * g2, r) - m1 * m2
    c1 = box(g1 * x, r) - m1 * mx
    c2 = box(g2 * x, r) - m2 * mx
    det = v11 * v22 - v12 * v12
    a1 = (v22 * c1 - v12 * c2) / det
    a2 = (v11 * c2 - v12 * c1) / det
    b = mx - a1 * m1 - a2 * m2
    return box(a1, r) * g1 + box(a2, r) * g2 + box(b, r)


def colour_nr2(rgb, p, radius2):
    """Lightroom-like colour NR: the fine stage (colour_nr's guided filter by
    luminance), then an adaptive one that smooths colour variation smaller
    than the photo's own colour noise over `radius2`, keeping colour edges."""
    radius, eps, mix, k, mix2 = p
    Y = np.maximum(rgb @ np.array([0.2126, 0.7152, 0.0722]), 1e-6)
    log = np.log2(Y + 1 / 16384)
    q = [rgb[..., c] / Y for c in range(3)]
    q = [x + mix * (guided(log, x, int(radius), eps) - x) for x in q]
    # The noise is the photo's own, judged before any smoothing (the engine
    # estimates it once for the whole frame).
    e = (k * chroma_noise(chroma_guide(rgb))) ** 2
    guide = chroma_guide(np.stack(q, -1) * Y[..., None])
    q = [x + mix2 * (guided2(guide[0], guide[1], x, radius2, e) - x) for x in q]
    return np.stack(q, -1) * Y[..., None]


LUMA = np.array([0.2126, 0.7152, 0.0722])


def rebuild(o1, o2, Y):
    """Light with opponent colour (o1, o2) in cube-root light and luminance
    exactly Y: solve for the green root by Newton (the luminance grows with
    it wherever the channels are positive)."""
    g = np.cbrt(np.maximum(Y, 0))
    for _ in range(8):
        r, b = np.maximum(g + o1, 0), np.maximum(g + o2, 0)
        gg = np.maximum(g, 0)
        f = LUMA[0] * r ** 3 + LUMA[1] * gg ** 3 + LUMA[2] * b ** 3 - Y
        d = 3 * (LUMA[0] * r ** 2 + LUMA[1] * gg ** 2 + LUMA[2] * b ** 2) + 1e-9
        g = g - f / d
    r, b, gg = np.maximum(g + o1, 0), np.maximum(g + o2, 0), np.maximum(g, 0)
    return np.stack([r ** 3, gg ** 3, b ** 3], -1)


def colour_nr3(rgb, p, radius2):
    """Colour NR on opponent colour (as Lab's a* and b*, in cube-root light),
    so brightness noise cannot leave colour flicker behind; luminance kept
    exactly. Fine stage guided by luminance, adaptive stage guided by the
    colour, smoothing what is within `k` times the photo's colour noise."""
    radius, eps, mix, k, mix2, power = p
    Y = np.maximum(rgb @ LUMA, 0)
    log = np.log2(Y + 1 / 16384)
    F = np.cbrt(np.maximum(rgb, 0))
    o = [F[..., 0] - F[..., 1], F[..., 2] - F[..., 1]]
    o = [x + mix * (guided(log, x, int(radius), eps) - x) for x in o]
    # What counts as noise grows faster than the noise: Lightroom's NR is
    # disproportionately stronger on a noisy photo. Relative to a noise of
    # 0.01 (a well-exposed ISO 1000-1250 frame).
    n = chroma_noise(chroma_guide(rgb))
    e = (k * n * (n / 0.01) ** power) ** 2
    guide = [gauss(x, 1.0) for x in o]
    # What is left of the colour noise shrinks as the photo gets noisier.
    mix2 = 1 - (1 - mix2) * min(1.0, 0.01 / max(n, 1e-6))
    o = [x + mix2 * (guided2(guide[0], guide[1], x, radius2, e) - x) for x in o]
    return rebuild(o[0], o[1], Y)


def fit_colour2(lr_path, base_dir):
    """Per photo, not pooled: the point is that the strength follows each
    photo's noise."""
    lr = json.load(open(lr_path))
    e = lr["settings"][COLOUR]["photos"]
    names = sorted(set(e) & {f[:-len("__neutral_0.f32")] for f in os.listdir(base_dir) if f.endswith("__neutral_0.f32")})
    wants = [np.array(e[n]["adobe"]["colour_gain"], dtype=float) for n in names]
    photos = []
    for n in names:
        p = f"{base_dir}/{n}__neutral_0.f32"
        meta = json.load(open(p[:-4] + ".json"))
        enc = np.fromfile(p, dtype="<f4").reshape(meta["height"], meta["width"], 3)
        h, w = enc.shape[:2]
        c = min(COLOUR_CROP, h, w)
        enc = enc[(h - c) // 2:(h + c) // 2, (w - c) // 2:(w + c) // 2].astype(np.float64)
        rgb = np.where(enc <= 0.04045, enc / 12.92, ((np.clip(enc, 0, None) + 0.055) / 1.055) ** 2.4)
        photos.append((D.lab(rgb @ D.SRGB_TO_XYZ.T), rgb))

    def sims(p, r2):
        with ThreadPoolExecutor(8) as ex:
            return list(ex.map(lambda ph: colour_gains(ph[0], colour_nr3(ph[1], [2] + list(p), r2)), photos))

    # Colour NR shows where there is colour noise: weight each photo by it.
    noise = [chroma_noise(chroma_guide(ph[1])) for ph in photos]
    visible = [float(np.clip(np.sqrt(n / 0.005), 0.5, 3.0)) for n in noise]

    def f(p, r2):
        return np.nan_to_num(np.concatenate([(s - w) * WEIGHTS * v for s, w, v in zip(sims(p, r2), wants, visible)]))
    best = None
    for r2 in (24,):
        r = least_squares(lambda p: f(p, r2), [0.0847, 0.556, 3.2, 0.81, 0.474], bounds=([1e-5, 0, 0.1, 0, 0], [0.5, 1, 60, 1, 3]),
                          diff_step=0.05, max_nfev=25)
        miss = float(np.sqrt(np.mean(r.fun ** 2)))
        print(f"adaptive radius {r2}: eps {r.x[0]:.4f} mix {r.x[1]:.3f} k {r.x[2]:.2f} mix2 {r.x[3]:.3f} power {r.x[4]:.3f}  rms miss {miss:.3f}")
        if best is None or miss < best[0]:
            best = (miss, r2, r.x)
    _, r2, x = best
    for n, s_, w in zip(names, sims(x, r2), wants):
        print(f"  {n[:12]:12} LR " + " ".join(f"{v:4.2f}" for v in w[[0, 2, 4, 6, 8]]) + "   sim " + " ".join(f"{v:4.2f}" for v in s_[[0, 2, 4, 6, 8]]))
    state = json.load(open(TARGETS))
    state["colour"] = {"radius": 2, "eps": float(x[0]), "mix": float(x[1]), "noise_k": float(x[2]),
                       "mix2": float(x[3]), "radius2": r2, "noise_power": float(x[4])}
    json.dump(state, open(TARGETS, "w"), indent=1)
    write(state)


def colour_gains(lab0, rgb1):
    lab1 = D.lab(rgb1 @ D.SRGB_TO_XYZ.T)
    g = []
    for c in (1, 2):
        b0, b1 = S.bands(lab0[..., c], S.SIGMAS), S.bands(lab1[..., c], S.SIGMAS)
        g.append([S.slope(x, y) for x, y in zip(b0, b1)])
    return np.mean(np.array(g, dtype=float), 0)


def fit_colour(lr_path, base_dir):
    lr = json.load(open(lr_path))
    e = lr["settings"][COLOUR]["photos"]
    names = sorted(set(e) & {f[:-len("__neutral_0.f32")] for f in os.listdir(base_dir) if f.endswith("__neutral_0.f32")})
    # The RAWs and camera JPEGs: where colour noise is.
    names = [n for n in names if n.startswith(("DSC", "_AAF"))][:COLOUR_PHOTOS]
    want = np.nanmedian(np.array([e[n]["adobe"]["colour_gain"] for n in names], dtype=float), 0)
    photos = []
    for n in names:
        p = f"{base_dir}/{n}__neutral_0.f32"
        meta = json.load(open(p[:-4] + ".json"))
        enc = np.fromfile(p, dtype="<f4").reshape(meta["height"], meta["width"], 3)
        h, w = enc.shape[:2]
        enc = enc[(h - COLOUR_CROP) // 2:(h + COLOUR_CROP) // 2, (w - COLOUR_CROP) // 2:(w + COLOUR_CROP) // 2].astype(np.float64)
        rgb = np.where(enc <= 0.04045, enc / 12.92, ((np.clip(enc, 0, None) + 0.055) / 1.055) ** 2.4)
        photos.append((D.lab(rgb @ D.SRGB_TO_XYZ.T), rgb))

    def sim(radius, p):
        with ThreadPoolExecutor(8) as ex:
            rows = list(ex.map(lambda ph: colour_gains(ph[0], colour_nr(ph[1], radius, *p)), photos))
        return np.nanmedian(np.array(rows), 0)
    best = None
    for radius in (2, 4, 7):
        f = lambda p: np.nan_to_num((sim(radius, p) - want) * WEIGHTS)
        r = least_squares(f, [0.01, 0.9, 6.0, 0.3], bounds=([1e-5, 0, 1, 0], [1, 1, 40, 1]), diff_step=0.05, max_nfev=20)
        miss = float(np.sqrt(np.mean(r.fun ** 2)))
        print(f"colour radius {radius}: eps {r.x[0]:.4f} mix {r.x[1]:.3f} broad {r.x[2]:.2f} x{r.x[3]:.3f}  rms miss {miss:.3f}")
        if best is None or miss < best[0]:
            best = (miss, radius, r.x)
    _, radius, x = best
    print("LR  " + " ".join(f"{v:5.2f}" for v in want))
    print("sim " + " ".join(f"{v:5.2f}" for v in sim(radius, x)))
    state = json.load(open(TARGETS))
    state["colour"] = {"radius": radius, "eps": float(x[0]), "mix": float(x[1]), "broad": float(x[2]), "broad_mix": float(x[3])}
    json.dump(state, open(TARGETS, "w"), indent=1)
    write(state)


def write(state=None):
    state = state or json.load(open(TARGETS))
    s = state["sharpen"]
    lines = [
        "//! Darkroom Index's Sharpening, fitted to Lightroom's by tools/fit_sharpen.py",
        "//! from tools/adobe_sharpen.py's measurements; do not edit.",
        "//!",
        "//! Bands at sigma `SIGMAS` x Radius (full-resolution pixels): their amounts",
        "//! at Lightroom's Sharpening 40, scaled by (Sharpening / 40) ^ `POWER`, and",
        "//! the soft limit (stops) on their sum, at Detail 25 and at Detail 75.",
        "//! `MASKING_50`: the edge mask's thresholds (log2 per pixel) at Masking 50.",
        "",
        "pub const SIGMAS: [f32; 3] = [" + ", ".join(f"{x:.2f}" for x in state["sigmas"]) + "];",
    ]
    lines.append(f"pub const POWER: f32 = {s['power']:.5f};")
    lines.append("/// The tone weighting: at `TONE_KNOTS` on the tonal key (DaVinci")
    lines.append("/// Intermediate, of the luminance blurred at sigma 2), how much of the")
    lines.append("/// sharpening is applied.")
    lines.append("pub const TONE_KNOTS: [f32; 5] = [" + ", ".join(f"{x:.4f}" for x in TONE_KNOTS) + "];")
    lines.append("pub const TONES: [f32; 5] = [" + ", ".join(f"{x:.4f}" for x in s["tones"]) + "];")
    for key in ("d25", "d75"):
        a = s[key]["amounts"]
        lines.append(f"pub const {key.upper()}: [f32; 3] = [" + ", ".join(f"{x:.5f}" for x in a) + "];")
        lines.append(f"pub const {key.upper()}_LIMIT: f32 = {s[key]['limit']:.5f};")
    m = state["masking50"]
    lines.append(f"pub const MASKING_50: [f32; 2] = [{m[0]:.5f}, {m[1]:.5f}];")
    c = state.get("colour", {})
    if c:
        lines.append("/// Color NR at 25 (Detail 50, Smoothness 50). The fine stage: a guided")
        lines.append("/// filter by luminance, its radius (full-resolution pixels), regularisation")
        lines.append("/// and how much is mixed in. The adaptive stage: a guided filter by the")
        lines.append("/// colour itself over `radius2`, smoothing colour variation smaller than")
        lines.append("/// `noise_k` times the photo's own colour noise, mixed in by `mix2`.")
        c = {"radius2": 12, "noise_k": 2.0, "mix2": 0.9, "noise_power": 0.0, **c}
        lines.append("/// Colour is opponent colour in cube-root light; noise counts as noise up")
        lines.append("/// to noise_k x noise x (noise / 0.01) ^ `noise_power`.")
        lines.append(f"pub const COLOUR_25: [f32; 7] = [{c['radius']:.4f}, {c['eps']:.6f}, {c['mix']:.4f}, "
                     f"{c['radius2']:.4f}, {c['noise_k']:.4f}, {c['mix2']:.4f}, {c['noise_power']:.4f}];")
    open(TABLE, "w").write("\n".join(lines) + "\n")
    import subprocess
    subprocess.run(["rustfmt", "--edition", "2024", TABLE], check=False)
    print("wrote", os.path.normpath(TABLE))


if __name__ == "__main__":
    if len(sys.argv) >= 4 and sys.argv[1] == "fit":
        fit(sys.argv[2], sys.argv[3])
    elif len(sys.argv) >= 4 and sys.argv[1] == "colour":
        fit_colour2(sys.argv[2], sys.argv[3])
    elif len(sys.argv) >= 2 and sys.argv[1] == "write":
        write()
    else:
        print(__doc__)
