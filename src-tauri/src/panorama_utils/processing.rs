use crate::panorama_stitching::{BRIEF_DESCRIPTOR_SIZE, Descriptor, Feature, KeyPoint, Match};
use image::{GrayImage, ImageBuffer, Luma};
use imageproc::corners::{Corner, corners_fast9};
use imageproc::filter::gaussian_blur_f32;
use nalgebra::{Matrix3, Point2};
use rand::prelude::*;
use rayon::prelude::*;

const MAX_PROCESSING_DIMENSION: u32 = 1600;
/// FAST's threshold, on frames whose tones have been normalized (see
/// `normalize_tones`), so it means the same on a dark RAW as on a JPEG.
const FAST_THRESHOLD: u8 = 10;
const NON_MAXIMA_SUPPRESSION_RADIUS: f32 = 6.0;
/// The most features kept per frame, spread over a grid of cells so one
/// busy patch (foliage) cannot take them all from the sky's horizon.
const MAX_FEATURES: usize = 3000;
const FEATURE_GRID: u32 = 16;
const BRIEF_PATCH_SIZE: u32 = 32;
const MATCH_RATIO_THRESHOLD: f32 = 0.8;
const RANSAC_ITERATIONS: usize = 2500;
const RANSAC_INLIER_THRESHOLD: f64 = 5.0;
pub const MIN_INLIERS_FOR_CONNECTION: usize = 15;
/// A pair's inliers must span at least this share of the frame's width
/// and of its height: a tight cluster fits any warp at all.
const MIN_INLIER_SPREAD: f64 = 0.1;

pub fn calculate_downscale_dimensions(width: u32, height: u32) -> (u32, u32, f64) {
    let long_side = width.max(height);
    if long_side <= MAX_PROCESSING_DIMENSION {
        return (width, height, 1.0);
    }
    let scale_factor = long_side as f64 / MAX_PROCESSING_DIMENSION as f64;
    let new_width = (width as f64 / scale_factor).round() as u32;
    let new_height = (height as f64 / scale_factor).round() as u32;
    (new_width, new_height, scale_factor)
}

/// Stretch a frame's tones over the full range, its median to mid-grey,
/// before corners are found. FAST's threshold is an absolute difference in
/// levels, so a RAW developed dark (or a frame of deep shade) offered a
/// tenth of the corners a camera JPEG did — too few to match. BRIEF only
/// compares pairs of pixels, which no monotone stretch changes.
pub fn normalize_tones(img: &GrayImage) -> GrayImage {
    let mut histogram = [0u64; 256];
    for p in img.pixels() {
        histogram[p[0] as usize] += 1;
    }
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return img.clone();
    }
    let level_at = |fraction: f64| {
        let target = (total as f64 * fraction) as u64;
        let mut seen = 0;
        for (level, count) in histogram.iter().enumerate() {
            seen += count;
            if seen > target {
                return level as f64;
            }
        }
        255.0
    };
    let (low, high) = (level_at(0.005), level_at(0.995));
    if high - low < 1.0 {
        return img.clone();
    }
    let stretch = |v: f64| ((v - low) / (high - low)).clamp(0.0, 1.0);
    let median = stretch(level_at(0.5)).clamp(0.01, 0.99);
    let gamma = (0.5f64.ln() / median.ln()).clamp(0.3, 2.0);
    let lut: Vec<u8> = (0..256)
        .map(|v| (stretch(v as f64).powf(gamma) * 255.0).round() as u8)
        .collect();
    let mut out = img.clone();
    for p in out.pixels_mut() {
        p[0] = lut[p[0] as usize];
    }
    out
}

pub fn find_features(img: &GrayImage, brief_pairs: &[(Point2<i32>, Point2<i32>)]) -> Vec<Feature> {
    let normalized = normalize_tones(img);
    let blurred_img_u8 = imageproc::filter::gaussian_blur_f32(&normalized, 1.5);
    let corners = corners_fast9(&blurred_img_u8, FAST_THRESHOLD);
    let keypoints = select_keypoints(
        &corners,
        img.width(),
        img.height(),
        NON_MAXIMA_SUPPRESSION_RADIUS,
    );
    let blurred_img_f32 = gaussian_blur_f32(&convert_gray_u8_to_f32(&normalized), 2.0);
    let features: Vec<Feature> = keypoints
        .par_iter()
        .filter_map(|kp| {
            compute_brief_descriptor(&blurred_img_f32, kp, BRIEF_PATCH_SIZE, brief_pairs).map(
                |descriptor| Feature {
                    keypoint: *kp,
                    descriptor,
                },
            )
        })
        .collect();
    features
}

/// The strongest corners at least `radius` apart, at most `MAX_FEATURES`
/// shared evenly between the cells of a grid (a cell with few corners
/// leaves its share to none: the sky simply has fewer).
fn select_keypoints(corners: &[Corner], width: u32, height: u32, radius: f32) -> Vec<KeyPoint> {
    let mut sorted_corners = corners.to_vec();
    sorted_corners.sort_by(|a, b| b.score.total_cmp(&a.score));
    let radius_sq = radius * radius;
    // Kept corners bucketed by `radius`-sized cell, so suppression only
    // looks at the neighbouring cells.
    let bucket = radius.max(1.0) as u32;
    let (bw, bh) = (width / bucket + 1, height / bucket + 1);
    let mut kept: Vec<Vec<(u32, u32)>> = vec![Vec::new(); (bw * bh) as usize];
    let cells = FEATURE_GRID * FEATURE_GRID;
    let per_cell = MAX_FEATURES.div_ceil(cells as usize);
    let mut cell_counts = vec![0usize; cells as usize];
    let mut result = Vec::new();
    for corner in sorted_corners {
        let cell = ((corner.y * FEATURE_GRID / height.max(1)).min(FEATURE_GRID - 1) * FEATURE_GRID
            + (corner.x * FEATURE_GRID / width.max(1)).min(FEATURE_GRID - 1))
            as usize;
        if cell_counts[cell] >= per_cell {
            continue;
        }
        let (cx, cy) = (corner.x / bucket, corner.y / bucket);
        let suppressed = (cy.saturating_sub(1)..=(cy + 1).min(bh - 1)).any(|by| {
            (cx.saturating_sub(1)..=(cx + 1).min(bw - 1)).any(|bx| {
                kept[(by * bw + bx) as usize].iter().any(|&(x, y)| {
                    let dx = x as f32 - corner.x as f32;
                    let dy = y as f32 - corner.y as f32;
                    dx * dx + dy * dy < radius_sq
                })
            })
        });
        if suppressed {
            continue;
        }
        kept[(cy * bw + cx) as usize].push((corner.x, corner.y));
        cell_counts[cell] += 1;
        result.push(KeyPoint {
            x: corner.x,
            y: corner.y,
        });
    }
    result
}

pub fn generate_brief_pairs() -> Vec<(Point2<i32>, Point2<i32>)> {
    let mut rng = StdRng::seed_from_u64(12345);
    let half_patch = BRIEF_PATCH_SIZE as i32 / 2;
    let distribution = match rand::distr::Uniform::new(-half_patch, half_patch) {
        Ok(dist) => dist,
        Err(e) => panic!("Failed to create uniform distribution: {}", e),
    };

    (0..BRIEF_DESCRIPTOR_SIZE)
        .map(|_| {
            (
                Point2::new(distribution.sample(&mut rng), distribution.sample(&mut rng)),
                Point2::new(distribution.sample(&mut rng), distribution.sample(&mut rng)),
            )
        })
        .collect()
}

fn compute_brief_descriptor(
    img: &ImageBuffer<Luma<f32>, Vec<f32>>,
    kp: &KeyPoint,
    patch_size: u32,
    pairs: &[(Point2<i32>, Point2<i32>)],
) -> Option<Descriptor> {
    let mut descriptor = [0u8; BRIEF_DESCRIPTOR_SIZE / 8];
    let (width, height) = img.dimensions();
    let half_patch_size = patch_size / 2;
    if kp.x < half_patch_size
        || kp.x >= width - half_patch_size
        || kp.y < half_patch_size
        || kp.y >= height - half_patch_size
    {
        return None;
    }
    for (i, pair) in pairs.iter().enumerate() {
        let p1_x = (kp.x as i32 + pair.0.x) as u32;
        let p1_y = (kp.y as i32 + pair.0.y) as u32;
        let p2_x = (kp.x as i32 + pair.1.x) as u32;
        let p2_y = (kp.y as i32 + pair.1.y) as u32;
        let intensity1 = img.get_pixel(p1_x, p1_y)[0];
        let intensity2 = img.get_pixel(p2_x, p2_y)[0];
        if intensity1 < intensity2 {
            let byte_index = i / 8;
            let bit_index = i % 8;
            descriptor[byte_index] |= 1 << bit_index;
        }
    }
    Some(descriptor)
}

fn hamming_distance(d1: &Descriptor, d2: &Descriptor) -> u32 {
    d1.chunks_exact(8)
        .zip(d2.chunks_exact(8))
        .map(|(a, b)| {
            let a = u64::from_ne_bytes(a.try_into().unwrap());
            let b = u64::from_ne_bytes(b.try_into().unwrap());
            (a ^ b).count_ones()
        })
        .sum()
}

/// For each feature, its nearest neighbour in the other set by descriptor,
/// if clearly nearer than the second nearest (Lowe's ratio test).
fn nearest_neighbours(from: &[Feature], to: &[Feature]) -> Vec<Option<usize>> {
    from.par_iter()
        .map(|f1| {
            let mut best_dist = u32::MAX;
            let mut second_best_dist = u32::MAX;
            let mut best_idx = 0;
            for (j, f2) in to.iter().enumerate() {
                let dist = hamming_distance(&f1.descriptor, &f2.descriptor);
                if dist < best_dist {
                    second_best_dist = best_dist;
                    best_dist = dist;
                    best_idx = j;
                } else if dist < second_best_dist {
                    second_best_dist = dist;
                }
            }
            (second_best_dist > 0
                && second_best_dist != u32::MAX
                && (best_dist as f32 / second_best_dist as f32) < MATCH_RATIO_THRESHOLD)
                .then_some(best_idx)
        })
        .collect()
}

/// Matches that each side picks as the other's nearest: one to one. One-way
/// matching let many features of a busy frame land on the same few of a
/// sparse one, and RANSAC then "confirmed" a warp with more inliers than
/// that frame had features — the degenerate warps that smeared one frame
/// over the whole canvas.
pub fn match_features(features1: &[Feature], features2: &[Feature]) -> Vec<Match> {
    if features1.is_empty() || features2.is_empty() {
        return Vec::new();
    }
    let forward = nearest_neighbours(features1, features2);
    let backward = nearest_neighbours(features2, features1);
    forward
        .iter()
        .enumerate()
        .filter_map(|(i, j)| {
            let j = (*j)?;
            (backward[j] == Some(i)).then_some(Match {
                index1: i,
                index2: j,
            })
        })
        .collect()
}

fn inliers_of(
    h: &Matrix3<f64>,
    matches: &[Match],
    points: &[(Point2<f64>, Point2<f64>)],
) -> Vec<Match> {
    let threshold_sq = RANSAC_INLIER_THRESHOLD.powi(2);
    matches
        .iter()
        .zip(points)
        .filter(|(_, (p1, p2))| {
            transfer(h, p1).is_some_and(|q| (q - p2).norm_squared() < threshold_sq)
        })
        .map(|(m, _)| *m)
        .collect()
}

/// A point through a homography, if it lands in front of the camera.
fn transfer(h: &Matrix3<f64>, p: &Point2<f64>) -> Option<Point2<f64>> {
    let q = h * nalgebra::Point3::new(p.x, p.y, 1.0);
    (q.z > 1e-8).then(|| Point2::new(q.x / q.z, q.y / q.z))
}

/// The homography from frame 1 to frame 2 (at feature-detection scale) with
/// the most inliers among `matches`, refit on them, if it is one a rotating
/// camera could produce (`is_plausible_homography`). Seeded, so the same
/// frames always stitch the same way.
pub fn find_homography_ransac(
    matches: &[Match],
    keypoints1: &[KeyPoint],
    keypoints2: &[KeyPoint],
    size1: (u32, u32),
) -> Option<(Matrix3<f64>, Vec<Match>)> {
    let mut rng = StdRng::seed_from_u64(0x5EED);
    let points: Vec<(Point2<f64>, Point2<f64>)> = matches
        .iter()
        .map(|m| {
            let p1 = keypoints1[m.index1];
            let p2 = keypoints2[m.index2];
            (
                Point2::new(p1.x as f64, p1.y as f64),
                Point2::new(p2.x as f64, p2.y as f64),
            )
        })
        .collect();

    if points.len() < MIN_INLIERS_FOR_CONNECTION {
        return None;
    }

    let mut best: Option<(Matrix3<f64>, Vec<Match>)> = None;
    for _ in 0..RANSAC_ITERATIONS {
        let sample_indices = rand::seq::index::sample(&mut rng, points.len(), 4);
        let sample_points: Vec<(Point2<f64>, Point2<f64>)> =
            sample_indices.iter().map(|i| points[i]).collect();

        if are_points_collinear(sample_points[0].0, sample_points[1].0, sample_points[2].0)
            || are_points_collinear(sample_points[0].0, sample_points[1].0, sample_points[3].0)
            || are_points_collinear(sample_points[0].0, sample_points[2].0, sample_points[3].0)
            || are_points_collinear(sample_points[1].0, sample_points[2].0, sample_points[3].0)
        {
            continue;
        }

        let Some(h) = compute_homography(&sample_points) else {
            continue;
        };
        if !is_plausible_homography(&h, size1) {
            continue;
        }
        let current_inliers = inliers_of(&h, matches, &points);
        if best
            .as_ref()
            .is_none_or(|(_, inliers)| current_inliers.len() > inliers.len())
        {
            best = Some((h, current_inliers));
        }
    }

    // Refit on every inlier, and take the refit's own inliers, twice: the
    // four-point fit is only as good as its four points.
    let (mut h, mut inliers) = best?;
    for _ in 0..2 {
        let inlier_points: Vec<(Point2<f64>, Point2<f64>)> = inliers
            .iter()
            .map(|m| {
                let p1 = keypoints1[m.index1];
                let p2 = keypoints2[m.index2];
                (
                    Point2::new(p1.x as f64, p1.y as f64),
                    Point2::new(p2.x as f64, p2.y as f64),
                )
            })
            .collect();
        let Some(refit) = compute_homography(&inlier_points) else {
            break;
        };
        let refit_inliers = inliers_of(&refit, matches, &points);
        if refit_inliers.len() < inliers.len() || !is_plausible_homography(&refit, size1) {
            break;
        }
        h = refit;
        inliers = refit_inliers;
    }

    if inliers.len() < MIN_INLIERS_FOR_CONNECTION {
        return None;
    }
    let (w, ht) = (size1.0.max(1) as f64, size1.1.max(1) as f64);
    let xs = inliers.iter().map(|m| keypoints1[m.index1].x as f64);
    let ys = inliers.iter().map(|m| keypoints1[m.index1].y as f64);
    let span = |v: Vec<f64>| {
        v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min)
    };
    if span(xs.collect()) < MIN_INLIER_SPREAD * w || span(ys.collect()) < MIN_INLIER_SPREAD * ht {
        return None;
    }
    Some((h, inliers))
}

/// Could this homography map one frame of a panorama onto a neighbour?
/// Frames from one lens turned about one point differ by a rotation, which
/// between overlapping frames changes area far less than fourfold.
pub fn is_plausible_homography(h: &Matrix3<f64>, size: (u32, u32)) -> bool {
    is_plausible_warp(h, size, 4.0)
}

/// Does this homography warp a frame of `size` into a sane shape? Every
/// corner must land in front of the camera, the frame must stay a convex
/// quadrilateral the same way round (not folded or mirrored), and its area
/// change by at most `max_area_ratio` either way.
pub fn is_plausible_warp(h: &Matrix3<f64>, size: (u32, u32), max_area_ratio: f64) -> bool {
    let (w, ht) = (size.0 as f64, size.1 as f64);
    let corners = [
        Point2::new(0.0, 0.0),
        Point2::new(w, 0.0),
        Point2::new(w, ht),
        Point2::new(0.0, ht),
    ];
    let mut mapped = [Point2::origin(); 4];
    for (out, corner) in mapped.iter_mut().zip(&corners) {
        match transfer(h, corner) {
            Some(p) if p.x.is_finite() && p.y.is_finite() => *out = p,
            _ => return false,
        }
    }
    // Same turning direction at every corner as the original (image
    // coordinates: y down, so the original's cross products are positive).
    let mut area = 0.0;
    for i in 0..4 {
        let (a, b, c) = (mapped[i], mapped[(i + 1) % 4], mapped[(i + 2) % 4]);
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if cross <= 0.0 {
            return false;
        }
        area += a.x * b.y - b.x * a.y;
    }
    let ratio = (area / 2.0) / (w * ht);
    (1.0 / max_area_ratio..=max_area_ratio).contains(&ratio)
}

fn are_points_collinear(p1: Point2<f64>, p2: Point2<f64>, p3: Point2<f64>) -> bool {
    let area = p1.x * (p2.y - p3.y) + p2.x * (p3.y - p1.y) + p3.x * (p1.y - p2.y);
    area.abs() < 1e-6
}

/// Hartley's normalization: points moved to their centroid and scaled to
/// an average distance of √2, which the DLT needs to be well conditioned.
fn normalization(points: impl Iterator<Item = Point2<f64>> + Clone) -> Matrix3<f64> {
    let n = points.clone().count().max(1) as f64;
    let (sx, sy) = points
        .clone()
        .fold((0.0, 0.0), |(sx, sy), p| (sx + p.x, sy + p.y));
    let (cx, cy) = (sx / n, sy / n);
    let mean_distance = points
        .map(|p| ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt())
        .sum::<f64>()
        / n;
    let s = if mean_distance > 1e-12 {
        std::f64::consts::SQRT_2 / mean_distance
    } else {
        1.0
    };
    Matrix3::new(s, 0.0, -s * cx, 0.0, s, -s * cy, 0.0, 0.0, 1.0)
}

/// The homography taking each first point to its second (direct linear
/// transform, least squares beyond four points), scaled so that its bottom
/// right entry is 1 — so frames in front of the camera map with z > 0.
///
/// Solved as the eigenvector of AᵀA with the smallest eigenvalue. (Taking
/// the last row of A's thin SVD, as this did, is wrong for four points: A
/// is then 8×9 and its thin SVD leaves out exactly the null vector, so
/// every RANSAC hypothesis was a meaningless matrix.)
pub fn compute_homography(points: &[(Point2<f64>, Point2<f64>)]) -> Option<Matrix3<f64>> {
    if points.len() < 4 {
        return None;
    }
    let t1 = normalization(points.iter().map(|p| p.0));
    let t2 = normalization(points.iter().map(|p| p.1));
    let mut ata = nalgebra::SMatrix::<f64, 9, 9>::zeros();
    for (p1, p2) in points {
        let a = t1 * nalgebra::Point3::new(p1.x, p1.y, 1.0);
        let b = t2 * nalgebra::Point3::new(p2.x, p2.y, 1.0);
        let (x, y) = (a.x, a.y);
        let (xp, yp) = (b.x, b.y);
        for row in [
            nalgebra::SVector::<f64, 9>::from([-x, -y, -1.0, 0.0, 0.0, 0.0, x * xp, y * xp, xp]),
            nalgebra::SVector::<f64, 9>::from([0.0, 0.0, 0.0, -x, -y, -1.0, x * yp, y * yp, yp]),
        ] {
            ata += row * row.transpose();
        }
    }
    let eigen = nalgebra::SymmetricEigen::new(ata);
    let (smallest, _) = eigen
        .eigenvalues
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))?;
    let h_vec = eigen.eigenvectors.column(smallest);
    let hn = Matrix3::from_iterator(h_vec.iter().cloned()).transpose();
    let h = t2.try_inverse()? * hn * t1;
    let scale = h[(2, 2)];
    if !scale.is_finite() || scale.abs() < 1e-12 {
        return None;
    }
    let h = h / scale;
    h.iter().all(|v| v.is_finite()).then_some(h)
}

fn convert_gray_u8_to_f32(img: &GrayImage) -> ImageBuffer<Luma<f32>, Vec<f32>> {
    let (width, height) = img.dimensions();
    ImageBuffer::from_fn(width, height, |x, y| {
        Luma([img.get_pixel(x, y)[0] as f32 / 255.0])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(h: &Matrix3<f64>, p: Point2<f64>) -> Point2<f64> {
        transfer(h, &p).unwrap()
    }

    /// A rotation-like homography between neighbouring frames.
    fn known() -> Matrix3<f64> {
        Matrix3::new(0.98, 0.03, -420.0, -0.02, 1.01, 12.0, 4e-5, -1e-5, 1.0)
    }

    /// Four points determine a homography exactly (the thin SVD this used
    /// to take could not: its 8×9 system's null vector was left out).
    #[test]
    fn four_points_recover_the_homography() {
        let h = known();
        let sources = [
            Point2::new(900.0, 100.0),
            Point2::new(1500.0, 150.0),
            Point2::new(1450.0, 1000.0),
            Point2::new(950.0, 900.0),
        ];
        let points: Vec<_> = sources.iter().map(|&p| (p, apply(&h, p))).collect();
        let fit = compute_homography(&points).unwrap();
        for p in [Point2::new(1200.0, 500.0), Point2::new(1000.0, 1050.0)] {
            assert!((apply(&fit, p) - apply(&h, p)).norm() < 1e-6);
        }
    }

    /// RANSAC finds the warp through a third of outliers, and refuses a
    /// pile of matches all landing on one spot.
    #[test]
    fn ransac_recovers_the_warp_and_rejects_degenerate_matches() {
        let h = known();
        let mut keypoints1 = Vec::new();
        let mut keypoints2 = Vec::new();
        for i in 0..90u32 {
            let p = Point2::new(800.0 + (i * 37 % 750) as f64, 50.0 + (i * 53 % 950) as f64);
            let q = if i % 3 == 0 {
                Point2::new((i * 97 % 1500) as f64, (i * 61 % 1000) as f64)
            } else {
                apply(&h, p)
            };
            keypoints1.push(KeyPoint {
                x: p.x as u32,
                y: p.y as u32,
            });
            keypoints2.push(KeyPoint {
                x: q.x.round().max(0.0) as u32,
                y: q.y.round().max(0.0) as u32,
            });
        }
        let matches: Vec<Match> = (0..90)
            .map(|i| Match {
                index1: i,
                index2: i,
            })
            .collect();
        let (fit, inliers) =
            find_homography_ransac(&matches, &keypoints1, &keypoints2, (1600, 1067)).unwrap();
        assert!(inliers.len() >= 55, "{} inliers", inliers.len());
        let p = Point2::new(1200.0, 500.0);
        assert!((apply(&fit, p) - apply(&h, p)).norm() < 3.0);

        let collapsed: Vec<KeyPoint> = (0..90).map(|_| KeyPoint { x: 40, y: 700 }).collect();
        assert!(find_homography_ransac(&matches, &keypoints1, &collapsed, (1600, 1067)).is_none());
    }

    /// Warps that fold a frame, mirror it, or throw a corner behind the
    /// camera are not panorama neighbours.
    #[test]
    fn implausible_warps_are_refused() {
        let size = (1600, 1067);
        assert!(is_plausible_homography(&known(), size));
        let mirrored = Matrix3::new(-1.0, 0.0, 1600.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0);
        assert!(!is_plausible_homography(&mirrored, size));
        // The frame's right edge crosses the horizon line.
        let behind = Matrix3::new(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0 / 1000.0, 0.0, 1.0);
        assert!(!is_plausible_homography(&behind, size));
        let blown_up = Matrix3::new(5.0, 0.0, 0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 1.0);
        assert!(!is_plausible_homography(&blown_up, size));
    }

    /// A dark frame's tones are stretched so its median sits at mid-grey,
    /// in the same order.
    #[test]
    fn dark_frames_are_normalized() {
        let dark = GrayImage::from_fn(64, 64, |x, y| Luma([((x + y) / 4) as u8]));
        let normalized = normalize_tones(&dark);
        let mut levels: Vec<u8> = normalized.pixels().map(|p| p[0]).collect();
        levels.sort();
        assert!((110..=145).contains(&levels[levels.len() / 2]));
        assert!(levels[levels.len() - 1] >= 250);
        let mut pairs: Vec<(u8, u8)> = dark
            .pixels()
            .zip(normalized.pixels())
            .map(|(a, b)| (a[0], b[0]))
            .collect();
        pairs.sort();
        assert!(pairs.windows(2).all(|w| w[0].1 <= w[1].1));
    }
}
