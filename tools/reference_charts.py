#!/usr/bin/env python3
"""Reference charts for checking Resolve's controls against v3.

Writes 16-bit sRGB TIFFs into a folder. Each chart isolates one thing a
control can do — tone, hue, saturation, skin, edges — so its effect can be
read off directly rather than guessed from a photograph.

    python3 tools/reference_charts.py OUT_DIR
"""
import os
import sys

import cv2
import numpy as np

N = 1024


def srgb_from_linear(v):
    v = np.clip(v, 0, 1)
    return np.where(v <= 0.0031308, 12.92 * v, 1.055 * v ** (1 / 2.4) - 0.055)


def hsv_to_rgb(h, s, v):
    h6 = (h % 1.0) * 6
    i = np.floor(h6).astype(int)
    f = h6 - i
    p, q, t = v * (1 - s), v * (1 - s * f), v * (1 - s * (1 - f))
    r = np.choose(i % 6, [v, q, p, p, t, v])
    g = np.choose(i % 6, [t, v, v, q, p, p])
    b = np.choose(i % 6, [p, p, t, v, v, q])
    return np.stack([r, g, b], -1)


def save(name, rgb):
    """rgb: float sRGB-encoded 0..1, shape (H, W, 3)."""
    out = (np.clip(rgb, 0, 1) * 65535 + 0.5).astype(np.uint16)
    cv2.imwrite(os.path.join(OUT, name + ".tif"), out[..., ::-1])
    print("wrote", name)


def grey_ramps():
    """Top: continuous ramp in display code values. Middle: continuous ramp
    in linear light (stops), where a stop is the same width everywhere.
    Bottom: 21 steps, 5% apart, with a 1% texture so local controls show."""
    img = np.zeros((N, N, 3))
    x = np.linspace(0, 1, N)
    img[: N // 3] = x[None, :, None]
    stops = np.linspace(-8, 0, N)
    img[N // 3 : 2 * N // 3] = srgb_from_linear(2.0 ** stops)[None, :, None]
    steps = np.floor(x * 21) / 20
    yy, xx = np.mgrid[0:N, 0:N]
    tex = 0.01 * (((xx // 8 + yy // 8) % 2) * 2 - 1)
    img[2 * N // 3 :] = np.clip(steps + tex[2 * N // 3 :], 0, 1)[..., None]
    save("chart-grey-ramps", img)


def hue_chroma():
    """Hue across, saturation down (pure at top, grey at bottom), at three
    brightness levels stacked: bright, mid, dark. Reads hue shifts,
    saturation changes and how tone controls treat colour."""
    h = np.linspace(0, 1, N, endpoint=False)[None, :]
    s = np.linspace(1, 0, N // 3)[:, None]
    bands = []
    for v in (0.95, 0.5, 0.18):
        bands.append(hsv_to_rgb(np.broadcast_to(h, (N // 3, N)), np.broadcast_to(s, (N // 3, N)), v))
    img = np.concatenate(bands, 0)
    img = np.concatenate([img, np.zeros((N - img.shape[0], N, 3))], 0)
    save("chart-hue-saturation", img)


def hue_lightness():
    """Twelve hues across, lightness from black to white down: shows what a
    tone control does to each colour at every level."""
    img = np.zeros((N, N, 3))
    for k in range(12):
        x0, x1 = k * N // 12, (k + 1) * N // 12
        v = np.linspace(1, 0.02, N)[:, None]
        h = np.full((N, x1 - x0), k / 12)
        img[:, x0:x1] = hsv_to_rgb(h, np.full_like(h, 0.85), np.broadcast_to(v, h.shape))
    save("chart-hue-lightness", img)


def colour_checker():
    """A 24-patch chart (the usual layout) at three exposures: −2, 0, +2
    stops. Skin tones are the first two patches."""
    patches = [
        (115, 82, 68), (194, 150, 130), (98, 122, 157), (87, 108, 67), (133, 128, 177), (103, 189, 170),
        (214, 126, 44), (80, 91, 166), (193, 90, 99), (94, 60, 108), (157, 188, 64), (224, 163, 46),
        (56, 61, 150), (70, 148, 73), (175, 54, 60), (231, 199, 31), (187, 86, 149), (8, 133, 161),
        (243, 243, 242), (200, 200, 200), (160, 160, 160), (122, 122, 121), (85, 85, 85), (52, 52, 52),
    ]
    lin = ((np.array(patches) / 255 + 0.055) / 1.055) ** 2.4
    img = np.zeros((N, N, 3))
    for band, stops in enumerate((-2, 0, 2)):
        y0 = band * N // 3
        cell_h, cell_w = (N // 3) // 4, N // 6
        for i, p in enumerate(lin * 2.0 ** stops):
            r, c = divmod(i, 6)
            img[y0 + r * cell_h + 6 : y0 + (r + 1) * cell_h - 6, c * cell_w + 6 : (c + 1) * cell_w - 6] = srgb_from_linear(p)
    save("chart-colour-checker", img)


def skin():
    """Skin tones from pale to deep, each a ramp from shadow to highlight,
    with faint texture: where hue and saturation drift show first."""
    tones = [(0.94, 0.80, 0.70), (0.86, 0.66, 0.53), (0.76, 0.55, 0.42), (0.62, 0.42, 0.31), (0.45, 0.29, 0.21), (0.30, 0.19, 0.14)]
    img = np.zeros((N, N, 3))
    yy, xx = np.mgrid[0:N, 0:N]
    tex = 1 + 0.02 * np.sin(xx / 3.0) * np.sin(yy / 4.0)
    for k, t in enumerate(tones):
        y0, y1 = k * N // 6, (k + 1) * N // 6
        lin = ((np.array(t) + 0.055) / 1.055) ** 2.4
        gain = 2.0 ** np.linspace(-3, 1.5, N)[None, :, None]
        img[y0:y1] = srgb_from_linear(lin[None, None, :] * gain * tex[y0:y1, :, None])
    save("chart-skin", img)


def edges_and_texture():
    """For controls that look at neighbours (Shadows, Highlights, Midtone
    Detail): hard edges between tones, soft gradients, fine texture at
    several contrasts, and a bright disc on a dark ground."""
    img = np.full((N, N, 3), 0.4)
    # Left: hard steps at four contrasts.
    for k, (lo, hi) in enumerate([(0.1, 0.9), (0.3, 0.7), (0.02, 0.4), (0.6, 0.98)]):
        y0 = k * N // 4
        img[y0 : y0 + N // 4, : N // 4] = lo
        img[y0 : y0 + N // 4, N // 4 : N // 2] = hi
    # Right top: texture at growing amplitude on a mid grey.
    yy, xx = np.mgrid[0 : N // 2, 0 : N // 2]
    amp = np.linspace(0.005, 0.15, N // 2)[None, :]
    noise = np.random.default_rng(3).standard_normal((N // 2, N // 2))
    img[: N // 2, N // 2 :] = np.clip(0.45 + amp * noise, 0, 1)[..., None]
    # Right bottom: a bright disc and a dark disc on a mid ground, with a soft
    # gradient behind.
    grad = np.linspace(0.2, 0.7, N // 2)[:, None]
    block = np.broadcast_to(grad, (N // 2, N // 2)).copy()
    cy, cx = N // 4, N // 4
    r2 = (yy - cy) ** 2 + (xx - cx) ** 2
    block[r2 < (N // 12) ** 2] = 0.97
    block[(yy - cy) ** 2 + (xx - 3 * N // 8) ** 2 < (N // 20) ** 2] = 0.03
    img[N // 2 :, N // 2 :] = block[..., None]
    save("chart-edges-texture", img)


if __name__ == "__main__":
    OUT = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(OUT, exist_ok=True)
    grey_ramps()
    hue_chroma()
    hue_lightness()
    colour_checker()
    skin()
    edges_and_texture()
