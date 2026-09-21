//! Sky replacement: composite a new sky into a photo using the Sky mask.
//!
//! Pasting a sky into the masked area looks wrong for two reasons, and this
//! module exists to fix both.
//!
//! **The old sky is mixed into the edges.** Along a branch, a wire or a
//! strand of hair, a pixel is part sky and part object: `I = a·B + (1-a)·F`.
//! Pasting leaves `F` carrying the old sky's colour, which reads as a bright
//! halo around every twig once the new sky is darker. Solving that equation
//! for `F` first (un-mixing, or spill removal) is what makes fine edges sit
//! convincingly against a different sky.
//!
//! **The light no longer matches.** A landscape lit by white overcast does
//! not belong under an orange sunset. The foreground is therefore shifted
//! toward the new sky's colour, most strongly near the horizon, and a little
//! of the new sky is mixed in there as atmospheric haze.
//!
//! The plate is placed so its own bottom edge lands on the photo's horizon
//! (the lowest row the mask still calls sky), scaled to cover, and
//! mirror-tiled if it is too short rather than stretched, because stretching
//! a short plate smears it into vertical streaks.

use image::imageops::{self, FilterType};
use image::{DynamicImage, GrayImage, RgbImage};
use rayon::prelude::*;

use crate::scene_masks::box_mean_public as box_mean;

/// How the sky plate is positioned and how hard it relights the photo.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SkyReplaceOptions {
    /// 0 = leave the foreground alone, 1 = fully adopt the new sky's colour.
    pub relight: f32,
    /// Atmospheric haze mixed into the foreground near the horizon.
    pub haze: f32,
    /// Move the horizon up (negative) or down (positive), as a fraction of
    /// the photo's height.
    pub horizon_offset: f32,
    /// Zoom into the plate; 1 = cover the frame exactly.
    pub scale: f32,
    /// Slide the plate sideways, as a fraction of its width.
    pub pan: f32,
    pub flip_horizontal: bool,
    /// Match the photo's grain so the new sky is not suspiciously clean.
    pub match_grain: bool,
    /// Move the sky/foreground boundary, as a fraction of the photo's long
    /// side. Negative pulls the new sky back behind the foreground, which
    /// hides a rim of leftover old sky; positive lets it eat into the
    /// foreground, which hides a dark fringe.
    pub edge_shift: f32,
    /// Width of the hand-over from foreground to sky, as a fraction of the
    /// long side. 0 keeps the matte's own edge.
    pub edge_feather: f32,
    /// Fade the new sky back into the original one just above the horizon,
    /// over this fraction of the photo's height, so the scene keeps its own
    /// haze and the seam disappears.
    pub horizon_fade: f32,
    /// Shift the new sky's colour cast toward the photo's own light.
    /// 0 leaves the plate as shot, 1 fully adopts the photo's white
    /// balance. Brightness and the plate's own colour variation are kept.
    pub white_balance_match: f32,
    /// Hand grading of the inserted sky, applied after the automatic match
    /// so the two compose: the slider moves away from whatever the match
    /// decided rather than fighting it. All are −100…100, 0 = no change.
    pub sky_temperature: f32,
    pub sky_tint: f32,
    /// Brightness of the sky in stops (−2…2), applied in linear light.
    pub sky_exposure: f32,
    pub sky_contrast: f32,
    pub sky_saturation: f32,
}

impl SkyReplaceOptions {
    /// The plate exactly as it was shot: nothing is matched or graded, and
    /// the foreground is left alone too.
    pub fn as_shot() -> Self {
        Self {
            relight: 0.0,
            haze: 0.0,
            white_balance_match: 0.0,
            ..Default::default()
        }
    }

    /// One click: match the sky to the photo's light, soften the seam, and
    /// relight the foreground to suit.
    pub fn auto_match() -> Self {
        Self {
            relight: 0.55,
            haze: 0.10,
            white_balance_match: 0.5,
            horizon_fade: 0.10,
            edge_feather: 0.0015,
            edge_shift: -0.0008,
            ..Default::default()
        }
    }
}

impl Default for SkyReplaceOptions {
    fn default() -> Self {
        Self {
            relight: 0.55,
            haze: 0.10,
            horizon_offset: 0.0,
            scale: 1.0,
            pan: 0.0,
            flip_horizontal: false,
            match_grain: true,
            edge_shift: 0.0,
            edge_feather: 0.0,
            horizon_fade: 0.0,
            white_balance_match: 0.4,
            sky_temperature: 0.0,
            sky_tint: 0.0,
            sky_exposure: 0.0,
            sky_contrast: 0.0,
            sky_saturation: 0.0,
        }
    }
}

/// The lowest row the mask still calls sky, per column, taken as the median
/// so a single tall tree does not drag the horizon down with it.
pub fn horizon_row(alpha: &GrayImage) -> u32 {
    let (w, h) = alpha.dimensions();
    let mut lowest = Vec::new();
    let step = (w / 256).max(1);
    for x in (0..w).step_by(step as usize) {
        let mut last = None;
        for y in 0..h {
            if alpha.get_pixel(x, y)[0] > 127 {
                last = Some(y);
            }
        }
        if let Some(y) = last {
            lowest.push(y);
        }
    }
    if lowest.is_empty() {
        return h / 2;
    }
    lowest.sort_unstable();
    lowest[lowest.len() / 2]
}

/// Scale the plate to cover the frame and sit with its bottom edge on the
/// horizon, mirror-tiling upward when it is too short.
pub fn place_plate(
    plate: &DynamicImage,
    size: (u32, u32),
    horizon: u32,
    o: &SkyReplaceOptions,
) -> RgbImage {
    let (w, h) = size;
    let zoom = o.scale.max(0.2);
    let width = ((w as f32 * zoom).round() as u32).max(1);
    let plate_h = ((plate.height() as f32 / plate.width() as f32) * width as f32).round() as u32;
    let scaled = plate.resize_exact(width, plate_h.max(1), FilterType::Lanczos3);
    let scaled = if o.flip_horizontal {
        scaled.fliph()
    } else {
        scaled
    };
    let band = scaled.to_rgb8();
    let (bw, bh) = band.dimensions();
    let x_shift = (o.pan * bw as f32).round() as i64;
    let horizon = horizon.max(1);

    RgbImage::from_fn(w, h, |x, y| {
        // Horizontal: wrap, so panning never runs out of plate.
        let sx = (x as i64 + x_shift).rem_euclid(bw as i64) as u32;
        // Vertical: the plate's bottom edge sits on the horizon; above it
        // the plate repeats mirrored, below it the last row continues
        // (below the horizon sky is only ever seen through gaps).
        let from_bottom = horizon as i64 - y as i64;
        let sy = if from_bottom <= 0 {
            bh - 1
        } else {
            let period = (2 * bh) as i64;
            let t = (from_bottom - 1).rem_euclid(period);
            if t < bh as i64 {
                (bh as i64 - 1 - t) as u32
            } else {
                (t - bh as i64) as u32
            }
        };
        *band.get_pixel(sx, sy.min(bh - 1))
    })
}

/// Move the matte's boundary and soften it. `shift` and `feather` are in
/// pixels; a positive shift grows the sky into the foreground.
pub fn adjust_edges(alpha: &GrayImage, shift: f32, feather: f32) -> GrayImage {
    if shift.abs() < 0.5 && feather < 0.5 {
        return alpha.clone();
    }
    let (w, h) = alpha.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    // A blur turns the hard matte into a ramp whose 0.5 level is the
    // boundary; offsetting the level moves the boundary, and rescaling the
    // ramp sets how wide the hand-over is.
    let radius = (feather.max(shift.abs()).max(1.0)).round() as usize;
    let values: Vec<f32> = alpha.pixels().map(|p| p[0] as f32 / 255.0).collect();
    let blurred = box_mean(&box_mean(&values, wu, hu, radius), wu, hu, radius);
    // Each unit of blurred value is roughly `radius` pixels of distance.
    let slope = if feather >= 0.5 {
        radius as f32 / feather
    } else {
        6.0
    };
    let offset = shift / radius.max(1) as f32 * 0.5;
    GrayImage::from_raw(
        w,
        h,
        blurred
            .iter()
            .map(|v| {
                let t = ((v - 0.5 + offset) * slope + 0.5).clamp(0.0, 1.0);
                (t * 255.0).round() as u8
            })
            .collect(),
    )
    .expect("edge-adjusted matte")
}

/// Per-channel gains that move `plate_mean` toward `scene_mean` in colour
/// only: the gains are normalised so overall brightness is unchanged.
pub fn white_balance_gains(plate_mean: [f32; 3], scene_mean: [f32; 3], strength: f32) -> [f32; 3] {
    let lum = |c: [f32; 3]| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).max(1e-4);
    let (pl, sl) = (lum(plate_mean), lum(scene_mean));
    let mut gains = [1.0f32; 3];
    for c in 0..3 {
        // Compare chromaticities, not raw levels, so a dark scene does not
        // darken the sky and a bright one does not blow it out.
        let ratio = ((scene_mean[c] / sl) + 1e-4) / ((plate_mean[c] / pl) + 1e-4);
        // Bounded: matching should tint the sky toward the photo's light,
        // never recolour it into a different sky.
        gains[c] = ratio
            .clamp(0.25, 4.0)
            .powf(strength.clamp(0.0, 1.0))
            .clamp(0.72, 1.38);
    }
    let g_lum = 0.2126 * gains[0] + 0.7152 * gains[1] + 0.0722 * gains[2];
    for g in gains.iter_mut() {
        *g /= g_lum.max(1e-4);
    }
    gains
}

/// Hand grading of the inserted sky: temperature, tint, exposure,
/// contrast and saturation, in that order.
///
/// Temperature and tint are luminance-preserving channel gains, so they
/// change the sky's colour without changing how bright it sits against the
/// foreground. Exposure and contrast work in linear light, so a stop is a
/// stop and the contrast pivot is mid-grey rather than a gamma-encoded
/// value.
pub fn grade_sky(sky: &mut RgbImage, o: &SkyReplaceOptions) {
    let temp = o.sky_temperature.clamp(-100.0, 100.0) / 100.0;
    let tint = o.sky_tint.clamp(-100.0, 100.0) / 100.0;
    let exposure = o.sky_exposure.clamp(-4.0, 4.0);
    let contrast = o.sky_contrast.clamp(-100.0, 100.0) / 100.0;
    let saturation = o.sky_saturation.clamp(-100.0, 100.0) / 100.0;
    if temp == 0.0 && tint == 0.0 && exposure == 0.0 && contrast == 0.0 && saturation == 0.0 {
        return;
    }
    // Warm raises red and drops blue; tint trades green against magenta.
    let raw = [1.0 + 0.30 * temp, 1.0 - 0.12 * tint, 1.0 - 0.30 * temp];
    let lum = 0.2126 * raw[0] + 0.7152 * raw[1] + 0.0722 * raw[2];
    let gains: [f32; 3] = std::array::from_fn(|c| raw[c] / lum.max(1e-4));
    let gain = 2f32.powf(exposure);
    let slope = 1.0 + contrast;
    let to_linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let to_srgb = |v: f32| {
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    const PIVOT: f32 = 0.18;

    sky.par_pixels_mut().for_each(|px| {
        let mut lin = [0.0f32; 3];
        for c in 0..3 {
            let v = to_linear((px[c] as f32 / 255.0).clamp(0.0, 1.0)) * gains[c] * gain;
            lin[c] = if contrast != 0.0 {
                (PIVOT * (v.max(1e-5) / PIVOT).powf(slope)).clamp(0.0, 4.0)
            } else {
                v
            };
        }
        if saturation != 0.0 {
            let l = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
            for v in lin.iter_mut() {
                *v = (l + (*v - l) * (1.0 + saturation)).max(0.0);
            }
        }
        for c in 0..3 {
            px[c] = (to_srgb(lin[c].clamp(0.0, 1.0)) * 255.0).round() as u8;
        }
    });
}

/// The old sky's colour as a smooth field, averaged from confident sky only.
fn old_sky_colour(photo: &RgbImage, alpha: &GrayImage, radius: usize) -> [Vec<f32>; 3] {
    let (w, h) = photo.dimensions();
    let (w, h) = (w as usize, h as usize);
    let mask: Vec<f32> = alpha.pixels().map(|p| (p[0] > 230) as u8 as f32).collect();
    let weight = box_mean(&box_mean(&mask, w, h, radius), w, h, radius);
    std::array::from_fn(|c| {
        let num: Vec<f32> = photo
            .pixels()
            .zip(&mask)
            .map(|(p, m)| p[c] as f32 / 255.0 * m)
            .collect();
        let num = box_mean(&box_mean(&num, w, h, radius), w, h, radius);
        num.iter()
            .zip(&weight)
            .map(|(n, d)| if *d > 1e-3 { n / d } else { -1.0 })
            .collect()
    })
}

/// Composite `plate` into `photo` wherever `alpha` says sky.
pub fn replace_sky(
    photo: &DynamicImage,
    alpha: &GrayImage,
    plate: &DynamicImage,
    o: &SkyReplaceOptions,
) -> anyhow::Result<DynamicImage> {
    let rgb = photo.to_rgb8();
    let (w, h) = rgb.dimensions();
    anyhow::ensure!(
        alpha.dimensions() == (w, h),
        "sky mask {:?} does not match the photo {:?}",
        alpha.dimensions(),
        (w, h)
    );
    let long = w.max(h) as f32;
    let alpha = &adjust_edges(alpha, o.edge_shift * long, o.edge_feather * long);
    let horizon = (horizon_row(alpha) as i64 + (o.horizon_offset * h as f32).round() as i64)
        .clamp(1, h as i64 - 1) as u32;
    let mut new_sky = place_plate(plate, (w, h), horizon, o);

    // White balance: move the plate's cast toward the light in this photo,
    // measured from the foreground (the sky itself is what we are
    // replacing, and a blown-out one carries no usable colour).
    if o.white_balance_match > 0.01 {
        // The scene's light is estimated from its brightest lit surfaces
        // (grey-world over the top quarter of foreground luminance), not
        // from the whole foreground: dark vegetation or shadow would drag
        // the estimate toward its own colour rather than the light's.
        let mut lums: Vec<f32> = Vec::new();
        for (i, px) in rgb.pixels().enumerate() {
            if alpha.as_raw()[i] < 128 {
                let l = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
                lums.push(l / 255.0);
            }
        }
        let cutoff = if lums.len() > 64 {
            lums.sort_by(f32::total_cmp);
            lums[lums.len() * 3 / 4]
        } else {
            0.0
        };
        let mut scene = [0.0f32; 3];
        let mut weight = 0.0f32;
        for (i, px) in rgb.pixels().enumerate() {
            if alpha.as_raw()[i] >= 128 {
                continue;
            }
            let l = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 255.0;
            if l < cutoff || l > 0.98 {
                continue;
            }
            weight += 1.0;
            for c in 0..3 {
                scene[c] += px[c] as f32 / 255.0;
            }
        }
        // The plate's own mean, over the whole frame, is the fair reference.
        let mut plate_all = [0.0f32; 3];
        for px in new_sky.pixels() {
            for c in 0..3 {
                plate_all[c] += px[c] as f32 / 255.0;
            }
        }
        let n = (w * h) as f32;
        for v in plate_all.iter_mut() {
            *v /= n;
        }
        if weight > 200.0 {
            for v in scene.iter_mut() {
                *v /= weight;
            }
            let gains = white_balance_gains(plate_all, scene, o.white_balance_match);
            for px in new_sky.pixels_mut() {
                for c in 0..3 {
                    px[c] = ((px[c] as f32 * gains[c]).clamp(0.0, 255.0)).round() as u8;
                }
            }
        }
    }
    grade_sky(&mut new_sky, o);
    let new_sky = new_sky;
    let radius = ((w.max(h) as usize) / 24).max(8);
    let old = old_sky_colour(&rgb, alpha, radius);

    // Relight: how the new sky's light compares with the old one, measured
    // over the confident sky of each.
    let mut ratio = [1.0f32; 3];
    let confident: Vec<usize> = (0..(w * h) as usize)
        .filter(|&i| alpha.as_raw()[i] > 230)
        .collect();
    if confident.len() > 500 {
        for (c, r) in ratio.iter_mut().enumerate() {
            let old_mean: f32 = confident
                .iter()
                .map(|&i| rgb.as_raw()[i * 3 + c] as f32 / 255.0)
                .sum::<f32>()
                / confident.len() as f32;
            let new_mean: f32 = confident
                .iter()
                .map(|&i| new_sky.as_raw()[i * 3 + c] as f32 / 255.0)
                .sum::<f32>()
                / confident.len() as f32;
            *r = (new_mean + 1e-3) / (old_mean + 1e-3);
        }
    }

    let grain = if o.match_grain {
        photo_grain(&rgb, alpha)
    } else {
        0.0
    };

    let mut out = vec![0u8; (w * h * 3) as usize];
    out.par_chunks_mut((w * 3) as usize)
        .enumerate()
        .for_each(|(y, row)| {
            // Near the horizon the foreground picks up most of the new
            // light and haze; far above and below it, less.
            let ramp = (1.0 - (y as f32 - horizon as f32) / (0.45 * h as f32)).clamp(0.0, 1.0);
            let relight = o.relight * (0.5 + 0.5 * ramp);
            let haze = o.haze * ramp;
            // Just above the horizon the new sky fades back into the old
            // one, so the scene keeps its own haze and the seam vanishes.
            let fade = if o.horizon_fade > 0.001 {
                let band = (o.horizon_fade * h as f32).max(1.0);
                let above = (horizon as f32 - y as f32).max(0.0);
                (above / band).clamp(0.0, 1.0)
            } else {
                1.0
            };
            for x in 0..w as usize {
                let i = y * w as usize + x;
                let a = alpha.as_raw()[i] as f32 / 255.0;
                for c in 0..3 {
                    let pixel = rgb.as_raw()[i * 3 + c] as f32 / 255.0;
                    let mut sky = new_sky.as_raw()[i * 3 + c] as f32 / 255.0;
                    if grain > 0.0 {
                        sky = (sky + grain * hash_noise(x as u32, y as u32, c as u32))
                            .clamp(0.0, 1.0);
                    }
                    // Un-mix: take the old sky back out of this pixel.
                    let old_c = old[c][i];
                    let fg = if a < 0.999 && old_c >= 0.0 {
                        ((pixel - a * old_c) / (1.0 - a)).clamp(0.0, 1.0)
                    } else {
                        pixel
                    };
                    let lit = (fg * (1.0 + relight * (ratio[c] - 1.0))).clamp(0.0, 1.0);
                    let hazed = lit * (1.0 - haze) + sky * haze;
                    // fade < 1 keeps some of the photo's own sky here
                    let sky = sky * fade + pixel * (1.0 - fade);
                    let value = a * sky + (1.0 - a) * hazed;
                    row[x * 3 + c] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        });
    Ok(DynamicImage::ImageRgb8(
        RgbImage::from_raw(w, h, out).expect("composite dimensions"),
    ))
}

/// Standard deviation of the photo's fine detail inside the sky, so the new
/// sky can be given matching grain.
fn photo_grain(photo: &RgbImage, alpha: &GrayImage) -> f32 {
    let (w, h) = photo.dimensions();
    let small = imageops::resize(photo, w.min(1024), h.min(1024), FilterType::Triangle);
    let mask = imageops::resize(alpha, small.width(), small.height(), FilterType::Triangle);
    let (sw, sh) = (small.width() as usize, small.height() as usize);
    let luma: Vec<f32> = small
        .pixels()
        .map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0)
        .collect();
    let blurred = box_mean(&box_mean(&luma, sw, sh, 2), sw, sh, 2);
    let mut n = 0usize;
    let mut sum = 0.0f32;
    for (i, m) in mask.pixels().enumerate() {
        if m[0] > 230 {
            let d = luma[i] - blurred[i];
            sum += d * d;
            n += 1;
        }
    }
    if n < 500 {
        0.0
    } else {
        (sum / n as f32).sqrt().min(0.05)
    }
}

/// Deterministic value noise in −0.5..0.5, so a re-render looks the same.
fn hash_noise(x: u32, y: u32, c: u32) -> f32 {
    let mut v = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))
        .wrapping_add(c.wrapping_mul(2_246_822_519));
    v ^= v >> 13;
    v = v.wrapping_mul(1_274_126_177);
    v ^= v >> 16;
    (v as f32 / u32::MAX as f32) - 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb};

    fn photo(sky: [u8; 3], ground: [u8; 3], horizon: u32, size: (u32, u32)) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(size.0, size.1, |_, y| {
            Rgb(if y < horizon { sky } else { ground })
        }))
    }

    fn matte(horizon: u32, size: (u32, u32)) -> GrayImage {
        GrayImage::from_fn(size.0, size.1, |_, y| {
            Luma([if y < horizon { 255 } else { 0 }])
        })
    }

    fn plain_plate(colour: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(256, 256, Rgb(colour)))
    }

    #[test]
    fn horizon_is_the_median_of_the_lowest_sky_per_column() {
        let (w, h) = (200u32, 100u32);
        // Sky down to row 40, plus one tall gap (a tree's branch) to row 80.
        let alpha = GrayImage::from_fn(w, h, |x, y| {
            Luma([if y < 40 || (x == 7 && y < 80) { 255 } else { 0 }])
        });
        assert_eq!(horizon_row(&alpha), 39);
    }

    #[test]
    fn sky_is_replaced_and_ground_is_kept() {
        let size = (120, 90);
        let opts = SkyReplaceOptions {
            relight: 0.0,
            haze: 0.0,
            match_grain: false,
            white_balance_match: 0.0,
            ..Default::default()
        };
        let out = replace_sky(
            &photo([200, 210, 240], [60, 90, 50], 45, size),
            &matte(45, size),
            &plain_plate([250, 120, 40]),
            &opts,
        )
        .unwrap()
        .to_rgb8();
        assert_eq!(out.get_pixel(60, 10).0, [250, 120, 40], "sky not replaced");
        assert_eq!(
            out.get_pixel(60, 80).0,
            [60, 90, 50],
            "ground changed with relight off"
        );
    }

    /// The point of un-mixing: a half-transparent edge pixel carries the old
    /// sky's colour, and after compositing it must carry the new sky's
    /// instead — not a blend of the object with the *old* sky.
    #[test]
    fn soft_edges_lose_the_old_sky_colour() {
        let size = (120u32, 90u32);
        let old_sky = [200.0f32, 210.0, 240.0];
        let object = [40.0f32, 40.0, 40.0];
        let mut rgb = RgbImage::from_fn(size.0, size.1, |_, y| {
            Rgb(if y < 45 {
                [old_sky[0] as u8, old_sky[1] as u8, old_sky[2] as u8]
            } else {
                [object[0] as u8, object[1] as u8, object[2] as u8]
            })
        });
        // One row of half-covered edge pixels.
        let mixed: [u8; 3] = std::array::from_fn(|c| (0.5 * old_sky[c] + 0.5 * object[c]) as u8);
        for x in 0..size.0 {
            rgb.put_pixel(x, 45, Rgb(mixed));
        }
        let alpha = GrayImage::from_fn(size.0, size.1, |_, y| {
            Luma([match y {
                y if y < 45 => 255,
                45 => 128,
                _ => 0,
            }])
        });
        let opts = SkyReplaceOptions {
            relight: 0.0,
            haze: 0.0,
            match_grain: false,
            white_balance_match: 0.0,
            ..Default::default()
        };
        let out = replace_sky(
            &DynamicImage::ImageRgb8(rgb),
            &alpha,
            &plain_plate([250, 120, 40]),
            &opts,
        )
        .unwrap()
        .to_rgb8();
        let edge = out.get_pixel(60, 45).0;
        // Expected: half the new sky, half the object.
        let want: [f32; 3] =
            std::array::from_fn(|c| 0.5 * [250.0, 120.0, 40.0][c] + 0.5 * object[c]);
        for c in 0..3 {
            assert!(
                (edge[c] as f32 - want[c]).abs() <= 6.0,
                "edge channel {c}: got {} want {:.0} (old sky not un-mixed)",
                edge[c],
                want[c]
            );
        }
    }

    #[test]
    fn relight_shifts_the_ground_toward_the_new_sky() {
        let size = (120, 90);
        let base = photo([200, 210, 240], [80, 80, 80], 45, size);
        let warm = plain_plate([250, 140, 40]);
        let neutral = SkyReplaceOptions {
            relight: 0.0,
            haze: 0.0,
            match_grain: false,
            white_balance_match: 0.0,
            ..Default::default()
        };
        let lit = SkyReplaceOptions {
            relight: 1.0,
            ..neutral
        };
        let a = replace_sky(&base, &matte(45, size), &warm, &neutral)
            .unwrap()
            .to_rgb8();
        let b = replace_sky(&base, &matte(45, size), &warm, &lit)
            .unwrap()
            .to_rgb8();
        let (pa, pb) = (a.get_pixel(60, 70).0, b.get_pixel(60, 70).0);
        assert!(pb[0] > pa[0] + 5, "red not lifted: {pa:?} -> {pb:?}");
        assert!(pb[2] < pa[2], "blue not reduced: {pa:?} -> {pb:?}");
    }

    #[test]
    fn edge_feather_turns_a_hard_matte_into_a_ramp() {
        let size = (120u32, 90u32);
        let hard = matte(45, size);
        let soft = adjust_edges(&hard, 0.0, 6.0);
        let column: Vec<u8> = (38..52).map(|y| soft.get_pixel(60, y)[0]).collect();
        let midtones = column.iter().filter(|&&v| v > 20 && v < 235).count();
        assert!(midtones >= 4, "no hand-over band: {column:?}");
        assert!(
            soft.get_pixel(60, 10)[0] > 250 && soft.get_pixel(60, 85)[0] < 5,
            "interiors moved"
        );
    }

    #[test]
    fn edge_shift_moves_the_boundary_both_ways() {
        let size = (120u32, 90u32);
        let hard = matte(45, size);
        let sky = |m: &GrayImage| m.pixels().filter(|p| p[0] > 127).count();
        let base = sky(&hard);
        let grown = sky(&adjust_edges(&hard, 4.0, 2.0));
        let shrunk = sky(&adjust_edges(&hard, -4.0, 2.0));
        assert!(grown > base + 200, "sky did not grow: {base} -> {grown}");
        assert!(
            shrunk + 200 < base,
            "sky did not shrink: {base} -> {shrunk}"
        );
    }

    #[test]
    fn white_balance_gains_tint_without_changing_brightness() {
        // A warm scene and a cold plate: matching must warm the plate.
        let gains = white_balance_gains([0.35, 0.45, 0.75], [0.60, 0.50, 0.40], 1.0);
        assert!(gains[0] > 1.0 && gains[2] < 1.0, "not warmed: {gains:?}");
        let lum = 0.2126 * gains[0] + 0.7152 * gains[1] + 0.0722 * gains[2];
        assert!((lum - 1.0).abs() < 1e-3, "brightness changed: {lum}");
        // Strength 0 is a no-op.
        let none = white_balance_gains([0.35, 0.45, 0.75], [0.60, 0.50, 0.40], 0.0);
        assert!(none.iter().all(|g| (g - 1.0).abs() < 1e-3), "{none:?}");
    }

    #[test]
    fn white_balance_match_moves_the_sky_toward_the_photos_light() {
        let size = (160, 120);
        // Warm sunlit ground under a neutral sky; the plate is cold blue.
        let base = photo([220, 220, 220], [210, 170, 120], 60, size);
        let plate = plain_plate([90, 130, 220]);
        let off = SkyReplaceOptions {
            relight: 0.0,
            haze: 0.0,
            match_grain: false,
            white_balance_match: 0.0,
            ..Default::default()
        };
        let on = SkyReplaceOptions {
            white_balance_match: 1.0,
            ..off
        };
        let a = replace_sky(&base, &matte(60, size), &plate, &off)
            .unwrap()
            .to_rgb8();
        let b = replace_sky(&base, &matte(60, size), &plate, &on)
            .unwrap()
            .to_rgb8();
        let (pa, pb) = (a.get_pixel(80, 20).0, b.get_pixel(80, 20).0);
        assert!(
            pb[0] > pa[0] && pb[2] < pa[2],
            "sky not warmed toward the scene: {pa:?} -> {pb:?}"
        );
        let lum = |p: [u8; 3]| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
        assert!(
            (lum(pb) - lum(pa)).abs() < 18.0,
            "brightness swung: {pa:?} -> {pb:?}"
        );
    }

    fn graded(plate: [u8; 3], edit: impl Fn(&mut SkyReplaceOptions)) -> [u8; 3] {
        let size = (120, 90);
        let mut o = SkyReplaceOptions::as_shot();
        edit(&mut o);
        o.match_grain = false;
        replace_sky(
            &photo([200, 205, 215], [90, 95, 90], 45, size),
            &matte(45, size),
            &plain_plate(plate),
            &o,
        )
        .unwrap()
        .to_rgb8()
        .get_pixel(60, 12)
        .0
    }

    fn luma(p: [u8; 3]) -> f32 {
        0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
    }

    #[test]
    fn as_shot_leaves_the_plate_exactly_as_it_is() {
        assert_eq!(graded([120, 150, 205], |_| {}), [120, 150, 205]);
    }

    #[test]
    fn sky_temperature_and_tint_shift_colour_not_brightness() {
        let base = graded([120, 150, 205], |_| {});
        let warm = graded([120, 150, 205], |o| o.sky_temperature = 60.0);
        let cool = graded([120, 150, 205], |o| o.sky_temperature = -60.0);
        assert!(
            warm[0] > base[0] && warm[2] < base[2],
            "not warmed: {base:?} -> {warm:?}"
        );
        assert!(
            cool[0] < base[0] && cool[2] > base[2],
            "not cooled: {base:?} -> {cool:?}"
        );
        assert!(
            (luma(warm) - luma(base)).abs() < 12.0,
            "brightness moved: {base:?} -> {warm:?}"
        );
        let green = graded([120, 150, 205], |o| o.sky_tint = -80.0);
        assert!(
            green[1] > base[1],
            "tint did not move green: {base:?} -> {green:?}"
        );
    }

    #[test]
    fn sky_exposure_is_in_stops_and_only_touches_the_sky() {
        let size = (120, 90);
        let mut o = SkyReplaceOptions::as_shot();
        o.sky_exposure = 1.0;
        o.match_grain = false;
        let out = replace_sky(
            &photo([200, 205, 215], [90, 95, 90], 45, size),
            &matte(45, size),
            &plain_plate([100, 100, 100]),
            &o,
        )
        .unwrap()
        .to_rgb8();
        // +1 stop doubles linear light: 100/255 sRGB = 0.1275 linear → 0.2550 → 138.
        let sky = out.get_pixel(60, 12).0;
        assert!(
            (sky[0] as i32 - 138).abs() <= 3,
            "one stop is not one stop: {sky:?}"
        );
        assert_eq!(out.get_pixel(60, 80).0, [90, 95, 90], "foreground changed");
    }

    #[test]
    fn sky_saturation_and_contrast_behave() {
        let grey = graded([120, 150, 205], |o| o.sky_saturation = -100.0);
        let spread = (grey[2] as i32 - grey[0] as i32).abs();
        assert!(spread < 12, "not desaturated: {grey:?}");
        // Contrast pivots on mid-grey: a dark sky gets darker, a bright one brighter.
        let dark_base = graded([60, 60, 60], |_| {});
        let dark_more = graded([60, 60, 60], |o| o.sky_contrast = 60.0);
        let bright_base = graded([220, 220, 220], |_| {});
        let bright_more = graded([220, 220, 220], |o| o.sky_contrast = 60.0);
        assert!(
            dark_more[0] < dark_base[0],
            "dark not deepened: {dark_base:?} -> {dark_more:?}"
        );
        assert!(
            bright_more[0] > bright_base[0],
            "bright not lifted: {bright_base:?} -> {bright_more:?}"
        );
    }

    #[test]
    fn auto_match_differs_from_as_shot_on_a_mismatched_scene() {
        let size = (160, 120);
        // Warm sunlit ground, cold blue plate: one click should visibly change it.
        let base = photo([215, 215, 215], [205, 165, 115], 60, size);
        let plate = plain_plate([90, 130, 220]);
        let a = replace_sky(
            &base,
            &matte(60, size),
            &plate,
            &SkyReplaceOptions::as_shot(),
        )
        .unwrap()
        .to_rgb8();
        let b = replace_sky(
            &base,
            &matte(60, size),
            &plate,
            &SkyReplaceOptions::auto_match(),
        )
        .unwrap()
        .to_rgb8();
        // as-shot still matches grain, which moves a level or two.
        let kept = a.get_pixel(80, 20).0;
        for (c, want) in [90, 130, 220].iter().enumerate() {
            assert!(
                (kept[c] as i32 - want).abs() <= 3,
                "as-shot altered the plate: {kept:?}"
            );
        }
        let m = b.get_pixel(80, 20).0;
        assert!(
            m[0] > 100 && m[2] < 220,
            "auto match did not warm the sky: {m:?}"
        );
        assert_ne!(
            a.get_pixel(80, 100).0,
            b.get_pixel(80, 100).0,
            "auto match did not relight the ground"
        );
    }

    #[test]
    fn horizon_fade_keeps_the_photos_own_sky_at_the_seam() {
        let size = (120, 200);
        let base = photo([200, 205, 215], [60, 90, 50], 150, size);
        let plate = plain_plate([250, 120, 40]);
        let opts = SkyReplaceOptions {
            relight: 0.0,
            haze: 0.0,
            match_grain: false,
            white_balance_match: 0.0,
            horizon_fade: 0.2,
            ..Default::default()
        };
        let out = replace_sky(&base, &matte(150, size), &plate, &opts)
            .unwrap()
            .to_rgb8();
        let at_seam = out.get_pixel(60, 148).0;
        let high = out.get_pixel(60, 40).0;
        assert_eq!(
            high,
            [250, 120, 40],
            "sky above the fade should be the plate"
        );
        assert!(
            at_seam[2] > 100,
            "seam kept none of the original sky: {at_seam:?}"
        );
        assert!(
            at_seam[0] > 190 && at_seam[0] < 250,
            "seam is not a blend: {at_seam:?}"
        );
    }

    #[test]
    fn a_short_plate_is_mirrored_rather_than_stretched() {
        // A plate with a distinctive gradient: mirroring keeps its texture,
        // stretching would smear it.
        let plate = DynamicImage::ImageRgb8(RgbImage::from_fn(64, 32, |_, y| {
            Rgb([(y * 8) as u8, 100, 200])
        }));
        let placed = place_plate(&plate, (64, 400), 380, &SkyReplaceOptions::default());
        // Bottom of the plate lands on the horizon.
        assert_eq!(placed.get_pixel(10, 379).0[0], 248);
        // Going up, it runs backwards then forwards again, never flat.
        let column: Vec<u8> = (0..380).map(|y| placed.get_pixel(10, y)[0]).collect();
        let distinct: std::collections::HashSet<u8> = column.iter().copied().collect();
        assert!(
            distinct.len() >= 30,
            "plate looks stretched: {} levels",
            distinct.len()
        );
    }
}
