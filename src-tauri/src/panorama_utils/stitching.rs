//! Panorama compositing: every frame warped onto the reference frame's
//! plane, then laid down one at a time over what is already there.
//!
//! Where a frame overlaps the panorama so far, the two are cut along the
//! seam that disturbs least, and joined across it by multi-band blending.
//! The seam is searched for where the two agree in colour *and* in
//! structure (edges, texture) over a neighbourhood, and may swing as far
//! sideways as it needs, so it keeps clear of what parallax has misplaced —
//! a near boat against a far mountain — rather than slicing through a
//! uniform white hull whose outline is offset. The blend switches fine
//! detail sharply at the cut (no ghosts) and broad tone over a wide band
//! (no brightness step down the middle of the picture).

use crate::panorama_stitching::ImageInfo;
use image::{GrayImage, Rgb, Rgb32FImage};
use nalgebra::{Matrix3, Point3};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;
use tauri::{AppHandle, Emitter};

/// The seam is searched on a grid at most this many cells on a side.
const SEAM_GRID: u32 = 2048;
/// Weight of the structure (gradient) difference against the colour
/// difference in the seam's cost: an edge one frame has and the other has
/// a few pixels over is what a cut must not cross.
const STRUCTURE_WEIGHT: f32 = 2.0;
/// Disagreements are weighed against the brightness where they are plus
/// this (encoded values).
const SHADOW_FLOOR: f32 = 0.1;
/// The coarser scale disagreement is also measured at (canvas pixels), and
/// its weight against the finer.
const COARSE_SCALE: f64 = 16.0;
const COARSE_WEIGHT: f32 = 4.0;
/// How far, in canvas pixels, a seam keeps from a disagreement.
const SEAM_CLEARANCE: f64 = 24.0;
/// Cost of a cell outside the overlap: the seam goes there only if the
/// overlap leaves it no way round.
const BLOCKED: f32 = 1.0e6;
/// Bands of the multi-band blend: the coarsest carries tone across a band
/// of about 2^(BANDS + 2) pixels. More would smooth tone wider, but also
/// mix more of a misplaced near object into its surroundings (a ghost);
/// exposure is already matched between frames before they are blended.
const BANDS: usize = 5;
/// Where one frame's own edge ends inside the other (a handheld pan's
/// frames step up and down), there is nothing of it beyond the edge to
/// blend with: within this many pixels of the edge, its broad tone is drawn
/// to the other's instead, so the edge leaves no line across a sky.
const EDGE_REACH: f32 = 96.0;
/// The grid (pixels) that tone difference is measured on.
const TONE_CELL: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: u32,
    y0: u32,
    /// Exclusive.
    x1: u32,
    /// Exclusive.
    y1: u32,
}

impl Rect {
    fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

struct Canvas {
    offset_x: f64,
    offset_y: f64,
    width: u32,
    height: u32,
}

/// A frame warped onto the canvas, kept to its bounding box.
struct Warped {
    rect: Rect,
    rgb: Vec<f32>,
    valid: Vec<bool>,
}

impl Warped {
    /// The index of canvas pixel (x, y) in the frame, if the frame covers it.
    fn at(&self, x: u32, y: u32) -> Option<usize> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let i =
            (y - self.rect.y0) as usize * self.rect.width() as usize + (x - self.rect.x0) as usize;
        self.valid[i].then_some(i)
    }
}

/// Where a frame is cut against the panorama: across the overlap, the
/// frame owns one side of a line running down it (vertical) or across it.
struct Seam {
    vertical: bool,
    frame_after: bool,
    /// The canvas row (vertical) or column `line[0]` is for.
    start: u32,
    /// The cut's canvas column (vertical) or row, along the overlap.
    line: Vec<f64>,
}

impl Seam {
    fn frame_owns(&self, x: u32, y: u32) -> bool {
        let (along, across) = if self.vertical { (y, x) } else { (x, y) };
        let i = (along.saturating_sub(self.start) as usize).min(self.line.len() - 1);
        if self.frame_after {
            across as f64 >= self.line[i]
        } else {
            (across as f64) < self.line[i]
        }
    }
}

pub fn progressive_seam_stitcher(
    images: &[&ImageInfo],
    global_homographies: &HashMap<usize, Matrix3<f64>>,
    app_handle: AppHandle,
) -> Rgb32FImage {
    if images.is_empty() {
        return Rgb32FImage::new(0, 0);
    }

    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for &img_info in images {
        let h = global_homographies[&img_info.id];
        let (w, h_img) = img_info.image.dimensions();
        let corners = [
            Point3::new(0.0, 0.0, 1.0),
            Point3::new(w as f64, 0.0, 1.0),
            Point3::new(w as f64, h_img as f64, 1.0),
            Point3::new(0.0, h_img as f64, 1.0),
        ];
        for p in corners.iter() {
            let tp = h * p;
            let tx = tp.x / tp.z;
            let ty = tp.y / tp.z;
            min_x = min_x.min(tx);
            max_x = max_x.max(tx);
            min_y = min_y.min(ty);
            max_y = max_y.max(ty);
        }
    }

    let canvas = Canvas {
        offset_x: -min_x,
        offset_y: -min_y,
        width: (max_x - min_x).ceil() as u32,
        height: (max_y - min_y).ceil() as u32,
    };
    println!("  - Output canvas size: {}x{}", canvas.width, canvas.height);

    let mut panorama = Rgb32FImage::new(canvas.width, canvas.height);
    let mut panorama_mask = GrayImage::new(canvas.width, canvas.height);

    for (i, &info) in images.iter().enumerate() {
        let name = Path::new(&info.filename)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let _ = app_handle.emit(
            "panorama-progress",
            format!("Stitching image {} of {}: {}", i + 1, images.len(), name),
        );
        println!("  - Compositing '{}'", name);
        let Some(warped) = warp_frame(&info.image, &global_homographies[&info.id], &canvas) else {
            println!("    - Lands off the canvas; left out.");
            continue;
        };
        add_frame(&mut panorama, &mut panorama_mask, &warped);
    }

    panorama
}

/// The canvas rectangle a frame lands on, if any of it is on the canvas.
fn footprint(h: &Matrix3<f64>, (w, h_img): (u32, u32), canvas: &Canvas) -> Option<Rect> {
    let (mut x0, mut y0, mut x1, mut y1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (cx, cy) in [
        (0.0, 0.0),
        (w as f64, 0.0),
        (w as f64, h_img as f64),
        (0.0, h_img as f64),
    ] {
        let p = h * Point3::new(cx, cy, 1.0);
        let (tx, ty) = (p.x / p.z + canvas.offset_x, p.y / p.z + canvas.offset_y);
        x0 = x0.min(tx);
        y0 = y0.min(ty);
        x1 = x1.max(tx);
        y1 = y1.max(ty);
    }
    let rect = Rect {
        x0: x0.floor().max(0.0) as u32,
        y0: y0.floor().max(0.0) as u32,
        x1: (x1.ceil().max(0.0) as u32).min(canvas.width),
        y1: (y1.ceil().max(0.0) as u32).min(canvas.height),
    };
    (rect.x1 > rect.x0 && rect.y1 > rect.y0).then_some(rect)
}

fn warp_frame(img: &Rgb32FImage, h: &Matrix3<f64>, canvas: &Canvas) -> Option<Warped> {
    let rect = footprint(h, img.dimensions(), canvas)?;
    let h_inv = h.try_inverse()?;
    let (iw, ih) = (img.width() as f64, img.height() as f64);
    let w = rect.width() as usize;
    let mut rgb = vec![0f32; w * rect.height() as usize * 3];
    let mut valid = vec![false; w * rect.height() as usize];
    rgb.par_chunks_mut(w * 3)
        .zip(valid.par_chunks_mut(w))
        .enumerate()
        .for_each(|(row, (rgb_row, valid_row))| {
            let y = (rect.y0 as usize + row) as f64 - canvas.offset_y;
            for col in 0..w {
                let x = (rect.x0 as usize + col) as f64 - canvas.offset_x;
                let p = h_inv * Point3::new(x, y, 1.0);
                let (sx, sy) = (p.x / p.z, p.y / p.z);
                if sx >= 0.0 && sx < iw && sy >= 0.0 && sy < ih {
                    let colour = get_interpolated_pixel(img, sx, sy);
                    rgb_row[col * 3..col * 3 + 3].copy_from_slice(&colour.0);
                    valid_row[col] = true;
                }
            }
        });
    Some(Warped { rect, rgb, valid })
}

/// Lays a frame over the panorama: cut and blended where they overlap,
/// as it is where it is the first to cover the canvas.
fn add_frame(pano: &mut Rgb32FImage, pano_mask: &mut GrayImage, frame: &Warped) {
    let blended = overlap_bounds(pano_mask, frame).map(|overlap| {
        let seam = find_seam(pano, pano_mask, frame, overlap);
        blend_across(pano, pano_mask, frame, &seam, overlap)
    });

    // The frame's pixels nothing covered yet (those near the overlap were
    // set by the blend), and its coverage.
    let width = pano.width() as usize;
    let rect = frame.rect;
    pano.par_chunks_mut(width * 3)
        .zip(pano_mask.par_chunks_mut(width))
        .enumerate()
        .skip(rect.y0 as usize)
        .take(rect.height() as usize)
        .for_each(|(y, (row, mask_row))| {
            let y = y as u32;
            for x in rect.x0..rect.x1 {
                let Some(i) = frame.at(x, y) else {
                    continue;
                };
                let xs = x as usize;
                if mask_row[xs] == 0 && !blended.is_some_and(|b| b.contains(x, y)) {
                    row[xs * 3..xs * 3 + 3].copy_from_slice(&frame.rgb[i * 3..i * 3 + 3]);
                }
                mask_row[xs] = 255;
            }
        });
}

/// The bounding box of where the frame and the panorama so far overlap.
fn overlap_bounds(pano_mask: &GrayImage, frame: &Warped) -> Option<Rect> {
    let width = pano_mask.width() as usize;
    let mask = pano_mask.as_raw();
    let rect = frame.rect;
    let rows: Vec<(u32, u32, u32)> = (rect.y0..rect.y1)
        .into_par_iter()
        .filter_map(|y| {
            let mut span: Option<(u32, u32)> = None;
            for x in rect.x0..rect.x1 {
                if mask[y as usize * width + x as usize] > 0 && frame.at(x, y).is_some() {
                    span = Some(span.map_or((x, x), |(lo, _)| (lo, x)));
                }
            }
            span.map(|(lo, hi)| (y, lo, hi))
        })
        .collect();
    let first = rows.first()?;
    let last = rows.last()?;
    Some(Rect {
        x0: rows.iter().map(|r| r.1).min()?,
        y0: first.0,
        x1: rows.iter().map(|r| r.2).max()? + 1,
        y1: last.0 + 1,
    })
}

/// The cut between the panorama and a frame across their overlap: the
/// cheapest path through a cost that is high wherever the two disagree in
/// colour or structure anywhere nearby.
fn find_seam(pano: &Rgb32FImage, pano_mask: &GrayImage, frame: &Warped, ov: Rect) -> Seam {
    // The frame lies after the overlap (to its right or below) or before.
    let dx = (frame.rect.x0 + frame.rect.x1) as f64 / 2.0 - (ov.x0 + ov.x1) as f64 / 2.0;
    let dy = (frame.rect.y0 + frame.rect.y1) as f64 / 2.0 - (ov.y0 + ov.y1) as f64 / 2.0;
    let vertical = dx.abs() > dy.abs();
    let frame_after = if vertical { dx > 0.0 } else { dy > 0.0 };

    let cell = ov.width().max(ov.height()).div_ceil(SEAM_GRID).max(1);
    let gw = ov.width().div_ceil(cell) as usize;
    let gh = ov.height().div_ceil(cell) as usize;
    let width = pano.width() as usize;
    let (pano_px, mask) = (pano.as_raw(), pano_mask.as_raw());

    // Both images averaged over each cell they both cover all of.
    let cells: Vec<Option<([f32; 3], [f32; 3])>> = (0..gw * gh)
        .into_par_iter()
        .map(|i| {
            let x0 = ov.x0 + (i % gw) as u32 * cell;
            let y0 = ov.y0 + (i / gw) as u32 * cell;
            let (mut a, mut b, mut n) = ([0f32; 3], [0f32; 3], 0f32);
            for y in y0..(y0 + cell).min(ov.y1) {
                for x in x0..(x0 + cell).min(ov.x1) {
                    let p = y as usize * width + x as usize;
                    let f = frame.at(x, y)?;
                    if mask[p] == 0 {
                        return None;
                    }
                    for c in 0..3 {
                        a[c] += pano_px[p * 3 + c];
                        b[c] += frame.rgb[f * 3 + c];
                    }
                    n += 1.0;
                }
            }
            Some((a.map(|v| v / n), b.map(|v| v / n)))
        })
        .collect();

    // Disagreement at the cells' own scale, and at a coarser one: blurred,
    // a stochastic texture (rippled water, leaves) differs little between
    // frames, while structure parallax has moved (a hull's edge 30 px
    // lower in one frame) still does — and a cut across it shows.
    let coarse_radius = (COARSE_SCALE / cell as f64).ceil() as usize;
    let blurred = {
        let planes: Vec<Vec<f32>> = (0..6)
            .into_par_iter()
            .map(|k| {
                let plane: Vec<f32> = cells
                    .iter()
                    .map(|c| c.map_or(f32::NAN, |(a, b)| if k < 3 { a[k] } else { b[k - 3] }))
                    .collect();
                mean_valid(&plane, gw, gh, coarse_radius)
            })
            .collect();
        (0..gw * gh)
            .map(|i| {
                cells[i].map(|_| {
                    (
                        [planes[0][i], planes[1][i], planes[2][i]],
                        [planes[3][i], planes[4][i], planes[5][i]],
                    )
                })
            })
            .collect::<Vec<_>>()
    };
    let fine = disagreement(&cells, gw, gh, 1);
    let coarse = disagreement(&blurred, gw, gh, coarse_radius.max(1) as isize);
    let energy: Vec<f32> = fine
        .iter()
        .zip(&coarse)
        .map(|(f, c)| f + COARSE_WEIGHT * c)
        .collect();

    // Kept clear of every disagreement by the clearance, and smoothed so a
    // stray cell counts for less than a patch.
    let radius = (SEAM_CLEARANCE / cell as f64).ceil() as usize;
    let energy = filter_valid(&energy, gw, gh, radius, f32::max);
    let energy = mean_valid(&energy, gw, gh, radius);
    // Squared, so a short stretch of plain disagreement (a moved person,
    // an edge offset by parallax) outweighs a long one of faint texture
    // mismatch (leaves stirring, rippled water), which no one sees.
    let energy: Vec<f32> = energy.iter().map(|e| e * e).collect();

    let mut sorted: Vec<f32> = energy.iter().copied().filter(|e| !e.is_nan()).collect();
    let median = if sorted.is_empty() {
        0.0
    } else {
        let mid = sorted.len() / 2;
        *sorted.select_nth_unstable_by(mid, f32::total_cmp).1
    };
    // A little for every cell crossed, so a seam through featureless sky
    // runs straight; and a ramp up to the overlap's sides, so it keeps off
    // the frames' edges, where they are softest and most vignetted.
    let step_cost = (0.1 * median).max(1e-4);
    let edge_cost = 2.0 * median + 10.0 * step_cost;

    // The cut runs along `rows`, one position across (`cols`) for each.
    let (rows, cols) = if vertical { (gh, gw) } else { (gw, gh) };
    let index = |row: usize, col: usize| {
        if vertical {
            row * gw + col
        } else {
            col * gw + row
        }
    };
    let mut cost = vec![BLOCKED; rows * cols];
    cost.par_chunks_mut(cols)
        .enumerate()
        .for_each(|(row, line)| {
            let valid = |col: usize| !energy[index(row, col)].is_nan();
            let (Some(lo), Some(hi)) = (
                (0..cols).find(|&c| valid(c)),
                (0..cols).rfind(|&c| valid(c)),
            ) else {
                return;
            };
            let ramp = ((hi - lo) / 8).max(1) as f32;
            for (col, out) in line.iter_mut().enumerate().take(hi + 1).skip(lo) {
                if valid(col) {
                    let side = (col - lo).min(hi - col) as f32;
                    *out = energy[index(row, col)]
                        + step_cost
                        + edge_cost * (1.0 - side / ramp).max(0.0);
                }
            }
        });
    let cut = cheapest_cut(&cost, cols, rows);

    // Back on the canvas: cell centres, interpolated along the cut.
    let (start, length, across0) = if vertical {
        (ov.y0, ov.height(), ov.x0)
    } else {
        (ov.x0, ov.width(), ov.y0)
    };
    let line: Vec<f64> = (0..length)
        .map(|a| {
            let g = ((a as f64 + 0.5) / cell as f64 - 0.5).clamp(0.0, (rows - 1) as f64);
            let (i, t) = (g.floor() as usize, g.fract());
            let j = (i + 1).min(rows - 1);
            let col = cut[i] as f64 * (1.0 - t) + cut[j] as f64 * t;
            across0 as f64 + (col + 0.5) * cell as f64
        })
        .collect();
    let (lo, hi) = line.iter().fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(lo, hi): (f64, f64), &v| (lo.min(v), hi.max(v)),
    );
    println!(
        "    - {} seam at {} {:.0}..{:.0} across the overlap ({}..{}), the new frame {}.",
        if vertical { "Vertical" } else { "Horizontal" },
        if vertical { "x" } else { "y" },
        lo,
        hi,
        if vertical { ov.x0 } else { ov.y0 },
        if vertical { ov.x1 } else { ov.y1 },
        match (vertical, frame_after) {
            (true, true) => "right of it",
            (true, false) => "left of it",
            (false, true) => "below it",
            (false, false) => "above it",
        }
    );
    Seam {
        vertical,
        frame_after,
        start,
        line,
    }
}

/// How much two images (`cells`: both, averaged per cell) disagree at each
/// cell: in colour, and in structure — the difference of their gradients
/// over `step` cells (an edge in one, a few pixels over in the other) —
/// relative to how bright it is there, since a mismatch in the shadows
/// shows as plainly once they are lifted, as RAW panoramas' usually are.
/// NaN where they do not both cover the cell.
fn disagreement(
    cells: &[Option<([f32; 3], [f32; 3])>],
    gw: usize,
    gh: usize,
    step: isize,
) -> Vec<f32> {
    (0..gw * gh)
        .into_par_iter()
        .map(|i| {
            let Some((a, b)) = cells[i] else {
                return f32::NAN;
            };
            let (cx, cy) = ((i % gw) as isize, (i / gw) as isize);
            let at = |x: isize, y: isize| {
                if x < 0 || y < 0 || x >= gw as isize || y >= gh as isize {
                    None
                } else {
                    cells[y as usize * gw + x as usize]
                }
            };
            let structure = |(sx, sy): (isize, isize)| -> f32 {
                let lo = at(cx - sx, cy - sy).unwrap_or((a, b));
                let hi = at(cx + sx, cy + sy).unwrap_or((a, b));
                (0..3)
                    .map(|c| ((hi.0[c] - lo.0[c]) - (hi.1[c] - lo.1[c])).abs())
                    .sum::<f32>()
                    * 0.5
            };
            let colour: f32 = (0..3).map(|c| (a[c] - b[c]).abs()).sum();
            let level = (0..3).map(|c| a[c] + b[c]).sum::<f32>() / 6.0;
            (colour + STRUCTURE_WEIGHT * (structure((step, 0)) + structure((0, step))))
                / (level + SHADOW_FLOOR)
        })
        .collect()
}

/// The cheapest path down a cost grid (`cols` wide, row-major): one
/// position per row, free to run sideways within a row as far as it likes,
/// paying for every cell it crosses, so it can swing wide round an obstacle
/// rather than crossing it a pixel a row at most.
fn cheapest_cut(cost: &[f32], cols: usize, rows: usize) -> Vec<usize> {
    // `came_from[row][col]`: the column the path came down from the row above.
    let mut came_from = vec![0u32; cols * rows];
    let mut total: Vec<f64> = cost[..cols].iter().map(|&c| c as f64).collect();
    for (i, from) in came_from[..cols].iter_mut().enumerate() {
        *from = i as u32;
    }
    let mut next = vec![0f64; cols];
    for row in 1..rows {
        let row_cost = &cost[row * cols..(row + 1) * cols];
        let from = &mut came_from[row * cols..(row + 1) * cols];
        for col in 0..cols {
            next[col] = total[col] + row_cost[col] as f64;
            from[col] = col as u32;
        }
        for col in 1..cols {
            let sideways = next[col - 1] + row_cost[col] as f64;
            if sideways < next[col] {
                next[col] = sideways;
                from[col] = from[col - 1];
            }
        }
        for col in (0..cols - 1).rev() {
            let sideways = next[col + 1] + row_cost[col] as f64;
            if sideways < next[col] {
                next[col] = sideways;
                from[col] = from[col + 1];
            }
        }
        std::mem::swap(&mut total, &mut next);
    }
    let mut col = (0..cols)
        .min_by(|&a, &b| total[a].total_cmp(&total[b]))
        .unwrap_or(0);
    let mut cut = vec![0; rows];
    for row in (0..rows).rev() {
        cut[row] = col;
        col = came_from[row * cols + col] as usize;
    }
    cut
}

/// A separable window filter over the cells that are not NaN (NaN cells
/// stay NaN and are skipped).
fn filter_valid(grid: &[f32], w: usize, h: usize, r: usize, pick: fn(f32, f32) -> f32) -> Vec<f32> {
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![f32::NAN; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                if src[y * w + x].is_nan() {
                    continue;
                }
                let (n, at) = if horizontal { (w, x) } else { (h, y) };
                let mut acc = f32::NAN;
                for k in at.saturating_sub(r)..(at + r + 1).min(n) {
                    let v = if horizontal {
                        src[y * w + k]
                    } else {
                        src[k * w + x]
                    };
                    if !v.is_nan() {
                        acc = if acc.is_nan() { v } else { pick(acc, v) };
                    }
                }
                *out = acc;
            }
        });
        out
    };
    pass(&pass(grid, true), false)
}

/// The mean over a (2r+1)² window of the cells that are not NaN.
fn mean_valid(grid: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], horizontal: bool| -> Vec<f32> {
        let mut out = vec![f32::NAN; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                if src[y * w + x].is_nan() {
                    continue;
                }
                let (n, at) = if horizontal { (w, x) } else { (h, y) };
                let (mut sum, mut count) = (0f32, 0f32);
                for k in at.saturating_sub(r)..(at + r + 1).min(n) {
                    let v = if horizontal {
                        src[y * w + k]
                    } else {
                        src[k * w + x]
                    };
                    if !v.is_nan() {
                        sum += v;
                        count += 1.0;
                    }
                }
                *out = sum / count;
            }
        });
        out
    };
    pass(&pass(grid, true), false)
}

/// The mean over a (2r+1)² window of the cells that are not NaN, at every
/// cell with any in reach (filling holes and edges); NaN elsewhere.
fn fill_mean(grid: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    // Sums and counts, separably, so a hole's window is still the square.
    let pass = |src: &[(f32, f32)], horizontal: bool| -> Vec<(f32, f32)> {
        let mut out = vec![(0f32, 0f32); w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, out) in row.iter_mut().enumerate() {
                let (n, at) = if horizontal { (w, x) } else { (h, y) };
                for k in at.saturating_sub(r)..(at + r + 1).min(n) {
                    let (s, c) = if horizontal {
                        src[y * w + k]
                    } else {
                        src[k * w + x]
                    };
                    out.0 += s;
                    out.1 += c;
                }
            }
        });
        out
    };
    let start: Vec<(f32, f32)> = grid
        .iter()
        .map(|&v| if v.is_nan() { (0.0, 0.0) } else { (v, 1.0) })
        .collect();
    pass(&pass(&start, true), false)
        .into_iter()
        .map(|(s, c)| if c > 0.0 { s / c } else { f32::NAN })
        .collect()
}

/// Joins the frame to the panorama across the seam by multi-band blending,
/// within the overlap and a margin round it; returns that region.
///
/// Each image is filled, where it has nothing, from the other, so the
/// blend has no empty canvas to pull in, and leaves either image as it was
/// wherever it is the only one.
fn blend_across(
    pano: &mut Rgb32FImage,
    pano_mask: &GrayImage,
    frame: &Warped,
    seam: &Seam,
    ov: Rect,
) -> Rect {
    let (width, height) = pano.dimensions();
    let margin = 4u32 << BANDS;
    let region = Rect {
        x0: ov.x0.saturating_sub(margin),
        y0: ov.y0.saturating_sub(margin),
        x1: (ov.x1 + margin).min(width),
        y1: (ov.y1 + margin).min(height),
    };
    let (rw, rh) = (region.width() as usize, region.height() as usize);
    let levels = ((rw.min(rh) as f64).log2() as usize)
        .saturating_sub(2)
        .min(BANDS);
    let mask = pano_mask.as_raw();
    let pixel = |i: usize| {
        let (x, y) = (region.x0 + (i % rw) as u32, region.y0 + (i / rw) as u32);
        let p = y as usize * width as usize + x as usize;
        (x, y, p, mask[p] > 0, frame.at(x, y))
    };

    // The frame's share: all of what it alone covers, its side of the seam
    // where both do.
    let share: Vec<f32> = (0..rw * rh)
        .into_par_iter()
        .map(|i| match pixel(i) {
            (_, _, _, _, None) => 0.0,
            (_, _, _, false, Some(_)) => 1.0,
            (x, y, _, true, Some(_)) => seam.frame_owns(x, y) as u8 as f32,
        })
        .collect();
    let shares = gaussian_pyramid(
        Plane {
            w: rw,
            h: rh,
            px: share,
        },
        levels,
    );

    // How far each pixel is from where only the panorama goes on, and
    // where only the frame does; and their difference in broad tone where
    // both cover.
    let on_pano: Vec<bool> = (0..rw * rh).into_par_iter().map(|i| pixel(i).3).collect();
    let on_frame: Vec<bool> = (0..rw * rh)
        .into_par_iter()
        .map(|i| pixel(i).4.is_some())
        .collect();
    let (frame_reach, pano_reach) = rayon::join(
        || {
            let not_pano_only: Vec<bool> = on_pano
                .iter()
                .zip(&on_frame)
                .map(|(&p, &f)| !p || f)
                .collect();
            distance_out(&not_pano_only, rw, rh)
        },
        || {
            let not_frame_only: Vec<bool> = on_pano
                .iter()
                .zip(&on_frame)
                .map(|(&p, &f)| p || !f)
                .collect();
            distance_out(&not_frame_only, rw, rh)
        },
    );
    let tone = tone_difference(pano, frame, region, &on_pano, &on_frame);
    let edge_weight = |d: f32| {
        let t = (1.0 - d / EDGE_REACH).max(0.0);
        t * t * (3.0 - 2.0 * t)
    };

    for c in 0..3 {
        let pano_px = pano.as_raw();
        let (a, b): (Vec<f32>, Vec<f32>) = (0..rw * rh)
            .into_par_iter()
            .map(|i| {
                let (x, y, p, on_pano, on_frame) = pixel(i);
                let mut from_pano = on_pano.then(|| pano_px[p * 3 + c]);
                let mut from_frame = on_frame.map(|f| frame.rgb[f * 3 + c]);
                if let (Some(a), Some(b)) = (from_pano.as_mut(), from_frame.as_mut()) {
                    // Near where the one that owns the pixel ends and only
                    // the other goes on, its tone meets the other's.
                    let d = tone.at(i % rw, i / rw, c);
                    if seam.frame_owns(x, y) {
                        *b -= edge_weight(frame_reach[i]) * d;
                    } else {
                        *a += edge_weight(pano_reach[i]) * d;
                    }
                }
                (
                    from_pano.or(from_frame).unwrap_or(0.0),
                    from_frame.or(from_pano).unwrap_or(0.0),
                )
            })
            .unzip();
        let out = blend_bands(
            Plane {
                w: rw,
                h: rh,
                px: a,
            },
            Plane {
                w: rw,
                h: rh,
                px: b,
            },
            &shares,
        );
        let pano_px: &mut [f32] = pano;
        pano_px
            .par_chunks_mut(width as usize * 3)
            .skip(region.y0 as usize)
            .take(rh)
            .enumerate()
            .for_each(|(row, line)| {
                for col in 0..rw {
                    let (x, y) = (region.x0 + col as u32, region.y0 + row as u32);
                    if mask[y as usize * width as usize + x as usize] > 0
                        || frame.at(x, y).is_some()
                    {
                        line[x as usize * 3 + c] = out.px[row * rw + col].max(0.0);
                    }
                }
            });
    }
    region
}

/// The distance, in pixels, from each pixel of a `w` × `h` region to the
/// nearest one that is not `inside` (two-pass chamfer). Beyond the region
/// counts as inside.
fn distance_out(inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    const DIAGONAL: f32 = std::f32::consts::SQRT_2;
    let mut d: Vec<f32> = inside
        .iter()
        .map(|&v| if v { f32::MAX / 2.0 } else { 0.0 })
        .collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut v = d[i];
            if x > 0 {
                v = v.min(d[i - 1] + 1.0);
            }
            if y > 0 {
                v = v.min(d[i - w] + 1.0);
                if x > 0 {
                    v = v.min(d[i - w - 1] + DIAGONAL);
                }
                if x + 1 < w {
                    v = v.min(d[i - w + 1] + DIAGONAL);
                }
            }
            d[i] = v;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            let mut v = d[i];
            if x + 1 < w {
                v = v.min(d[i + 1] + 1.0);
            }
            if y + 1 < h {
                v = v.min(d[i + w] + 1.0);
                if x + 1 < w {
                    v = v.min(d[i + w + 1] + DIAGONAL);
                }
                if x > 0 {
                    v = v.min(d[i + w - 1] + DIAGONAL);
                }
            }
            d[i] = v;
        }
    }
    d
}

/// The frame's broad difference in tone from the panorama (frame minus
/// panorama, per channel) over a region, measured where both cover it on a
/// coarse grid, smoothed, and carried a little past where they both do.
struct Tone {
    w: usize,
    h: usize,
    planes: [Vec<f32>; 3],
}

impl Tone {
    /// At region pixel (x, y), interpolated between grid cells.
    fn at(&self, x: usize, y: usize, c: usize) -> f32 {
        let gx = ((x as f32 + 0.5) / TONE_CELL as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
        let gy = ((y as f32 + 0.5) / TONE_CELL as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (gx as usize, gy as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (tx, ty) = (gx - x0 as f32, gy - y0 as f32);
        let p = &self.planes[c];
        let v = |x: usize, y: usize| {
            let v = p[y * self.w + x];
            if v.is_nan() { 0.0 } else { v }
        };
        let top = v(x0, y0) * (1.0 - tx) + v(x1, y0) * tx;
        let bottom = v(x0, y1) * (1.0 - tx) + v(x1, y1) * tx;
        top * (1.0 - ty) + bottom * ty
    }
}

fn tone_difference(
    pano: &Rgb32FImage,
    frame: &Warped,
    region: Rect,
    on_pano: &[bool],
    on_frame: &[bool],
) -> Tone {
    let (rw, rh) = (region.width() as usize, region.height() as usize);
    let (tw, th) = (rw.div_ceil(TONE_CELL), rh.div_ceil(TONE_CELL));
    let width = pano.width() as usize;
    let pano_px = pano.as_raw();
    let cells: Vec<Option<[f32; 3]>> = (0..tw * th)
        .into_par_iter()
        .map(|i| {
            let (cx, cy) = ((i % tw) * TONE_CELL, (i / tw) * TONE_CELL);
            let (mut sum, mut n) = ([0f32; 3], 0f32);
            for y in cy..(cy + TONE_CELL).min(rh) {
                for x in cx..(cx + TONE_CELL).min(rw) {
                    let r = y * rw + x;
                    if !(on_pano[r] && on_frame[r]) {
                        continue;
                    }
                    let (gx, gy) = (region.x0 + x as u32, region.y0 + y as u32);
                    let p = gy as usize * width + gx as usize;
                    let Some(f) = frame.at(gx, gy) else {
                        continue;
                    };
                    for c in 0..3 {
                        sum[c] += frame.rgb[f * 3 + c] - pano_px[p * 3 + c];
                    }
                    n += 1.0;
                }
            }
            (n > 0.0).then(|| sum.map(|v| v / n))
        })
        .collect();
    let radius = (EDGE_REACH as usize).div_ceil(TONE_CELL);
    let planes = [0, 1, 2].map(|c| {
        let plane: Vec<f32> = cells.iter().map(|v| v.map_or(f32::NAN, |v| v[c])).collect();
        fill_mean(&plane, tw, th, radius)
    });
    Tone {
        w: tw,
        h: th,
        planes,
    }
}

/// One level of an image pyramid: a single channel.
struct Plane {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

/// Halves a plane with the 5-tap binomial kernel.
fn reduce(p: &Plane) -> Plane {
    let (w2, h2) = (p.w.div_ceil(2), p.h.div_ceil(2));
    let tap = |at: &dyn Fn(isize) -> f32, i: isize| {
        (at(i - 2) + 4.0 * at(i - 1) + 6.0 * at(i) + 4.0 * at(i + 1) + at(i + 2)) / 16.0
    };
    let mut across = vec![0f32; w2 * p.h];
    across.par_chunks_mut(w2).enumerate().for_each(|(y, row)| {
        let src = &p.px[y * p.w..(y + 1) * p.w];
        let at = |i: isize| src[i.clamp(0, p.w as isize - 1) as usize];
        for (x, out) in row.iter_mut().enumerate() {
            *out = tap(&at, 2 * x as isize);
        }
    });
    let mut px = vec![0f32; w2 * h2];
    px.par_chunks_mut(w2).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let at = |i: isize| across[i.clamp(0, p.h as isize - 1) as usize * w2 + x];
            *out = tap(&at, 2 * y as isize);
        }
    });
    Plane { w: w2, h: h2, px }
}

/// Doubles a plane back up to `w` × `h` (the inverse step of `reduce`).
fn expand(p: &Plane, w: usize, h: usize) -> Plane {
    let tap = |at: &dyn Fn(isize) -> f32, i: usize| {
        let half = (i / 2) as isize;
        if i.is_multiple_of(2) {
            (at(half - 1) + 6.0 * at(half) + at(half + 1)) / 8.0
        } else {
            (at(half) + at(half + 1)) / 2.0
        }
    };
    let mut across = vec![0f32; w * p.h];
    across.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &p.px[y * p.w..(y + 1) * p.w];
        let at = |i: isize| src[i.clamp(0, p.w as isize - 1) as usize];
        for (x, out) in row.iter_mut().enumerate() {
            *out = tap(&at, x);
        }
    });
    let mut px = vec![0f32; w * h];
    px.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let at = |i: isize| across[i.clamp(0, p.h as isize - 1) as usize * w + x];
            *out = tap(&at, y);
        }
    });
    Plane { w, h, px }
}

fn gaussian_pyramid(base: Plane, levels: usize) -> Vec<Plane> {
    let mut pyramid = vec![base];
    for _ in 0..levels {
        let next = reduce(pyramid.last().unwrap());
        pyramid.push(next);
    }
    pyramid
}

/// Blends `a` into `b` band by band, each band weighted by the matching
/// level of `b`'s share (`shares`, a Gaussian pyramid of its weight).
fn blend_bands(a: Plane, b: Plane, shares: &[Plane]) -> Plane {
    let levels = shares.len() - 1;
    let mut ga = gaussian_pyramid(a, levels);
    let mut gb = gaussian_pyramid(b, levels);
    let mix = |out: &mut [f32], a: &[f32], b: &[f32], share: &[f32]| {
        out.par_iter_mut()
            .zip(a.par_iter().zip(b.par_iter()).zip(share.par_iter()))
            .for_each(|(o, ((&a, &b), &s))| *o += a + s * (b - a));
    };
    let top = &shares[levels];
    let mut out = Plane {
        w: top.w,
        h: top.h,
        px: vec![0.0; top.w * top.h],
    };
    mix(&mut out.px, &ga[levels].px, &gb[levels].px, &top.px);
    for k in (0..levels).rev() {
        let (coarse_a, coarse_b) = (ga.pop().unwrap(), gb.pop().unwrap());
        let (w, h) = (shares[k].w, shares[k].h);
        let up_a = expand(&coarse_a, w, h);
        let up_b = expand(&coarse_b, w, h);
        // This level's band of each: what the coarser level does not hold.
        let band_a: Vec<f32> = ga[k]
            .px
            .par_iter()
            .zip(&up_a.px)
            .map(|(g, u)| g - u)
            .collect();
        let band_b: Vec<f32> = gb[k]
            .px
            .par_iter()
            .zip(&up_b.px)
            .map(|(g, u)| g - u)
            .collect();
        out = expand(&out, w, h);
        mix(&mut out.px, &band_a, &band_b, &shares[k].px);
    }
    out
}

fn get_interpolated_pixel(img: &Rgb32FImage, x: f64, y: f64) -> Rgb<f32> {
    let (width, height) = img.dimensions();
    let x_floor = x.floor() as u32;
    let y_floor = y.floor() as u32;
    if x_floor + 1 >= width || y_floor + 1 >= height || x < 0.0 || y < 0.0 {
        return *img.get_pixel(
            x.max(0.0).min(width as f64 - 1.0) as u32,
            y.max(0.0).min(height as f64 - 1.0) as u32,
        );
    }
    let dx = x - x_floor as f64;
    let dy = y - y_floor as f64;
    let p00 = img.get_pixel(x_floor, y_floor);
    let p10 = img.get_pixel(x_floor + 1, y_floor);
    let p01 = img.get_pixel(x_floor, y_floor + 1);
    let p11 = img.get_pixel(x_floor + 1, y_floor + 1);
    let mut final_pixel = [0.0; 3];
    for i in 0..3 {
        let c00 = p00[i] as f64;
        let c10 = p10[i] as f64;
        let c01 = p01[i] as f64;
        let c11 = p11[i] as f64;
        let top = c00 * (1.0 - dx) + c10 * dx;
        let bottom = c01 * (1.0 - dx) + c11 * dx;
        final_pixel[i] = top * (1.0 - dy) + bottom * dy;
    }
    Rgb([
        final_pixel[0] as f32,
        final_pixel[1] as f32,
        final_pixel[2] as f32,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Plane {
        Plane {
            w,
            h,
            px: (0..w * h).map(|i| f(i % w, i / w)).collect(),
        }
    }

    #[test]
    fn blend_leaves_each_side_as_it_was() {
        let (w, h) = (201, 61);
        let a = plane(w, h, |x, y| ((x * 7 + y * 3) % 11) as f32 / 11.0);
        let b = plane(w, h, |x, y| ((x * 5 + y * 13) % 17) as f32 / 17.0 + 0.3);
        let shares = gaussian_pyramid(plane(w, h, |x, _| (x >= 100) as u8 as f32), 3);
        let out = blend_bands(
            Plane {
                w,
                h,
                px: a.px.clone(),
            },
            Plane {
                w,
                h,
                px: b.px.clone(),
            },
            &shares,
        );
        for y in 0..h {
            // Far from the cut each side is its own image, exactly.
            assert!((out.px[y * w] - a.px[y * w]).abs() < 1e-4);
            assert!((out.px[y * w + w - 1] - b.px[y * w + w - 1]).abs() < 1e-4);
        }
    }

    #[test]
    fn identical_images_blend_to_themselves() {
        let (w, h) = (64, 40);
        let a = plane(w, h, |x, y| ((x * 31 + y * 17) % 23) as f32 / 23.0);
        let shares = gaussian_pyramid(plane(w, h, |x, y| ((x + y) % 2) as f32), 3);
        let out = blend_bands(
            Plane {
                w,
                h,
                px: a.px.clone(),
            },
            Plane {
                w,
                h,
                px: a.px.clone(),
            },
            &shares,
        );
        for (o, v) in out.px.iter().zip(&a.px) {
            assert!((o - v).abs() < 1e-5);
        }
    }

    #[test]
    fn seam_keeps_clear_of_a_misplaced_object() {
        // Two frames of one smooth scene, overlapping across x 200..400,
        // with a near object parallax has placed 10 px apart in them.
        let (w, h) = (600u32, 300u32);
        let scene =
            |x: u32, y: u32| 0.3 + 0.2 * (x as f32 / w as f32) + 0.1 * (y as f32 / h as f32);
        let object =
            |x: u32, y: u32, left: u32| (left..left + 40).contains(&x) && (100..200).contains(&y);
        let mut pano = Rgb32FImage::new(w, h);
        let mut mask = GrayImage::new(w, h);
        for y in 0..h {
            for x in 0..400 {
                let v = if object(x, y, 280) { 0.9 } else { scene(x, y) };
                pano.put_pixel(x, y, Rgb([v, v * 0.8, v * 0.6]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }
        let rect = Rect {
            x0: 200,
            y0: 0,
            x1: w,
            y1: h,
        };
        let mut rgb = Vec::new();
        for y in 0..h {
            for x in 200..w {
                let v = if object(x, y, 290) { 0.9 } else { scene(x, y) };
                rgb.extend([v, v * 0.8, v * 0.6]);
            }
        }
        let frame = Warped {
            rect,
            rgb,
            valid: vec![true; (400 * h) as usize],
        };
        let overlap = overlap_bounds(&mask, &frame).unwrap();
        assert_eq!((overlap.x0, overlap.x1), (200, 400));
        let seam = find_seam(&pano, &mask, &frame, overlap);
        assert!(seam.vertical && seam.frame_after);
        for y in 100..200 {
            let x = seam.line[(y - seam.start) as usize];
            assert!(
                !(275.0..335.0).contains(&x),
                "row {y}: cut at x {x}, through the object"
            );
        }
    }

    #[test]
    fn a_frame_edge_inside_the_panorama_leaves_no_step() {
        // The panorama so far covers x 0..300; a brighter frame covers
        // x 100..400 but only from row 120 down, its top edge crossing
        // the panorama.
        let (w, h) = (400u32, 300u32);
        let mut pano = Rgb32FImage::new(w, h);
        let mut mask = GrayImage::new(w, h);
        for y in 0..h {
            for x in 0..300 {
                pano.put_pixel(x, y, Rgb([0.5; 3]));
                mask.put_pixel(x, y, image::Luma([255]));
            }
        }
        let rect = Rect {
            x0: 100,
            y0: 120,
            x1: w,
            y1: h,
        };
        let n = (rect.width() * rect.height()) as usize;
        let frame = Warped {
            rect,
            rgb: vec![0.55; n * 3],
            valid: vec![true; n],
        };
        add_frame(&mut pano, &mut mask, &frame);
        let at = |x, y| pano.get_pixel(x, y)[0];
        // Just inside the frame's edge it meets the panorama's tone...
        assert!(
            (at(290, 120) - at(290, 119)).abs() < 0.005,
            "{} vs {}",
            at(290, 120),
            at(290, 119)
        );
        // ...and well inside it, it is itself.
        assert!((at(290, 290) - 0.55).abs() < 0.005, "{}", at(290, 290));
        assert!((at(380, 200) - 0.55).abs() < 1e-6);
    }

    #[test]
    fn cut_swings_round_an_obstacle() {
        // A wall of cost across the middle rows, with a gap far to the
        // right: the cut must reach the gap, many columns in one row.
        let (cols, rows) = (40, 30);
        let mut cost = vec![1.0f32; cols * rows];
        for row in 12..18 {
            for col in 0..cols {
                if col != 35 {
                    cost[row * cols + col] = 1000.0;
                }
            }
        }
        let cut = cheapest_cut(&cost, cols, rows);
        assert!(cut[12..18].iter().all(|&col| col == 35));
    }
}
