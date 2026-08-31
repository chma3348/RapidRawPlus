//! Gradient-domain healing (Poisson seamless cloning).
//!
//! A clone stamp copies source pixels verbatim, so any tone difference
//! between the source and its new surroundings shows up as a hard-edged
//! blob — measured at a 49/255 step on a real edit, which no amount of
//! global tone matching closed.
//!
//! Healing keeps the source's *texture* but takes its *tone* from the
//! destination. Writing the result as `source + c`, we want the gradients
//! of the result to equal the gradients of the source (so `c` is harmonic,
//! `∇²c = 0`) while the result matches the destination on the mask
//! boundary (so `c = destination − source` there). Solving that Dirichlet
//! problem and adding `c` back gives a patch whose seam disappears.
//!
//! `c` is harmonic and therefore very smooth, so a coarse-to-fine solve is
//! both accurate and cheap: relax on a small grid, upsample as the initial
//! guess for the next level, relax again.

use image::{GrayImage, RgbaImage};

/// Relaxation sweeps per pyramid level.
const ITERS: usize = 60;
/// Pyramid depth. Six levels reduce a ~650px region to ~10px, small enough
/// for relaxation to propagate boundary information all the way across.
const LEVELS: usize = 6;
/// Context ring kept around the mask so the solve has boundary values.
const PAD: i64 = 8;

/// Averages 2x2 blocks. Used for both the boundary data and the mask.
fn downsample(v: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let nw = (w / 2).max(1);
    let nh = (h / 2).max(1);
    let mut out = vec![0.0f32; nw * nh];
    for y in 0..nh {
        let y0 = (2 * y).min(h - 1);
        let y1 = (2 * y + 1).min(h - 1);
        for x in 0..nw {
            let x0 = (2 * x).min(w - 1);
            let x1 = (2 * x + 1).min(w - 1);
            out[y * nw + x] =
                0.25 * (v[y0 * w + x0] + v[y0 * w + x1] + v[y1 * w + x0] + v[y1 * w + x1]);
        }
    }
    (out, nw, nh)
}

fn upsample(v: &[f32], w: usize, h: usize, tw: usize, th: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; tw * th];
    for y in 0..th {
        let fy = ((y as f32 + 0.5) * h as f32 / th as f32 - 0.5).max(0.0);
        let y0 = (fy.floor() as usize).min(h - 1);
        let y1 = (y0 + 1).min(h - 1);
        let wy = fy - y0 as f32;
        for x in 0..tw {
            let fx = ((x as f32 + 0.5) * w as f32 / tw as f32 - 0.5).max(0.0);
            let x0 = (fx.floor() as usize).min(w - 1);
            let x1 = (x0 + 1).min(w - 1);
            let wx = fx - x0 as f32;
            let a = v[y0 * w + x0] * (1.0 - wx) + v[y0 * w + x1] * wx;
            let b = v[y1 * w + x0] * (1.0 - wx) + v[y1 * w + x1] * wx;
            out[y * tw + x] = a * (1.0 - wy) + b * wy;
        }
    }
    out
}

/// Solves `∇²c = 0` inside the mask with `c = diff` outside it.
fn solve_correction(diff: &[f32], inside_in: &[bool], w: usize, h: usize, level: usize) -> Vec<f32> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    // The relaxation sweep below skips the outermost ring of cells. A ring
    // cell marked interior would therefore never be written by the sweep
    // *nor* pinned to its boundary value — it would keep its stale initial
    // value (zero at the coarsest level) and then leak that zero inward as
    // the pyramid unwinds, which is exactly what left a 67/255 seam. Force
    // the ring to be boundary at every level so every cell is either
    // relaxed or pinned.
    let mut inside = inside_in.to_vec();
    for x in 0..w {
        inside[x] = false;
        inside[(h - 1) * w + x] = false;
    }
    for y in 0..h {
        inside[y * w] = false;
        inside[y * w + w - 1] = false;
    }
    let inside = inside;

    let mut c = if level > 0 && w > 8 && h > 8 {
        let (coarse_diff, cw, ch) = downsample(diff, w, h);
        let inside_f: Vec<f32> = inside.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect();
        let (coarse_inside_f, _, _) = downsample(&inside_f, w, h);
        // A coarse cell counts as interior only when all four of its
        // children are, so coarsening can never swallow the boundary.
        let coarse_inside: Vec<bool> = coarse_inside_f.iter().map(|v| *v > 0.99).collect();
        let coarse = solve_correction(&coarse_diff, &coarse_inside, cw, ch, level - 1);
        upsample(&coarse, cw, ch, w, h)
    } else {
        vec![0.0f32; w * h]
    };

    let pin = |c: &mut Vec<f32>| {
        for i in 0..w * h {
            if !inside[i] {
                c[i] = diff[i];
            }
        }
    };
    pin(&mut c);

    let mut next = c.clone();
    for _ in 0..ITERS {
        for y in 1..h.saturating_sub(1) {
            for x in 1..w.saturating_sub(1) {
                let i = y * w + x;
                if inside[i] {
                    next[i] = 0.25 * (c[i - w] + c[i + w] + c[i - 1] + c[i + 1]);
                }
            }
        }
        std::mem::swap(&mut c, &mut next);
        pin(&mut c);
    }
    c
}

/// Heals `mask` from `offset`, returning a copy of `image` with the masked
/// region replaced by seamlessly blended source content.
pub fn heal_blend(image: &RgbaImage, mask: &GrayImage, offset_x: i32, offset_y: i32) -> RgbaImage {
    let (w, h) = image.dimensions();
    let mut out = image.clone();
    if offset_x == 0 && offset_y == 0 {
        return out;
    }

    // Work on the mask's bounding box plus a ring of context.
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for y in 0..h {
        for x in 0..w {
            if mask.get_pixel(x, y)[0] > 0 {
                x0 = x0.min(x as i64);
                y0 = y0.min(y as i64);
                x1 = x1.max(x as i64);
                y1 = y1.max(y as i64);
            }
        }
    }
    if x0 == i64::MAX {
        return out;
    }
    x0 = (x0 - PAD).max(0);
    y0 = (y0 - PAD).max(0);
    x1 = (x1 + PAD).min(w as i64 - 1);
    y1 = (y1 + PAD).min(h as i64 - 1);
    let bw = (x1 - x0 + 1) as usize;
    let bh = (y1 - y0 + 1) as usize;

    let at = |x: i64, y: i64| {
        let cx = x.clamp(0, w as i64 - 1) as u32;
        let cy = y.clamp(0, h as i64 - 1) as u32;
        *image.get_pixel(cx, cy)
    };

    let mut inside = vec![false; bw * bh];
    let mut alpha = vec![0.0f32; bw * bh];
    for iy in 0..bh {
        for ix in 0..bw {
            let m = mask.get_pixel((x0 + ix as i64) as u32, (y0 + iy as i64) as u32)[0];
            let i = iy * bw + ix;
            alpha[i] = m as f32 / 255.0;
            // Border cells are always treated as boundary so the relaxation
            // (which skips the outermost ring) always has data to pull from.
            let on_border = ix == 0 || iy == 0 || ix == bw - 1 || iy == bh - 1;
            inside[i] = m > 127 && !on_border;
        }
    }

    let mut src = vec![[0.0f32; 3]; bw * bh];
    let mut diff = vec![[0.0f32; 3]; bw * bh];
    for iy in 0..bh {
        for ix in 0..bw {
            let x = x0 + ix as i64;
            let y = y0 + iy as i64;
            let d = at(x, y);
            let s = at(x + offset_x as i64, y + offset_y as i64);
            let i = iy * bw + ix;
            for ch in 0..3 {
                src[i][ch] = s[ch] as f32;
                diff[i][ch] = d[ch] as f32 - s[ch] as f32;
            }
        }
    }

    for ch in 0..3 {
        let plane: Vec<f32> = diff.iter().map(|d| d[ch]).collect();
        let c = solve_correction(&plane, &inside, bw, bh, LEVELS);
        for iy in 0..bh {
            for ix in 0..bw {
                let i = iy * bw + ix;
                let a = alpha[i];
                if a <= 0.0 {
                    continue;
                }
                let healed = src[i][ch] + c[i];
                let px = out.get_pixel_mut((x0 + ix as i64) as u32, (y0 + iy as i64) as u32);
                let base = px[ch] as f32;
                px[ch] = (base * (1.0 - a) + healed * a).clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgba};

    /// Deterministic value noise so the "texture" is real structure rather
    /// than a flat fill the solver could trivially match.
    fn texture(x: u32, y: u32) -> f32 {
        let v = ((x as f32 * 12.9898 + y as f32 * 78.233).sin() * 43758.547).fract();
        v.abs() * 30.0
    }

    /// Left half is dark, right half is bright, both carry the same grain.
    /// Cloning right-to-left must not leave a step at the mask edge.
    fn fixture() -> (RgbaImage, GrayImage) {
        let mut img = RgbaImage::new(200, 120);
        for y in 0..120 {
            for x in 0..200 {
                let tone = if x < 100 { 80.0 } else { 180.0 };
                let v = (tone + texture(x, y)).clamp(0.0, 255.0) as u8;
                img.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        let mut mask = GrayImage::new(200, 120);
        for y in 40..80 {
            for x in 30..70 {
                mask.put_pixel(x, y, Luma([255]));
            }
        }
        (img, mask)
    }

    fn ring_step(img: &RgbaImage, mask: &GrayImage) -> f32 {
        let mut inner = (0.0f32, 0usize);
        let mut outer = (0.0f32, 0usize);
        for y in 0..img.height() {
            for x in 0..img.width() {
                let v = img.get_pixel(x, y)[0] as f32;
                let m = mask.get_pixel(x, y)[0] > 127;
                let near = (28..72).contains(&x) && (38..82).contains(&y);
                if m && (30..34).contains(&x) {
                    inner.0 += v;
                    inner.1 += 1;
                } else if !m && near && (26..30).contains(&x) {
                    outer.0 += v;
                    outer.1 += 1;
                }
            }
        }
        (inner.0 / inner.1 as f32 - outer.0 / outer.1 as f32).abs()
    }

    #[test]
    fn heal_removes_the_tone_step_a_raw_copy_leaves() {
        let (img, mask) = fixture();
        // Source is 100px to the right: bright content into a dark hole.
        let healed = heal_blend(&img, &mask, 100, 0);
        let step = ring_step(&healed, &mask);
        assert!(
            step < 6.0,
            "healed seam step {step:.2} should be near zero (raw copy leaves ~100)"
        );
    }

    #[test]
    fn heal_keeps_the_destination_tone_not_the_source_tone() {
        let (img, mask) = fixture();
        let healed = heal_blend(&img, &mask, 100, 0);
        let mut sum = 0.0;
        let mut n = 0;
        for y in 45..75 {
            for x in 35..65 {
                sum += healed.get_pixel(x, y)[0] as f32;
                n += 1;
            }
        }
        let mean = sum / n as f32;
        // Destination tone is ~80+grain; the source it copied from is ~180.
        assert!(
            (mean - 95.0).abs() < 20.0,
            "healed mean {mean:.1} should sit near the destination tone (~95), not the source (~195)"
        );
    }

    #[test]
    fn heal_preserves_source_texture() {
        let (img, mask) = fixture();
        let healed = heal_blend(&img, &mask, 100, 0);
        let mut vals = Vec::new();
        for y in 45..75 {
            for x in 35..65 {
                vals.push(healed.get_pixel(x, y)[0] as f32);
            }
        }
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        let std = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt();
        // A flat paste-over would have std ~0; the grain is ~30 wide.
        assert!(std > 4.0, "texture std {std:.2} — source grain was flattened away");
    }

    #[test]
    fn heal_leaves_unmasked_pixels_untouched() {
        let (img, mask) = fixture();
        let healed = heal_blend(&img, &mask, 100, 0);
        for y in 0..img.height() {
            for x in 0..img.width() {
                if mask.get_pixel(x, y)[0] == 0 {
                    assert_eq!(
                        img.get_pixel(x, y),
                        healed.get_pixel(x, y),
                        "pixel ({x},{y}) outside the mask was modified"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_offset_is_a_no_op() {
        let (img, mask) = fixture();
        assert_eq!(heal_blend(&img, &mask, 0, 0), img);
    }
}
