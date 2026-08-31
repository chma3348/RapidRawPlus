//! Panorama finishing: exposure compensation between frames, and
//! trimming the empty canvas the warp leaves around the result.
//!
//! Frames shot in aperture priority (or with any auto exposure) rarely
//! match brightness exactly, and a seam between two differently exposed
//! frames reads as a band across the sky no matter how good the blend is.
//! Compensation estimates one gain per frame from the brightness ratios
//! measured in the overlaps, so the frames agree before they are blended.

use image::Rgb32FImage;

/// Gains are clamped to this range: a wild ratio from a bad overlap must
/// not be able to blow out or black out a frame.
const MIN_GAIN: f64 = 0.25;
const MAX_GAIN: f64 = 4.0;
const SOLVER_ITERATIONS: usize = 200;

/// Median of `b / a` over paired samples. Median rather than mean because
/// overlaps routinely include moving subjects and specular highlights.
pub fn median_ratio(samples: &[(f32, f32)]) -> Option<f64> {
    let mut ratios: Vec<f64> = samples
        .iter()
        .filter(|(a, b)| *a > 0.01 && *b > 0.01 && a.is_finite() && b.is_finite())
        .map(|(a, b)| (*b as f64) / (*a as f64))
        .filter(|r| r.is_finite() && *r > 0.0)
        .collect();
    if ratios.len() < 16 {
        return None;
    }
    let mid = ratios.len() / 2;
    let (_, &mut median, _) = ratios.select_nth_unstable_by(mid, |x, y| x.partial_cmp(y).unwrap());
    Some(median)
}

/// Solves one gain per frame from pairwise overlap ratios.
///
/// Each entry `(i, j, r)` asserts that frame `j` is `r` times brighter
/// than frame `i` where they overlap, i.e. `g_i / g_j = r`. The system is
/// solved in log space by relaxation and normalised so the average gain is
/// 1.0 — the panorama gets consistent, not globally brighter or darker.
pub fn estimate_gains(frame_count: usize, pairs: &[(usize, usize, f64)]) -> Vec<f64> {
    if frame_count == 0 {
        return Vec::new();
    }
    let mut log_gain = vec![0.0f64; frame_count];
    if pairs.is_empty() {
        return vec![1.0; frame_count];
    }

    // Adjacency of log-space constraints: l_i = l_j + delta.
    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); frame_count];
    for (i, j, r) in pairs {
        if *i >= frame_count || *j >= frame_count || !r.is_finite() || *r <= 0.0 {
            continue;
        }
        let ln_r = r.ln();
        adjacency[*i].push((*j, ln_r));
        adjacency[*j].push((*i, -ln_r));
    }

    // Gauss-Seidel (in-place updates). Jacobi iteration oscillates
    // indefinitely on a bipartite constraint graph — which a simple chain
    // of frames is — and settles on the wrong relative solution.
    for _ in 0..SOLVER_ITERATIONS {
        for idx in 0..frame_count {
            if adjacency[idx].is_empty() {
                continue;
            }
            let sum: f64 = adjacency[idx]
                .iter()
                .map(|(j, delta)| log_gain[*j] + delta)
                .sum();
            log_gain[idx] = sum / adjacency[idx].len() as f64;
        }
    }

    let mean: f64 = log_gain.iter().sum::<f64>() / frame_count as f64;
    log_gain
        .iter()
        .map(|l| (l - mean).exp().clamp(MIN_GAIN, MAX_GAIN))
        .collect()
}

/// Multiplies an image by a gain in place.
pub fn apply_gain(img: &mut Rgb32FImage, gain: f64) {
    if (gain - 1.0).abs() < 1e-4 {
        return;
    }
    let g = gain as f32;
    for p in img.pixels_mut() {
        for c in 0..3 {
            p[c] *= g;
        }
    }
}

/// Bounds of the written area, found by trimming border rows and columns
/// that the warp never touched. Deliberately conservative: only *exactly*
/// unwritten (all-zero) lines are trimmed, so real scene content — however
/// dark — is never cropped away.
pub fn trim_empty_borders(img: &Rgb32FImage) -> (u32, u32, u32, u32) {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return (0, 0, w, h);
    }
    let row_empty = |y: u32| (0..w).all(|x| img.get_pixel(x, y).0.iter().all(|v| *v <= 0.0));
    let col_empty = |x: u32| (0..h).all(|y| img.get_pixel(x, y).0.iter().all(|v| *v <= 0.0));

    let mut top = 0;
    while top < h && row_empty(top) {
        top += 1;
    }
    if top == h {
        return (0, 0, w, h); // fully empty: leave it alone
    }
    let mut bottom = h - 1;
    while bottom > top && row_empty(bottom) {
        bottom -= 1;
    }
    let mut left = 0;
    while left < w && col_empty(left) {
        left += 1;
    }
    let mut right = w - 1;
    while right > left && col_empty(right) {
        right -= 1;
    }
    (left, top, right - left + 1, bottom - top + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn median_ratio_is_robust_to_outliers() {
        let mut samples: Vec<(f32, f32)> = (0..64).map(|_| (0.4, 0.8)).collect();
        // A moving subject and a specular hit in the overlap.
        samples.push((0.4, 8.0));
        samples.push((0.4, 0.001));
        let r = median_ratio(&samples).expect("ratio");
        assert!((r - 2.0).abs() < 0.05, "median ratio {r:.3} should be ~2.0");
    }

    #[test]
    fn ignores_too_few_samples() {
        let samples: Vec<(f32, f32)> = (0..5).map(|_| (0.4, 0.8)).collect();
        assert!(median_ratio(&samples).is_none());
    }

    /// Three frames whose true relative brightness is 1 : 2 : 0.5 must be
    /// recovered up to a global scale.
    #[test]
    fn solver_recovers_relative_gains() {
        let truth = [1.0f64, 2.0, 0.5];
        // r_ij = brightness_j / brightness_i, so g_i/g_j = r_ij.
        let pairs = vec![
            (0usize, 1usize, truth[1] / truth[0]),
            (1usize, 2usize, truth[2] / truth[1]),
        ];
        let gains = estimate_gains(3, &pairs);
        // Corrected brightness must be equal across frames.
        let corrected: Vec<f64> = truth.iter().zip(&gains).map(|(b, g)| b * g).collect();
        let first = corrected[0];
        for (idx, value) in corrected.iter().enumerate() {
            assert!(
                (value / first - 1.0).abs() < 0.02,
                "frame {idx} corrected to {value:.3}, expected ~{first:.3} (gains {gains:?})"
            );
        }
        let mean_gain: f64 = gains.iter().sum::<f64>() / gains.len() as f64;
        assert!(
            (mean_gain - 1.0).abs() < 0.35,
            "gains should stay centred on 1.0, got mean {mean_gain:.3}"
        );
    }

    #[test]
    fn solver_is_identity_without_pairs() {
        assert_eq!(estimate_gains(3, &[]), vec![1.0, 1.0, 1.0]);
    }

    #[test]
    fn trims_unwritten_canvas_only() {
        let mut img = Rgb32FImage::new(40, 30);
        // Written region with a deliberately BLACK pixel inside it: real
        // content must survive the trim.
        for y in 5..25 {
            for x in 8..32 {
                img.put_pixel(x, y, Rgb([0.4, 0.4, 0.4]));
            }
        }
        img.put_pixel(10, 10, Rgb([0.0, 0.0, 0.0]));
        let (x, y, w, h) = trim_empty_borders(&img);
        assert_eq!((x, y, w, h), (8, 5, 24, 20));
    }

    #[test]
    fn leaves_a_full_canvas_untouched() {
        let img = Rgb32FImage::from_pixel(10, 10, Rgb([0.2, 0.2, 0.2]));
        assert_eq!(trim_empty_borders(&img), (0, 0, 10, 10));
    }
}
