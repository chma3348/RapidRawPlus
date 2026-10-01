//! Output sharpening: a last, light sharpening pass on the exported file,
//! after it has been resized, tuned to where it will be seen — the
//! Screen / Glossy paper / Matte paper choice in Lightroom's export.
//!
//! Shrinking a photo softens it, and each medium loses detail differently:
//! a screen shows pixels crisply and needs only a fine touch; ink spreads on
//! paper, glossy a little and matte more, so prints want a wider, stronger
//! pass. Print strengths assume 300 ppi, the usual print resolution.
//!
//! It is an unsharp mask on brightness alone — the difference between the
//! picture and a blurred copy of its luminance, added back to every channel
//! — so colours do not shift or fringe, and a small threshold keeps flat
//! areas and noise from being roughened. The strengths are all in `TUNING`.

use image::DynamicImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Medium {
    Screen,
    GlossyPaper,
    MattePaper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Amount {
    Low,
    Standard,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSharpening {
    pub medium: Medium,
    pub amount: Amount,
}

/// (blur radius as a Gaussian sigma in pixels, [low, standard, high] strength).
/// Adjust here; everything else follows.
const TUNING: [(Medium, f32, [f32; 3]); 3] = [
    (Medium::Screen, 0.6, [0.4, 0.7, 1.0]),
    (Medium::GlossyPaper, 0.9, [0.6, 1.0, 1.4]),
    (Medium::MattePaper, 1.1, [0.8, 1.3, 1.8]),
];

/// Changes smaller than this (about half a level in 8 bits) are left alone.
const THRESHOLD: f32 = 0.002;

fn settings(s: OutputSharpening) -> (f32, f32) {
    let (_, sigma, amounts) = TUNING
        .iter()
        .find(|(m, _, _)| *m == s.medium)
        .copied()
        .unwrap_or(TUNING[0]);
    let amount = match s.amount {
        Amount::Low => amounts[0],
        Amount::Standard => amounts[1],
        Amount::High => amounts[2],
    };
    (sigma, amount)
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil().max(1.0) as i32;
    let mut kernel: Vec<f32> = (-radius..=radius)
        .map(|x| (-(x * x) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|k| *k /= sum);
    kernel
}

/// Separable Gaussian blur of a single plane, edges clamped.
fn blur(plane: &[f32], width: usize, height: usize, kernel: &[f32]) -> Vec<f32> {
    let r = (kernel.len() / 2) as isize;
    let mut horizontal = vec![0.0f32; plane.len()];
    horizontal
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            let src = &plane[y * width..(y + 1) * width];
            for (x, out) in row.iter_mut().enumerate() {
                let mut acc = 0.0;
                for (i, k) in kernel.iter().enumerate() {
                    let sx = (x as isize + i as isize - r).clamp(0, width as isize - 1) as usize;
                    acc += src[sx] * k;
                }
                *out = acc;
            }
        });
    let mut out = vec![0.0f32; plane.len()];
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (i, k) in kernel.iter().enumerate() {
                let sy = (y as isize + i as isize - r).clamp(0, height as isize - 1) as usize;
                acc += horizontal[sy * width + x] * k;
            }
            *o = acc;
        }
    });
    out
}

/// Sharpen `image` for `target`. 8-bit images stay 8-bit, deeper ones come
/// back as 16-bit.
pub fn sharpen(image: DynamicImage, target: OutputSharpening) -> DynamicImage {
    let (sigma, amount) = settings(target);
    let eight_bit = matches!(
        image,
        DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) | DynamicImage::ImageLuma8(_)
    );
    let mut rgba = image.to_rgba32f();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    if w == 0 || h == 0 {
        return image;
    }
    let luma: Vec<f32> = rgba
        .pixels()
        .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .collect();
    let blurred = blur(&luma, w, h, &gaussian_kernel(sigma));
    rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i = y * w + x;
            let d = luma[i] - blurred[i];
            let d = d.signum() * (d.abs() - THRESHOLD).max(0.0);
            let add = amount * d;
            for c in 0..3 {
                row[x * 4 + c] = (row[x * 4 + c] + add).clamp(0.0, 1.0);
            }
        }
    });
    let sharpened = DynamicImage::ImageRgba32F(rgba);
    if eight_bit {
        DynamicImage::ImageRgba8(sharpened.to_rgba8())
    } else {
        DynamicImage::ImageRgba16(sharpened.to_rgba16())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    const SCREEN: OutputSharpening = OutputSharpening {
        medium: Medium::Screen,
        amount: Amount::Standard,
    };

    /// A soft vertical edge from dark to light, 16-bit.
    fn soft_edge() -> DynamicImage {
        DynamicImage::ImageRgba16(ImageBuffer::from_fn(64, 8, |x, _| {
            let t = ((x as f32 - 28.0) / 8.0).clamp(0.0, 1.0);
            let v = ((0.25 + 0.5 * t) * 65535.0) as u16;
            Rgba([v, v, v, 65535])
        }))
    }

    fn row(image: &DynamicImage) -> Vec<f32> {
        let rgba = image.to_rgba32f();
        (0..rgba.width()).map(|x| rgba.get_pixel(x, 4)[0]).collect()
    }

    #[test]
    fn flat_areas_are_left_alone() {
        let flat = DynamicImage::ImageRgba16(ImageBuffer::from_pixel(
            32,
            32,
            Rgba([30000, 30000, 30000, 65535]),
        ));
        let out = sharpen(flat.clone(), SCREEN);
        assert_eq!(out.to_rgba16().as_raw(), flat.to_rgba16().as_raw());
    }

    #[test]
    fn edges_gain_contrast_without_moving_the_tones() {
        let before = row(&soft_edge());
        let after = row(&sharpen(soft_edge(), SCREEN));
        // Darker just before the edge, lighter just after: crisper.
        assert!(after[28] < before[28], "{} vs {}", after[28], before[28]);
        assert!(after[36] > before[36], "{} vs {}", after[36], before[36]);
        // Away from the edge nothing changes, and the average holds.
        assert!((after[4] - before[4]).abs() < 1e-3);
        assert!((after[60] - before[60]).abs() < 1e-3);
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        assert!((mean(&after) - mean(&before)).abs() < 0.005);
    }

    #[test]
    fn stronger_for_paper_than_for_screen_and_with_the_amount() {
        let overshoot = |s: OutputSharpening| {
            let before = row(&soft_edge());
            let after = row(&sharpen(soft_edge(), s));
            after
                .iter()
                .zip(&before)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max)
        };
        let screen = overshoot(SCREEN);
        let matte = overshoot(OutputSharpening {
            medium: Medium::MattePaper,
            amount: Amount::Standard,
        });
        let low = overshoot(OutputSharpening {
            medium: Medium::Screen,
            amount: Amount::Low,
        });
        let high = overshoot(OutputSharpening {
            medium: Medium::Screen,
            amount: Amount::High,
        });
        assert!(matte > screen);
        assert!(low < screen && screen < high);
    }

    #[test]
    fn colour_is_kept_and_depth_preserved() {
        let colourful = DynamicImage::ImageRgb8(ImageBuffer::from_fn(32, 8, |x, _| {
            if x < 16 {
                image::Rgb([200, 40, 40])
            } else {
                image::Rgb([40, 40, 200])
            }
        }));
        let out = sharpen(colourful, SCREEN);
        assert!(
            matches!(out, DynamicImage::ImageRgba8(_)),
            "8-bit stays 8-bit"
        );
        // Sharpening adds the same amount to each channel: hue stays put.
        let p = out.to_rgba8().get_pixel(2, 4).0;
        assert_eq!(p[1], p[2]);
        assert!(matches!(
            sharpen(soft_edge(), SCREEN),
            DynamicImage::ImageRgba16(_)
        ));
    }
}
