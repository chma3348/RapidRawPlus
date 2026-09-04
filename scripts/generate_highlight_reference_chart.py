#!/usr/bin/env python3
"""Generate a highlight stress chart for Resolve/RapidRAW matching.

The chart is designed for highlight slider measurements: upper grey ramps,
near-white color patches, clipped-channel ramps, and specular-like soft spots.
"""

from __future__ import annotations

import argparse
import colorsys
import math
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont


W, H = 1920, 1080
BG = (0, 0, 0)
HUES = [0, 25, 55, 90, 130, 165, 190, 215, 245, 275, 305, 335]


def srgb8(v: float) -> int:
    return max(0, min(255, int(round(v * 255.0))))


def rgb8(rgb: tuple[float, float, float]) -> tuple[int, int, int]:
    return tuple(srgb8(v) for v in rgb)


def hsv_rgb(h: float, s: float, v: float) -> tuple[int, int, int]:
    return rgb8(colorsys.hsv_to_rgb((h % 360) / 360.0, s, v))


def mix(a: tuple[int, int, int], b: tuple[int, int, int], t: float) -> tuple[int, int, int]:
    return tuple(srgb8((a[i] / 255.0) * (1.0 - t) + (b[i] / 255.0) * t) for i in range(3))


def draw_label(draw: ImageDraw.ImageDraw, xy: tuple[int, int], text: str) -> None:
    font = ImageFont.load_default()
    draw.text(xy, text, fill=(180, 180, 180), font=font)


def draw_steps(
    draw: ImageDraw.ImageDraw,
    x: int,
    y: int,
    patch_w: int,
    patch_h: int,
    values: list[float],
    gap: int = 4,
) -> None:
    for i, val in enumerate(values):
        g = srgb8(val)
        x0 = x + i * (patch_w + gap)
        draw.rectangle((x0, y, x0 + patch_w - 1, y + patch_h - 1), fill=(g, g, g))


def draw_hue_grid(draw: ImageDraw.ImageDraw, x: int, y: int) -> None:
    patch_w, patch_h = 68, 66
    gap = 5
    rows = [
        ("pastel max", 0.25, 1.00),
        ("medium max", 0.50, 1.00),
        ("strong max", 0.78, 1.00),
        ("full max", 1.00, 1.00),
        ("strong 92", 0.78, 0.92),
        ("full 88", 1.00, 0.88),
    ]
    for row, (_, sat, val) in enumerate(rows):
        for col, hue in enumerate(HUES):
            x0 = x + col * (patch_w + gap)
            y0 = y + row * (patch_h + gap)
            draw.rectangle((x0, y0, x0 + patch_w - 1, y0 + patch_h - 1), fill=hsv_rgb(hue, sat, val))


def draw_gradient(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int], left: tuple[int, int, int], right: tuple[int, int, int]) -> None:
    x1, y1, x2, y2 = box
    width = max(1, x2 - x1)
    for x in range(x1, x2):
        t = (x - x1) / max(1, width - 1)
        c = mix(left, right, t)
        draw.line((x, y1, x, y2), fill=c)


def draw_hue_to_white_ramps(draw: ImageDraw.ImageDraw, x: int, y: int) -> None:
    ramp_w, ramp_h = 276, 36
    gap_x, gap_y = 18, 12
    for i, hue in enumerate([0, 55, 115, 185, 245, 305]):
        col = i % 3
        row = i // 3
        x0 = x + col * (ramp_w + gap_x)
        y0 = y + row * (ramp_h + gap_y)
        base = hsv_rgb(hue, 1.0, 1.0)
        draw_gradient(draw, (x0, y0, x0 + ramp_w, y0 + ramp_h), base, (255, 255, 255))


def draw_channel_clip_ramps(draw: ImageDraw.ImageDraw, x: int, y: int) -> None:
    ramp_w, ramp_h = 420, 26
    gap_x, gap_y = 16, 10
    starts = [
        ((255, 80, 80), (255, 255, 255)),
        ((255, 200, 30), (255, 255, 255)),
        ((70, 255, 110), (255, 255, 255)),
        ((55, 210, 255), (255, 255, 255)),
        ((75, 95, 255), (255, 255, 255)),
        ((255, 70, 220), (255, 255, 255)),
    ]
    for i, (left, right) in enumerate(starts):
        x0 = x + (i % 2) * (ramp_w + gap_x)
        y0 = y + (i // 2) * (ramp_h + gap_y)
        draw_gradient(draw, (x0, y0, x0 + ramp_w, y0 + ramp_h), left, right)


def draw_grey_ramps(draw: ImageDraw.ImageDraw) -> None:
    draw_steps(draw, 80, 120, 83, 98, [0.40, 0.48, 0.56, 0.64, 0.70, 0.76, 0.81, 0.86, 0.90, 0.93, 0.96, 0.98, 1.00], 5)
    draw_gradient(draw, (80, 250, 1840, 310), (115, 115, 115), (255, 255, 255))
    draw_gradient(draw, (80, 328, 1840, 388), (205, 205, 205), (255, 255, 255))
    draw.rectangle((80, 406, 1840, 448), fill=(255, 255, 255))


def draw_specular_field(img: Image.Image, x: int, y: int, width: int, height: int) -> None:
    px = img.load()
    spots = [
        (0.18, 0.50, 0.13, (255, 185, 70)),
        (0.39, 0.42, 0.10, (255, 240, 140)),
        (0.61, 0.47, 0.12, (150, 220, 255)),
        (0.81, 0.40, 0.09, (255, 130, 220)),
    ]
    for yy in range(y, y + height):
        for xx in range(x, x + width):
            u = (xx - x) / max(1, width - 1)
            v = (yy - y) / max(1, height - 1)
            base = [18 + 58 * u, 18 + 35 * u, 18 + 24 * u]
            for sx, sy, radius, color in spots:
                d = math.hypot((u - sx) / radius, (v - sy) / radius)
                halo = max(0.0, 1.0 - d)
                core = max(0.0, 1.0 - d * 4.0)
                for i in range(3):
                    base[i] = base[i] * (1.0 - halo * 0.78) + color[i] * halo * 0.78
                    base[i] = base[i] * (1.0 - core) + 255.0 * core
            px[xx, yy] = tuple(max(0, min(255, int(round(c)))) for c in base)


def generate(path: Path) -> None:
    img = Image.new("RGB", (W, H), BG)
    draw = ImageDraw.Draw(img)

    # Registration marks help catch accidental cropping/scaling.
    draw.rectangle((0, 0, 79, 79), fill=(255, 255, 255))
    draw.rectangle((1840, 0, 1919, 79), fill=(255, 255, 255))
    draw.rectangle((0, 1000, 79, 1079), fill=(255, 255, 255))
    draw.rectangle((1840, 1000, 1919, 1079), fill=(255, 255, 255))

    draw_label(draw, (80, 92), "upper grey steps / highlight ramps")
    draw_grey_ramps(draw)

    draw_label(draw, (80, 468), "near-white hue patches")
    draw_hue_grid(draw, 80, 492)

    draw_label(draw, (80, 930), "hue-to-white ramps")
    draw_hue_to_white_ramps(draw, 80, 954)

    draw_label(draw, (970, 930), "clipped-channel ramps")
    draw_channel_clip_ramps(draw, 970, 954)

    draw_label(draw, (970, 468), "specular highlight stress field")
    draw_specular_field(img, 970, 492, 870, 408)

    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path)


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate a highlight reference PNG.")
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("analysis_charts/highlight_reference_chart.png"),
        help="Output PNG path.",
    )
    args = parser.parse_args()
    generate(args.out)
    print(args.out)


if __name__ == "__main__":
    main()
