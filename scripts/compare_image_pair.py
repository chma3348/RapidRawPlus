#!/usr/bin/env python3
"""Compare two rendered images and optionally write a visual diff sheet.

This intentionally depends only on Pillow so it can run in this repo's current
dev environment without NumPy.
"""

from __future__ import annotations

import argparse
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

from PIL import Image, ImageDraw, ImageFont


LUMA = (0.2126, 0.7152, 0.0722)
DEFAULT_BINS = (0.0, 0.05, 0.10, 0.20, 0.35, 0.50, 0.75, 1.01)


@dataclass
class Stats:
    n: int = 0
    se: float = 0.0
    dr: float = 0.0
    dg: float = 0.0
    db: float = 0.0
    dy: float = 0.0
    ds: float = 0.0
    edge_ref: float = 0.0
    edge_candidate: float = 0.0
    edge_n: int = 0

    def add(self, ref: tuple[int, int, int], cand: tuple[int, int, int]) -> None:
        rf, gf, bf = [v / 255.0 for v in ref]
        cf, mf, yf = [v / 255.0 for v in cand]
        d0 = cand[0] - ref[0]
        d1 = cand[1] - ref[1]
        d2 = cand[2] - ref[2]
        self.n += 1
        self.se += (d0 * d0 + d1 * d1 + d2 * d2) / 3.0
        self.dr += d0
        self.dg += d1
        self.db += d2
        self.dy += (luma_f((cf, mf, yf)) - luma_f((rf, gf, bf))) * 255.0
        self.ds += saturation_f((cf, mf, yf)) - saturation_f((rf, gf, bf))

    def add_edge(self, ref_edge: float, candidate_edge: float) -> None:
        self.edge_ref += ref_edge
        self.edge_candidate += candidate_edge
        self.edge_n += 1

    def summary(self) -> str:
        if self.n == 0:
            return "no samples"
        edge_ratio = self.edge_candidate / self.edge_ref if self.edge_ref > 1.0e-9 else 1.0
        return (
            f"n={self.n} rms={math.sqrt(self.se / self.n):.2f} "
            f"dRGB=({self.dr / self.n:+.2f},{self.dg / self.n:+.2f},{self.db / self.n:+.2f}) "
            f"dY={self.dy / self.n:+.2f} dSat={self.ds / self.n:+.4f} "
            f"edgeRatio={edge_ratio:.3f}"
        )


def luma_rgb(rgb: tuple[int, int, int]) -> float:
    return (rgb[0] * LUMA[0] + rgb[1] * LUMA[1] + rgb[2] * LUMA[2]) / 255.0


def luma_f(rgb: tuple[float, float, float]) -> float:
    return rgb[0] * LUMA[0] + rgb[1] * LUMA[1] + rgb[2] * LUMA[2]


def saturation_f(rgb: tuple[float, float, float]) -> float:
    hi = max(rgb)
    lo = min(rgb)
    return (hi - lo) / hi if hi > 1.0e-8 else 0.0


def parse_region(region: str) -> tuple[str, tuple[int, int, int, int]]:
    name, coords = region.split(":", 1)
    values = [int(v.strip()) for v in coords.split(",")]
    if len(values) != 4:
        raise ValueError(f"Region needs x1,y1,x2,y2: {region}")
    return name, tuple(values)  # type: ignore[return-value]


def iter_samples(width: int, height: int, step: int, box: tuple[int, int, int, int] | None = None) -> Iterable[tuple[int, int]]:
    x1, y1, x2, y2 = box or (0, 0, width, height)
    for y in range(max(0, y1), min(height, y2), step):
        for x in range(max(0, x1), min(width, x2), step):
            yield x, y


def collect_stats(
    ref: Image.Image,
    candidate: Image.Image,
    step: int,
    box: tuple[int, int, int, int] | None = None,
) -> Stats:
    width, height = ref.size
    ref_px = ref.load()
    cand_px = candidate.load()
    stats = Stats()
    for x, y in iter_samples(width, height, step, box):
        stats.add(ref_px[x, y], cand_px[x, y])
        if x + step < width and y + step < height:
            ref_y = luma_rgb(ref_px[x, y])
            cand_y = luma_rgb(cand_px[x, y])
            ref_edge = abs(luma_rgb(ref_px[x + step, y]) - ref_y) + abs(luma_rgb(ref_px[x, y + step]) - ref_y)
            cand_edge = abs(luma_rgb(cand_px[x + step, y]) - cand_y) + abs(luma_rgb(cand_px[x, y + step]) - cand_y)
            stats.add_edge(ref_edge, cand_edge)
    return stats


def tone_bins(ref: Image.Image, candidate: Image.Image, step: int) -> list[tuple[str, Stats]]:
    width, height = ref.size
    ref_px = ref.load()
    cand_px = candidate.load()
    bins = [(f"{lo:.2f}-{hi:.2f}", lo, hi, Stats()) for lo, hi in zip(DEFAULT_BINS[:-1], DEFAULT_BINS[1:])]
    for x, y in iter_samples(width, height, step):
        ref_rgb = ref_px[x, y]
        cand_rgb = cand_px[x, y]
        y_ref = luma_rgb(ref_rgb)
        for _, lo, hi, stats in bins:
            if lo <= y_ref < hi:
                stats.add(ref_rgb, cand_rgb)
                if x + step < width and y + step < height:
                    ref_edge = abs(luma_rgb(ref_px[x + step, y]) - y_ref) + abs(luma_rgb(ref_px[x, y + step]) - y_ref)
                    cand_y = luma_rgb(cand_rgb)
                    cand_edge = abs(luma_rgb(cand_px[x + step, y]) - cand_y) + abs(luma_rgb(cand_px[x, y + step]) - cand_y)
                    stats.add_edge(ref_edge, cand_edge)
                break
    return [(label, stats) for label, _, _, stats in bins if stats.n > 0]


def color_heat(value: float, limit: float) -> tuple[int, int, int]:
    t = max(-1.0, min(1.0, value / limit))
    if t >= 0:
        return (int(25 + 230 * t), int(25 * (1 - t)), int(25 * (1 - t)))
    t = -t
    return (int(25 * (1 - t)), int(75 * (1 - t)), int(255 * t))


def make_diff_images(ref: Image.Image, candidate: Image.Image, amp: float) -> dict[str, Image.Image]:
    width, height = ref.size
    ref_px = ref.load()
    cand_px = candidate.load()
    rgb_diff = Image.new("RGB", ref.size)
    luma_heat = Image.new("RGB", ref.size)
    sat_heat = Image.new("RGB", ref.size)
    edge_heat = Image.new("RGB", ref.size)
    dpx = rgb_diff.load()
    lpx = luma_heat.load()
    spx = sat_heat.load()
    epx = edge_heat.load()

    for y in range(height):
        for x in range(width):
            r = ref_px[x, y]
            c = cand_px[x, y]
            dpx[x, y] = tuple(max(0, min(255, int(128 + (c[i] - r[i]) * amp))) for i in range(3))
            lpx[x, y] = color_heat((luma_rgb(c) - luma_rgb(r)) * 255.0, 24.0)
            sr = saturation_f(tuple(v / 255.0 for v in r))
            sc = saturation_f(tuple(v / 255.0 for v in c))
            spx[x, y] = color_heat(sc - sr, 0.18)
            if x + 1 < width and y + 1 < height:
                yr = luma_rgb(r)
                yc = luma_rgb(c)
                er = abs(luma_rgb(ref_px[x + 1, y]) - yr) + abs(luma_rgb(ref_px[x, y + 1]) - yr)
                ec = abs(luma_rgb(cand_px[x + 1, y]) - yc) + abs(luma_rgb(cand_px[x, y + 1]) - yc)
                epx[x, y] = color_heat((ec - er) * 255.0, 10.0)
            else:
                epx[x, y] = (25, 25, 25)

    return {
        "Reference": ref,
        "Candidate": candidate,
        f"RGB Diff x{amp:g}": rgb_diff,
        "Luma Diff": luma_heat,
        "Saturation Diff": sat_heat,
        "Edge Diff": edge_heat,
    }


def labeled_panel(label: str, image: Image.Image, width: int) -> Image.Image:
    scale = width / image.width
    panel = image.resize((width, int(image.height * scale)), Image.Resampling.LANCZOS)
    label_h = 34
    out = Image.new("RGB", (panel.width, panel.height + label_h), (15, 15, 15))
    out.paste(panel, (0, label_h))
    draw = ImageDraw.Draw(out)
    font = ImageFont.load_default()
    draw.text((10, 11), label, fill=(235, 235, 235), font=font)
    return out


def make_contact_sheet(images: dict[str, Image.Image], panel_width: int = 520) -> Image.Image:
    panels = [labeled_panel(label, image, panel_width) for label, image in images.items()]
    cols = 2
    rows = math.ceil(len(panels) / cols)
    gap = 12
    width = cols * panel_width + (cols + 1) * gap
    row_h = max(p.height for p in panels)
    height = rows * row_h + (rows + 1) * gap
    sheet = Image.new("RGB", (width, height), (0, 0, 0))
    for idx, panel in enumerate(panels):
        x = gap + (idx % cols) * (panel_width + gap)
        y = gap + (idx // cols) * (row_h + gap)
        sheet.paste(panel, (x, y))
    return sheet


def main() -> None:
    parser = argparse.ArgumentParser(description="Compare a reference render with a candidate render.")
    parser.add_argument("reference", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--out", type=Path, help="Optional labeled diff contact sheet path.")
    parser.add_argument("--step", type=int, default=3, help="Sampling stride for metrics.")
    parser.add_argument("--diff-amp", type=float, default=4.0, help="Amplification for RGB diff view.")
    parser.add_argument("--region", action="append", default=[], help="Named region as name:x1,y1,x2,y2 in reference pixels.")
    args = parser.parse_args()

    ref = Image.open(args.reference).convert("RGB")
    candidate = Image.open(args.candidate).convert("RGB")
    if candidate.size != ref.size:
        candidate = candidate.resize(ref.size, Image.Resampling.LANCZOS)

    print(f"reference={args.reference} size={ref.size[0]}x{ref.size[1]}")
    print(f"candidate={args.candidate} resized_to_reference={candidate.size[0]}x{candidate.size[1]}")
    print(f"overall {collect_stats(ref, candidate, max(1, args.step)).summary()}")

    print("\ntone bins by reference luma:")
    for label, stats in tone_bins(ref, candidate, max(1, args.step)):
        print(f"  {label} {stats.summary()}")

    if args.region:
        print("\nregions:")
        for region in args.region:
            name, box = parse_region(region)
            print(f"  {name} {collect_stats(ref, candidate, max(1, args.step), box).summary()}")

    if args.out:
        sheet = make_contact_sheet(make_diff_images(ref, candidate, args.diff_amp))
        args.out.parent.mkdir(parents=True, exist_ok=True)
        sheet.save(args.out)
        print(f"\nwrote {args.out}")


if __name__ == "__main__":
    main()
