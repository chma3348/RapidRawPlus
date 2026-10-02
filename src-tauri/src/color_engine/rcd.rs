//! RCD, Ratio Corrected Demosaicing (Luis Sanz Rodríguez, 2017), the
//! default demosaic of darktable and RawTherapee, for 2x2 RGB Bayer sensors.
//! It replaces PPG, which left a fine maze of false detail at the pixel level
//! (on a full-size Sony RAW, 1.2-1.45x Lightroom's energy at the finest
//! scale) and is weaker on fine colour edges.
//!
//! Directions are chosen from colour-difference high-pass filters (vertical
//! against horizontal for green, the two diagonals for red and blue at
//! blue and red), and each missing value is a gradient-weighted blend of
//! neighbouring estimates, corrected by the ratio of a low-pass of the
//! mosaic (green) or taken as colour differences (red and blue).
//!
//! Worked in 256-pixel tiles with a 9-pixel apron, in parallel; the mosaic
//! is mirrored at the frame's edges (period two, so the colour pattern is
//! kept), so every output pixel gets the full algorithm.
use rawler::cfa::CFA;
use rawler::imgop::Rect;
use rawler::pixarray::{Color2D, PixF32};
use rayon::prelude::*;

const TILE: usize = 256;

/// A finished tile: where it goes, its size, its pixels.
struct Done {
    at: (usize, usize),
    size: (usize, usize),
    pixels: Vec<[f32; 3]>,
}
const APRON: usize = 9;
const EPS: f32 = 1e-5;
const EPS_SQ: f32 = 1e-10;

/// Whether RCD applies to this pattern: a 2x2 RGB Bayer.
pub fn supports(cfa: &CFA) -> bool {
    cfa.is_rgb() && cfa.width == 2 && cfa.height == 2
}

/// Demosaic the region `roi` of `pixels` (one sample per site, laid out by
/// `cfa`), as rawler's demosaicers do: the result is `roi`'s size.
pub fn demosaic(pixels: &PixF32, cfa: &CFA, roi: Rect) -> Color2D<f32, 3> {
    assert!(
        supports(cfa),
        "RCD needs a 2x2 RGB Bayer pattern, got {cfa}"
    );
    let cfa = cfa.shift(roi.x(), roi.y());
    let (w, h) = (roi.width(), roi.height());
    let stride = pixels.dim().w;
    let data = pixels.pixels();
    let (x0, y0) = (roi.x(), roi.y());
    // A sample of the region at (row, col), mirrored beyond its edges.
    let mirror = |v: isize, n: usize| -> usize {
        let n = n as isize;
        let mut v = v;
        if v < 0 {
            v = -v;
        }
        if v >= n {
            v = 2 * (n - 1) - v;
        }
        v.clamp(0, n - 1) as usize
    };
    let sample = |r: isize, c: isize| -> f32 {
        data[(y0 + mirror(r, h)) * stride + x0 + mirror(c, w)].max(0.0)
    };
    let colour = |r: isize, c: isize| -> usize {
        cfa.color_at(r.rem_euclid(2) as usize, c.rem_euclid(2) as usize)
    };

    let tiles: Vec<(usize, usize)> = (0..h.div_ceil(TILE))
        .flat_map(|ty| (0..w.div_ceil(TILE)).map(move |tx| (ty * TILE, tx * TILE)))
        .collect();
    let done: Vec<Done> = tiles
        .par_iter()
        .map(|&(ty, tx)| {
            let core_h = TILE.min(h - ty);
            let core_w = TILE.min(w - tx);
            Done {
                at: (ty, tx),
                size: (core_h, core_w),
                pixels: tile(ty, tx, core_h, core_w, &sample, &colour),
            }
        })
        .collect();
    let mut result = vec![[0.0f32; 3]; w * h];
    for Done {
        at: (ty, tx),
        size: (core_h, core_w),
        pixels,
    } in done
    {
        for r in 0..core_h {
            let dst = (ty + r) * w + tx;
            result[dst..dst + core_w].copy_from_slice(&pixels[r * core_w..(r + 1) * core_w]);
        }
    }
    Color2D::new_with(result, w, h)
}

fn intp(a: f32, b: f32, c: f32) -> f32 {
    a * (b - c) + c
}

/// One tile: rows `ty..ty+core_h`, columns `tx..tx+core_w` of the region,
/// computed with an apron around it.
fn tile(
    ty: usize,
    tx: usize,
    core_h: usize,
    core_w: usize,
    sample: &(impl Fn(isize, isize) -> f32 + Sync),
    colour: &(impl Fn(isize, isize) -> usize + Sync),
) -> Vec<[f32; 3]> {
    let hh = core_h + 2 * APRON;
    let ww = core_w + 2 * APRON;
    let (oy, ox) = (ty as isize - APRON as isize, tx as isize - APRON as isize);
    let n = hh * ww;
    let (w1, w2, w3, w4) = (ww, 2 * ww, 3 * ww, 4 * ww);
    let mut cfa = vec![0.0f32; n];
    let mut col = vec![0u8; n];
    for r in 0..hh {
        for c in 0..ww {
            cfa[r * ww + c] = sample(oy + r as isize, ox + c as isize);
            col[r * ww + c] = colour(oy + r as isize, ox + c as isize) as u8;
        }
    }
    let mut rgb = [vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]];
    for i in 0..n {
        rgb[col[i] as usize][i] = cfa[i];
    }

    // Step 1: vertical against horizontal, from the squared colour-difference
    // high-pass along each.
    let mut buf_a = vec![0.0f32; n];
    let mut buf_b = vec![0.0f32; n];
    for r in 3..hh - 3 {
        for c in 3..ww - 3 {
            let i = r * ww + c;
            let v = (cfa[i - w3] - cfa[i - w1] - cfa[i + w1] + cfa[i + w3])
                - 3.0 * (cfa[i - w2] + cfa[i + w2])
                + 6.0 * cfa[i];
            let hz = (cfa[i - 3] - cfa[i - 1] - cfa[i + 1] + cfa[i + 3])
                - 3.0 * (cfa[i - 2] + cfa[i + 2])
                + 6.0 * cfa[i];
            buf_a[i] = v * v;
            buf_b[i] = hz * hz;
        }
    }
    let mut vh_dir = vec![0.5f32; n];
    for r in 4..hh - 4 {
        for c in 4..ww - 4 {
            let i = r * ww + c;
            let v_stat = (buf_a[i - w1] + buf_a[i] + buf_a[i + w1]).max(EPS_SQ);
            let h_stat = (buf_b[i - 1] + buf_b[i] + buf_b[i + 1]).max(EPS_SQ);
            vh_dir[i] = v_stat / (v_stat + h_stat);
        }
    }

    // Step 2: a low-pass of the mosaic at red and blue sites.
    let mut lpf = vec![0.0f32; n];
    for r in 2..hh - 2 {
        for c in 2..ww - 2 {
            let i = r * ww + c;
            if col[i] == 1 {
                continue;
            }
            lpf[i] = cfa[i]
                + 0.5 * (cfa[i - w1] + cfa[i + w1] + cfa[i - 1] + cfa[i + 1])
                + 0.25 * (cfa[i - w1 - 1] + cfa[i - w1 + 1] + cfa[i + w1 - 1] + cfa[i + w1 + 1]);
        }
    }

    // Step 3: green at red and blue sites.
    for r in 4..hh - 4 {
        for c in 4..ww - 4 {
            let i = r * ww + c;
            if col[i] == 1 {
                continue;
            }
            let n_grad = EPS
                + (cfa[i - w1] - cfa[i + w1]).abs()
                + (cfa[i] - cfa[i - w2]).abs()
                + (cfa[i - w1] - cfa[i - w3]).abs()
                + (cfa[i - w2] - cfa[i - w4]).abs();
            let s_grad = EPS
                + (cfa[i - w1] - cfa[i + w1]).abs()
                + (cfa[i] - cfa[i + w2]).abs()
                + (cfa[i + w1] - cfa[i + w3]).abs()
                + (cfa[i + w2] - cfa[i + w4]).abs();
            let w_grad = EPS
                + (cfa[i - 1] - cfa[i + 1]).abs()
                + (cfa[i] - cfa[i - 2]).abs()
                + (cfa[i - 1] - cfa[i - 3]).abs()
                + (cfa[i - 2] - cfa[i - 4]).abs();
            let e_grad = EPS
                + (cfa[i - 1] - cfa[i + 1]).abs()
                + (cfa[i] - cfa[i + 2]).abs()
                + (cfa[i + 1] - cfa[i + 3]).abs()
                + (cfa[i + 2] - cfa[i + 4]).abs();
            let ratio = |a: f32, b: f32| 1.0 + (a - b) / (EPS + a + b);
            let n_est = cfa[i - w1] * ratio(lpf[i], lpf[i - w2]);
            let s_est = cfa[i + w1] * ratio(lpf[i], lpf[i + w2]);
            let w_est = cfa[i - 1] * ratio(lpf[i], lpf[i - 2]);
            let e_est = cfa[i + 1] * ratio(lpf[i], lpf[i + 2]);
            let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
            let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);
            let central = vh_dir[i];
            let around = 0.25
                * (vh_dir[i - w1 - 1]
                    + vh_dir[i - w1 + 1]
                    + vh_dir[i + w1 - 1]
                    + vh_dir[i + w1 + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            rgb[1][i] = intp(disc, h_est, v_est).max(0.0);
        }
    }

    // Step 4.0-4.1: the two diagonals, from the squared colour-difference
    // high-pass along each.
    for r in 3..hh - 3 {
        for c in 3..ww - 3 {
            let i = r * ww + c;
            let p = (cfa[i - w3 - 3] - cfa[i - w1 - 1] - cfa[i + w1 + 1] + cfa[i + w3 + 3])
                - 3.0 * (cfa[i - w2 - 2] + cfa[i + w2 + 2])
                + 6.0 * cfa[i];
            let q = (cfa[i - w3 + 3] - cfa[i - w1 + 1] - cfa[i + w1 - 1] + cfa[i + w3 - 3])
                - 3.0 * (cfa[i - w2 + 2] + cfa[i + w2 - 2])
                + 6.0 * cfa[i];
            buf_a[i] = p * p;
            buf_b[i] = q * q;
        }
    }
    let mut pq_dir = vec![0.5f32; n];
    for r in 4..hh - 4 {
        for c in 4..ww - 4 {
            let i = r * ww + c;
            if col[i] == 1 {
                continue;
            }
            let p_stat = (buf_a[i - w1 - 1] + buf_a[i] + buf_a[i + w1 + 1]).max(EPS_SQ);
            let q_stat = (buf_b[i - w1 + 1] + buf_b[i] + buf_b[i + w1 - 1]).max(EPS_SQ);
            pq_dir[i] = p_stat / (p_stat + q_stat);
        }
    }

    // Step 4.2: red at blue sites and blue at red sites.
    for r in 4..hh - 4 {
        for cc in 4..ww - 4 {
            let i = r * ww + cc;
            if col[i] == 1 {
                continue;
            }
            let c = 2 - col[i] as usize;
            let central = pq_dir[i];
            let around = 0.25
                * (pq_dir[i - w1 - 1]
                    + pq_dir[i - w1 + 1]
                    + pq_dir[i + w1 - 1]
                    + pq_dir[i + w1 + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            let x = &rgb[c];
            let g = &rgb[1];
            let nw_grad = EPS
                + (x[i - w1 - 1] - x[i + w1 + 1]).abs()
                + (x[i - w1 - 1] - x[i - w3 - 3]).abs()
                + (g[i] - g[i - w2 - 2]).abs();
            let ne_grad = EPS
                + (x[i - w1 + 1] - x[i + w1 - 1]).abs()
                + (x[i - w1 + 1] - x[i - w3 + 3]).abs()
                + (g[i] - g[i - w2 + 2]).abs();
            let sw_grad = EPS
                + (x[i - w1 + 1] - x[i + w1 - 1]).abs()
                + (x[i + w1 - 1] - x[i + w3 - 3]).abs()
                + (g[i] - g[i + w2 - 2]).abs();
            let se_grad = EPS
                + (x[i - w1 - 1] - x[i + w1 + 1]).abs()
                + (x[i + w1 + 1] - x[i + w3 + 3]).abs()
                + (g[i] - g[i + w2 + 2]).abs();
            let nw_est = x[i - w1 - 1] - g[i - w1 - 1];
            let ne_est = x[i - w1 + 1] - g[i - w1 + 1];
            let sw_est = x[i + w1 - 1] - g[i + w1 - 1];
            let se_est = x[i + w1 + 1] - g[i + w1 + 1];
            let p_est = (nw_grad * se_est + se_grad * nw_est) / (nw_grad + se_grad);
            let q_est = (ne_grad * sw_est + sw_grad * ne_est) / (ne_grad + sw_grad);
            let v = (g[i] + intp(disc, q_est, p_est)).max(0.0);
            rgb[c][i] = v;
        }
    }

    // Step 4.3: red and blue at green sites.
    for r in 4..hh - 4 {
        for cc in 4..ww - 4 {
            let i = r * ww + cc;
            if col[i] != 1 {
                continue;
            }
            let central = vh_dir[i];
            let around = 0.25
                * (vh_dir[i - w1 - 1]
                    + vh_dir[i - w1 + 1]
                    + vh_dir[i + w1 - 1]
                    + vh_dir[i + w1 + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            let g1 = rgb[1][i];
            let (n1, s1, w1g, e1) = (rgb[1][i - w1], rgb[1][i + w1], rgb[1][i - 1], rgb[1][i + 1]);
            for c in [0usize, 2] {
                let x = &rgb[c];
                let sn = (x[i - w1] - x[i + w1]).abs();
                let ew = (x[i - 1] - x[i + 1]).abs();
                let n_grad = EPS + (g1 - n1).abs() + sn + (x[i - w1] - x[i - w3]).abs();
                let s_grad = EPS + (g1 - s1).abs() + sn + (x[i + w1] - x[i + w3]).abs();
                let w_grad = EPS + (g1 - w1g).abs() + ew + (x[i - 1] - x[i - 3]).abs();
                let e_grad = EPS + (g1 - e1).abs() + ew + (x[i + 1] - x[i + 3]).abs();
                let n_est = x[i - w1] - n1;
                let s_est = x[i + w1] - s1;
                let w_est = x[i - 1] - w1g;
                let e_est = x[i + 1] - e1;
                let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
                let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
                let v = (g1 + intp(disc, h_est, v_est)).max(0.0);
                rgb[c][i] = v;
            }
        }
    }

    let mut out = Vec::with_capacity(core_h * core_w);
    for r in 0..core_h {
        for c in 0..core_w {
            let i = (r + APRON) * ww + c + APRON;
            out.push([rgb[0][i], rgb[1][i], rgb[2][i]]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawler::imgop::sensor::bayer::Demosaic;
    use rawler::imgop::sensor::bayer::ppg::PPGDemosaic;
    use rawler::imgop::{Dim2, Point};

    // A photo-like scene: demosaicers rely on the channels sharing their
    // texture, as in photographs (a scene whose channels carry unrelated
    // patterns favours none of them and says little).
    use super::tests_support::photo_like as scene;

    fn mosaic(rgb: &[[f32; 3]], w: usize, h: usize, cfa: &CFA) -> PixF32 {
        PixF32::new_with(
            (0..w * h)
                .map(|i| rgb[i][cfa.color_at(i / w, i % w)])
                .collect(),
            w,
            h,
        )
    }

    fn mse(a: &Color2D<f32, 3>, truth: &[[f32; 3]], w: usize, h: usize, margin: usize) -> f64 {
        let mut s = 0.0;
        let mut n = 0.0;
        for y in margin..h - margin {
            for x in margin..w - margin {
                for (got, want) in a.pixels()[y * w + x].iter().zip(truth[y * w + x]) {
                    let d = (got - want) as f64;
                    s += d * d;
                    n += 1.0;
                }
            }
        }
        s / n
    }

    #[test]
    fn rcd_reconstructs_better_than_ppg_on_every_bayer_layout() {
        let (w, h) = (300, 220);
        let truth = scene(w, h);
        for name in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            let cfa = CFA::new(name);
            let pixels = mosaic(&truth, w, h, &cfa);
            let roi = Rect::new(Point::new(0, 0), Dim2::new(w, h));
            let rcd = demosaic(&pixels, &cfa, roi);
            let ppg = PPGDemosaic::new().demosaic(&pixels, &cfa, &Default::default(), roi);
            let (e_rcd, e_ppg) = (mse(&rcd, &truth, w, h, 4), mse(&ppg, &truth, w, h, 4));
            eprintln!("{name}: RCD {e_rcd:.3e}  PPG {e_ppg:.3e}");
            assert!(
                e_rcd < e_ppg,
                "{name}: RCD {e_rcd} not better than PPG {e_ppg}"
            );
        }
    }

    #[test]
    fn flat_and_grey_stay_exact_and_tiles_join_seamlessly() {
        // Across tile boundaries (a frame bigger than one tile, an offset
        // region) a flat grey and a flat colour come back unchanged.
        let (w, h) = (600, 530);
        let cfa = CFA::new("RGGB");
        for value in [[0.2f32, 0.2, 0.2], [0.6, 0.3, 0.1]] {
            let truth = vec![value; w * h];
            let pixels = mosaic(&truth, w, h, &cfa);
            let roi = Rect::new(Point::new(3, 5), Dim2::new(w - 10, h - 12));
            let out = demosaic(&pixels, &cfa, roi);
            assert_eq!(out.dim(), Dim2::new(w - 10, h - 12));
            for p in out.pixels() {
                for c in 0..3 {
                    assert!((p[c] - value[c]).abs() < 1e-4, "{p:?} vs {value:?}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests_support {
    /// Like a photograph: texture shared by every channel (fine stripes at
    /// an angle, rings), colour varying slowly, and a hard colour edge.
    pub fn photo_like(w: usize, h: usize) -> Vec<[f32; 3]> {
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let stripes = 0.5 + 0.5 * ((x * 0.9 + y * 0.35) * 0.55).sin();
                let ring = 0.5 + 0.5 * (((x - 90.0).powi(2) + (y - 70.0).powi(2)) * 0.004).sin();
                let lum = 0.15 + 0.6 * stripes * (0.4 + 0.6 * ring);
                let edge = if x > 0.6 * w as f32 && y < 0.5 * h as f32 {
                    1.0
                } else {
                    0.0
                };
                let chroma = [
                    0.9 + 0.5 * edge,
                    0.85,
                    0.6 + 0.5 * (x / w as f32) - 0.3 * edge,
                ];
                std::array::from_fn(|c| (lum * chroma[c]).min(1.0))
            })
            .collect()
    }
}
