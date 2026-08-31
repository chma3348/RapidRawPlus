//! Bracket merging with alignment and deghosting.
//!
//! Three stages, each independently testable:
//!   1. `align_translation` — median-threshold-bitmap (Ward) alignment.
//!      MTB is exposure-invariant by construction, which is exactly what a
//!      bracket needs: frames differing by several stops still align.
//!   2. `merge_frames` — per-pixel weighted radiance average. Each frame
//!      contributes `value / exposure`, weighted by how well exposed that
//!      pixel is, so blown and crushed samples drop out instead of
//!      poisoning the result.
//!   3. deghosting — samples that disagree with the best-exposed frame by
//!      more than a stop are rejected, so something that moved between
//!      frames resolves to one state instead of smearing.
//!
//! Everything works in LINEAR light; the caller is responsible for
//! encoding for display.

use image::{GrayImage, Luma, Rgb, Rgb32FImage};

/// Pyramid depth for alignment search: each level halves the image, so a
/// 6-level search covers shifts of roughly ±64px at full resolution.
const ALIGN_LEVELS: u32 = 6;
/// Pixels within this distance of the median carry no reliable edge
/// information, so they are excluded from the match score.
const MTB_NOISE_TOLERANCE: u8 = 4;
/// Deghost rejection threshold, in stops of disagreement.
const DEGHOST_STOPS: f32 = 1.0;

fn luma_u8(img: &Rgb32FImage) -> GrayImage {
    let mut out = GrayImage::new(img.width(), img.height());
    for (x, y, p) in img.enumerate_pixels() {
        // Encoded-ish luma: alignment cares about structure, not radiometry.
        let l = (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
            .clamp(0.0, 1.0)
            .powf(1.0 / 2.2);
        out.put_pixel(x, y, Luma([(l * 255.0).round() as u8]));
    }
    out
}

fn median_of(img: &GrayImage) -> u8 {
    let mut hist = [0u32; 256];
    for p in img.pixels() {
        hist[p[0] as usize] += 1;
    }
    let total: u32 = hist.iter().sum();
    let mut seen = 0u32;
    for (value, count) in hist.iter().enumerate() {
        seen += count;
        if seen * 2 >= total {
            return value as u8;
        }
    }
    128
}

/// Median threshold bitmap plus an exclusion mask for near-median pixels.
fn mtb(img: &GrayImage) -> (Vec<bool>, Vec<bool>) {
    let median = median_of(img);
    let mut bitmap = Vec::with_capacity(img.as_raw().len());
    let mut exclusion = Vec::with_capacity(img.as_raw().len());
    for p in img.pixels() {
        bitmap.push(p[0] > median);
        exclusion.push(p[0].abs_diff(median) > MTB_NOISE_TOLERANCE);
    }
    (bitmap, exclusion)
}

fn halve(img: &GrayImage) -> GrayImage {
    let (w, h) = (img.width() / 2, img.height() / 2);
    let mut out = GrayImage::new(w.max(1), h.max(1));
    for y in 0..out.height() {
        for x in 0..out.width() {
            let mut sum = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let sx = (x * 2 + dx).min(img.width() - 1);
                    let sy = (y * 2 + dy).min(img.height() - 1);
                    sum += img.get_pixel(sx, sy)[0] as u32;
                }
            }
            out.put_pixel(x, y, Luma([(sum / 4) as u8]));
        }
    }
    out
}

fn mismatch(
    a: &(Vec<bool>, Vec<bool>),
    b: &(Vec<bool>, Vec<bool>),
    w: i64,
    h: i64,
    dx: i64,
    dy: i64,
) -> u64 {
    let (a_bits, a_excl) = a;
    let (b_bits, b_excl) = b;
    let mut error = 0u64;
    for y in 0..h {
        let sy = y + dy;
        if sy < 0 || sy >= h {
            continue;
        }
        for x in 0..w {
            let sx = x + dx;
            if sx < 0 || sx >= w {
                continue;
            }
            let ai = (y * w + x) as usize;
            let bi = (sy * w + sx) as usize;
            if a_excl[ai] && b_excl[bi] && a_bits[ai] != b_bits[bi] {
                error += 1;
            }
        }
    }
    error
}

/// Returns the (dx, dy) that best aligns `moving` onto `reference`.
/// Pyramid search: each level refines the previous level's estimate by
/// ±1px, so the cost stays near-linear in pixels.
pub fn align_translation(reference: &Rgb32FImage, moving: &Rgb32FImage) -> (i32, i32) {
    let mut ref_pyramid = vec![luma_u8(reference)];
    let mut mov_pyramid = vec![luma_u8(moving)];
    for _ in 1..ALIGN_LEVELS {
        let r = ref_pyramid.last().unwrap();
        let m = mov_pyramid.last().unwrap();
        if r.width() < 32 || r.height() < 32 {
            break;
        }
        ref_pyramid.push(halve(r));
        mov_pyramid.push(halve(m));
    }

    let (mut dx, mut dy) = (0i64, 0i64);
    for level in (0..ref_pyramid.len()).rev() {
        let r = &ref_pyramid[level];
        let m = &mov_pyramid[level];
        let (w, h) = (r.width() as i64, r.height() as i64);
        let r_mtb = mtb(r);
        let m_mtb = mtb(m);
        // Carry the coarser estimate down one level.
        dx *= 2;
        dy *= 2;
        let (mut best_dx, mut best_dy) = (dx, dy);
        let mut best_err = u64::MAX;
        for cand_dy in (dy - 1)..=(dy + 1) {
            for cand_dx in (dx - 1)..=(dx + 1) {
                let err = mismatch(&r_mtb, &m_mtb, w, h, cand_dx, cand_dy);
                if err < best_err {
                    best_err = err;
                    best_dx = cand_dx;
                    best_dy = cand_dy;
                }
            }
        }
        dx = best_dx;
        dy = best_dy;
    }
    (dx as i32, dy as i32)
}

/// Shifts an image by whole pixels, replicating the border.
pub fn shift_image(img: &Rgb32FImage, dx: i32, dy: i32) -> Rgb32FImage {
    if dx == 0 && dy == 0 {
        return img.clone();
    }
    let (w, h) = (img.width() as i32, img.height() as i32);
    let mut out = Rgb32FImage::new(img.width(), img.height());
    for y in 0..h {
        for x in 0..w {
            let sx = (x + dx).clamp(0, w - 1) as u32;
            let sy = (y + dy).clamp(0, h - 1) as u32;
            out.put_pixel(x as u32, y as u32, *img.get_pixel(sx, sy));
        }
    }
    out
}

/// Confidence that a linear sample is usable: peaks mid-range, falls to
/// zero at black and at clipping.
fn weight(linear: f32) -> f32 {
    let encoded = linear.clamp(0.0, 1.0).powf(1.0 / 2.2);
    let centred = (encoded - 0.5) * 2.0;
    let w = 1.0 - centred.powi(12);
    w.clamp(0.0, 1.0)
}

pub struct MergeOptions {
    pub deghost: bool,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self { deghost: true }
    }
}

/// Merges aligned linear frames into a single radiance image.
/// `exposures` are shutter times in seconds; the result is scaled so its
/// bright end lands near 1.0 while keeping relative radiance intact.
pub fn merge_frames(
    frames: &[Rgb32FImage],
    exposures: &[f32],
    options: &MergeOptions,
) -> Result<Rgb32FImage, String> {
    if frames.is_empty() {
        return Err("No frames to merge".to_string());
    }
    if frames.len() != exposures.len() {
        return Err("Frame and exposure counts differ".to_string());
    }
    let (w, h) = frames[0].dimensions();
    if frames.iter().any(|f| f.dimensions() != (w, h)) {
        return Err("All frames must share dimensions".to_string());
    }
    if exposures.iter().any(|e| *e <= 0.0) {
        return Err("Exposure times must be positive".to_string());
    }

    let mut out = Rgb32FImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut merged = [0.0f32; 3];
            for c in 0..3 {
                // Best-exposed sample anchors both the fallback value and
                // the deghost comparison.
                let mut best_w = -1.0f32;
                let mut best_radiance = 0.0f32;
                for (frame, exposure) in frames.iter().zip(exposures) {
                    let v = frame.get_pixel(x, y)[c].max(0.0);
                    let wgt = weight(v);
                    if wgt > best_w {
                        best_w = wgt;
                        best_radiance = v / exposure;
                    }
                }

                let mut acc = 0.0f32;
                let mut acc_w = 0.0f32;
                for (frame, exposure) in frames.iter().zip(exposures) {
                    let v = frame.get_pixel(x, y)[c].max(0.0);
                    let wgt = weight(v);
                    if wgt <= 0.0 {
                        continue;
                    }
                    let radiance = v / exposure;
                    if options.deghost && best_radiance > 1e-6 && radiance > 1e-6 {
                        // Something that moved reads as a large radiance
                        // disagreement with the best-exposed sample.
                        let stops = (radiance / best_radiance).log2().abs();
                        if stops > DEGHOST_STOPS {
                            continue;
                        }
                    }
                    acc += radiance * wgt;
                    acc_w += wgt;
                }
                merged[c] = if acc_w > 0.0 { acc / acc_w } else { best_radiance };
            }
            out.put_pixel(x, y, Rgb(merged));
        }
    }

    // Normalise by a high percentile so the result sits in a familiar
    // range without clipping speculars to white.
    let mut lumas: Vec<f32> = out
        .pixels()
        .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .collect();
    let idx = ((lumas.len() as f32 * 0.999) as usize).min(lumas.len().saturating_sub(1));
    let (_, &mut pivot, _) = lumas.select_nth_unstable_by(idx, |a, b| a.partial_cmp(b).unwrap());
    if pivot > 1e-6 {
        let scale = 1.0 / pivot;
        for p in out.pixels_mut() {
            for c in 0..3 {
                p[c] *= scale;
            }
        }
    }
    Ok(out)
}

/// Aligns every frame to the reference (the one whose exposure sits
/// nearest the middle of the bracket) and reports the shifts applied.
pub fn align_frames(frames: &mut [Rgb32FImage], exposures: &[f32]) -> Vec<(i32, i32)> {
    if frames.len() < 2 {
        return vec![(0, 0); frames.len()];
    }
    let mut order: Vec<usize> = (0..exposures.len()).collect();
    order.sort_by(|a, b| exposures[*a].partial_cmp(&exposures[*b]).unwrap());
    let reference_index = order[order.len() / 2];
    let reference = frames[reference_index].clone();

    let mut shifts = vec![(0, 0); frames.len()];
    for (i, frame) in frames.iter_mut().enumerate() {
        if i == reference_index {
            continue;
        }
        let (dx, dy) = align_translation(&reference, frame);
        if dx != 0 || dy != 0 {
            *frame = shift_image(frame, dx, dy);
        }
        shifts[i] = (dx, dy);
    }
    shifts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash01(x: i64, y: i64) -> f32 {
        let mut h = (x.wrapping_mul(374_761_393).wrapping_add(y.wrapping_mul(668_265_263))) as u64;
        h ^= h >> 13;
        h = h.wrapping_mul(1_274_126_177);
        h ^= h >> 16;
        (h & 0xffff) as f32 / 65535.0
    }

    /// Smooth value noise: non-periodic structure, so alignment cannot
    /// lock onto a repeating pattern the way it can with a checkerboard.
    fn value_noise(x: f32, y: f32, cell: f32) -> f32 {
        let (gx, gy) = (x / cell, y / cell);
        let (x0, y0) = (gx.floor() as i64, gy.floor() as i64);
        let (fx, fy) = (gx - x0 as f32, gy - y0 as f32);
        let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
        let n00 = hash01(x0, y0);
        let n10 = hash01(x0 + 1, y0);
        let n01 = hash01(x0, y0 + 1);
        let n11 = hash01(x0 + 1, y0 + 1);
        let a = n00 + (n10 - n00) * sx;
        let b = n01 + (n11 - n01) * sx;
        a + (b - a) * sy
    }

    fn scene(w: u32, h: u32) -> Rgb32FImage {
        let mut img = Rgb32FImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32, y as f32);
                let mut v = 0.18
                    + 0.45 * value_noise(fx, fy, 29.0)
                    + 0.18 * value_noise(fx, fy, 11.0);
                // Unique landmarks break any residual symmetry.
                if (30..58).contains(&x) && (44..70).contains(&y) {
                    v = 0.92;
                }
                let (dx, dy) = (fx - w as f32 * 0.72, fy - h as f32 * 0.35);
                if (dx * dx + dy * dy).sqrt() < 18.0 {
                    v = 0.05;
                }
                img.put_pixel(x, y, Rgb([v, v * 0.95, v * 0.85]));
            }
        }
        img
    }

    /// Simulates one bracket frame: radiance * exposure, clipped at 1.0.
    fn expose(scene: &Rgb32FImage, exposure: f32) -> Rgb32FImage {
        let mut out = scene.clone();
        for p in out.pixels_mut() {
            for c in 0..3 {
                p[c] = (p[c] * exposure).min(1.0);
            }
        }
        out
    }

    #[test]
    fn mtb_alignment_recovers_known_shift() {
        let base = scene(256, 256);
        let moved = shift_image(&base, 7, -5);
        let (dx, dy) = align_translation(&base, &moved);
        assert_eq!(
            (dx, dy),
            (-7, 5),
            "alignment must report the inverse shift that re-registers the frame"
        );
        // And applying it must undo the offset. Border pixels are lost to
        // edge replication by definition, so compare the interior.
        let restored = shift_image(&moved, dx, dy);
        let margin = 12u32;
        let mut worst = 0.0f32;
        for y in margin..(base.height() - margin) {
            for x in margin..(base.width() - margin) {
                for c in 0..3 {
                    worst = worst.max((base.get_pixel(x, y)[c] - restored.get_pixel(x, y)[c]).abs());
                }
            }
        }
        assert!(worst < 0.02, "re-registered interior still differs by {worst}");
    }

    /// Alignment must survive a multi-stop exposure difference — the whole
    /// point of using a median threshold bitmap.
    #[test]
    fn alignment_is_exposure_invariant() {
        let base = scene(256, 256);
        let dark = expose(&base, 0.25);
        let moved_dark = shift_image(&dark, 4, 3);
        let (dx, dy) = align_translation(&base, &moved_dark);
        assert_eq!((dx, dy), (-4, -3), "2-stop difference broke alignment");
    }

    /// The merge must recover the true radiance RATIO between two regions,
    /// even though no single frame exposes both correctly.
    #[test]
    fn recovers_radiance_ratio_across_exposures() {
        let (w, h) = (64u32, 64u32);
        let mut truth = Rgb32FImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // Left half is 8x brighter than the right half.
                let v = if x < w / 2 { 0.8 } else { 0.1 };
                truth.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let exposures = [0.25f32, 1.0, 4.0];
        let frames: Vec<Rgb32FImage> = exposures.iter().map(|e| expose(&truth, *e)).collect();
        let merged = merge_frames(&frames, &exposures, &MergeOptions { deghost: false }).unwrap();

        let bright = merged.get_pixel(10, 32)[0];
        let dark = merged.get_pixel(w - 10, 32)[0];
        let ratio = bright / dark;
        assert!(
            (ratio - 8.0).abs() < 0.8,
            "recovered ratio {ratio:.2} should be ~8 (true radiance ratio)"
        );
    }

    /// A subject that moved between frames must resolve to one state
    /// rather than a smeared average.
    #[test]
    fn deghost_rejects_a_moving_subject() {
        let (w, h) = (64u32, 64u32);
        let truth = Rgb32FImage::from_pixel(w, h, Rgb([0.25, 0.25, 0.25]));
        let exposures = [0.5f32, 1.0, 2.0];
        let mut frames: Vec<Rgb32FImage> = exposures.iter().map(|e| expose(&truth, *e)).collect();
        // In the middle frame only, a bright object occupies a patch.
        for y in 20..30 {
            for x in 20..30 {
                frames[1].put_pixel(x, y, Rgb([0.95, 0.95, 0.95]));
            }
        }
        let ghosted = merge_frames(&frames, &exposures, &MergeOptions { deghost: false }).unwrap();
        let clean = merge_frames(&frames, &exposures, &MergeOptions { deghost: true }).unwrap();

        // Each merge is normalised independently, so compare the patch
        // against its OWN background rather than across images.
        let clean_ratio = clean.get_pixel(25, 25)[0] / clean.get_pixel(5, 5)[0];
        let ghosted_ratio = ghosted.get_pixel(25, 25)[0] / ghosted.get_pixel(5, 5)[0];
        assert!(
            clean_ratio < 1.15,
            "deghosted patch should match its background (ratio {clean_ratio:.2})"
        );
        assert!(
            ghosted_ratio > 1.25,
            "without deghosting the moving object should contaminate the merge (ratio {ghosted_ratio:.2})"
        );
    }

    #[test]
    fn rejects_bad_input() {
        let a = Rgb32FImage::new(4, 4);
        assert!(merge_frames(&[], &[], &MergeOptions::default()).is_err());
        assert!(merge_frames(&[a.clone()], &[0.0], &MergeOptions::default()).is_err());
        assert!(merge_frames(&[a], &[1.0, 2.0], &MergeOptions::default()).is_err());
    }
}
