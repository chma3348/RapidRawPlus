//! Deterministic post-generation blending. This module never calls a model.
use image::{GrayImage, Luma, RgbaImage};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlendOptions {
    pub improved: bool,
    /// Transition width at a 1536-pixel crop, independent of source resolution.
    pub transition: f32,
    pub appearance: f32,
}

impl BlendOptions {
    pub fn validate(self) -> Result<Self, String> {
        if !self.transition.is_finite()
            || !(0.0..=160.0).contains(&self.transition)
            || !self.appearance.is_finite()
            || !(0.0..=100.0).contains(&self.appearance)
        {
            return Err("Blend settings are outside their supported range.".into());
        }
        Ok(self)
    }
}

fn luminance(p: &image::Rgba<u8>) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// Conservative allowed region: sky membership plus foreground/line barriers.
/// This is deliberately stricter than the mask used to condition generation.
fn sky_allowed(photo: &RgbaImage, sky: &GrayImage) -> GrayImage {
    let (w, h) = photo.dimensions();
    let barrier = GrayImage::from_fn(w, h, |x, y| {
        let p = photo.get_pixel(x, y);
        let red_line = p[0] as f32 > p[1] as f32 * 1.25
            && p[0] as i32 - p[1] as i32 > 12
            && p[0] as f32 > p[2] as f32 * 1.2;
        let l = luminance(p);
        let edge = [
            (x.saturating_sub(1), y),
            ((x + 1).min(w - 1), y),
            (x, y.saturating_sub(1)),
            (x, (y + 1).min(h - 1)),
        ]
        .iter()
        .any(|&(nx, ny)| (l - luminance(photo.get_pixel(nx, ny))).abs() > 28.0);
        Luma([if red_line || edge || sky.get_pixel(x, y)[0] <= 127 {
            255
        } else {
            0
        }])
    });
    GrayImage::from_fn(w, h, |x, y| {
        // A small guard also protects antialiased foreground boundaries.
        let blocked = (y.saturating_sub(2)..=(y + 2).min(h - 1)).any(|ny| {
            (x.saturating_sub(2)..=(x + 2).min(w - 1)).any(|nx| barrier.get_pixel(nx, ny)[0] > 0)
        });
        Luma([if blocked { 0 } else { 255 }])
    })
}

fn median(values: &mut [f32]) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    values[values.len() / 2]
}

/// Returns uncomposited color and the final alpha mask; the editor applies alpha
/// exactly once. No global contrast normalization against a blank white sky.
pub fn blend(
    photo: &RgbaImage,
    generated: &RgbaImage,
    selection: &GrayImage,
    sky: Option<&GrayImage>,
    allow_expansion: bool,
    options: BlendOptions,
) -> Result<(RgbaImage, GrayImage), String> {
    let options = options.validate()?;
    let dims = photo.dimensions();
    if dims.0 == 0
        || dims.1 == 0
        || generated.dimensions() != dims
        || selection.dimensions() != dims
        || sky.is_some_and(|s| s.dimensions() != dims)
    {
        return Err("Cached replacement images do not share the same dimensions.".into());
    }
    if !options.improved {
        let margin = (dims.0.max(dims.1) / 384).clamp(2, 24);
        return Ok((
            crate::heal_blend::blend_generated(photo, generated, selection, margin as f32 * 2.0),
            selection.clone(),
        ));
    }
    let (w, h) = (dims.0 as usize, dims.1 as usize);
    let allowed = sky.map(|s| sky_allowed(photo, s));
    let radius = (options.transition * dims.0.max(dims.1) as f32 / 1536.0).round() as u32;
    let mut distances = vec![u32::MAX; w * h];
    let mut queue = BinaryHeap::new();
    for (i, p) in selection.pixels().enumerate() {
        if p[0] > 0 {
            distances[i] = 0;
        }
    }
    if allow_expansion
        && radius > 0
        && let Some(allowed) = &allowed
    {
        // Do not turn isolated clipped specks into large new islands.
        // Only substantial components seed the outward transition.
        let mut visited = vec![false; w * h];
        for start in 0..w * h {
            if visited[start] || selection.as_raw()[start] == 0 {
                continue;
            }
            let mut component = VecDeque::from([start]);
            let mut boundary = Vec::new();
            let mut area = 0u64;
            visited[start] = true;
            while let Some(i) = component.pop_front() {
                area += 1;
                let (x, y) = (i % w, i / w);
                let mut is_boundary = false;
                for (nx, ny) in [
                    (x.saturating_sub(1), y),
                    ((x + 1).min(w - 1), y),
                    (x, y.saturating_sub(1)),
                    (x, (y + 1).min(h - 1)),
                ] {
                    let n = ny * w + nx;
                    if selection.as_raw()[n] == 0 {
                        is_boundary = true;
                    } else if !visited[n] {
                        visited[n] = true;
                        component.push_back(n);
                    }
                }
                if is_boundary {
                    boundary.push(i);
                }
            }
            if area >= (radius as u64 * radius as u64 / 4).max(16) {
                for i in boundary {
                    queue.push(Reverse((0u32, i)));
                }
            }
        }
        while let Some(Reverse((distance, i))) = queue.pop() {
            if distance != distances[i] {
                continue;
            }
            let (x, y) = (i % w, i / w);
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let n = ny * w + nx;
                    let diagonal = nx != x && ny != y;
                    let next = distance + if diagonal { 14 } else { 10 };
                    if next > radius * 10 {
                        continue;
                    }
                    // Diagonal paths may not cut corners around a barrier.
                    if diagonal
                        && (allowed.as_raw()[y * w + nx] == 0 || allowed.as_raw()[ny * w + x] == 0)
                    {
                        continue;
                    }
                    if distances[n] > next && allowed.as_raw()[n] > 0 {
                        distances[n] = next;
                        queue.push(Reverse((next, n)));
                    }
                }
            }
        }
    }
    let final_mask = GrayImage::from_fn(dims.0, dims.1, |x, y| {
        let selected = selection.get_pixel(x, y)[0];
        let d = distances[y as usize * w + x as usize];
        if selected > 0 {
            return Luma([selected]);
        }
        let a = if radius > 0 && d < radius * 10 {
            1.0 - d as f32 / (radius * 10) as f32
        } else {
            0.0
        };
        Luma([(255.0 * a * a * (3.0 - 2.0 * a)).round() as u8])
    });
    let mut result = generated.clone();
    // Match only corresponding healthy sky samples, not hills, red markings,
    // clipped pixels, or unrelated materials (e.g. skin around a shirt).
    let mut offsets = Vec::new();
    let mut saturation_ratios = Vec::new();
    if let Some(allowed) = &allowed {
        let stride = (w * h / 60000).max(1);
        for i in (0..w * h).step_by(stride) {
            if allowed.as_raw()[i] == 0 || selection.as_raw()[i] > 0 {
                continue;
            }
            let p = photo.get_pixel((i % w) as u32, (i / w) as u32);
            let g = generated.get_pixel((i % w) as u32, (i / w) as u32);
            let pl = luminance(p);
            let gl = luminance(g);
            if !(45.0..240.0).contains(&pl)
                || !(25.0..240.0).contains(&gl)
                || p.0[..3].iter().any(|&v| v >= 250)
            {
                continue;
            }
            offsets.push(pl - gl);
            let pc = p.0[..3].iter().map(|&v| (v as f32 - pl).abs()).sum::<f32>();
            let gc = g.0[..3].iter().map(|&v| (v as f32 - gl).abs()).sum::<f32>();
            if gc > 12.0 {
                saturation_ratios.push(pc / gc);
            }
        }
    }
    if offsets.len() >= 32 && options.appearance > 0.0 {
        let strength = options.appearance / 100.0;
        let offset = median(&mut offsets).clamp(-32.0, 32.0) * strength;
        let saturation = if saturation_ratios.len() >= 32 {
            1.0 + (median(&mut saturation_ratios).clamp(0.65, 1.15) - 1.0) * strength
        } else {
            1.0
        };
        for p in result.pixels_mut() {
            let l = luminance(p);
            for c in 0..3 {
                p[c] = (l + offset + (p[c] as f32 - l) * saturation)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
    // At protected edges, use the proven narrow seam correction. This never
    // drags the entire cloud interior toward the surrounding white region.
    let hard = GrayImage::from_fn(dims.0, dims.1, |x, y| {
        Luma([if final_mask.get_pixel(x, y)[0] > 0 {
            255
        } else {
            0
        }])
    });
    let seam = if allow_expansion && sky.is_some() {
        (dims.0.max(dims.1) / 384).clamp(2, 24) as f32 * 2.0
    } else {
        // Non-sky selections and subtractive refinements keep their exact
        // confines. The width control adjusts the inward seam instead.
        (radius as f32).max(1.0)
    };
    Ok((
        crate::heal_blend::blend_generated(photo, &result, &hard, seam),
        final_mask,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> BlendOptions {
        BlendOptions {
            improved: true,
            transition: 160.0,
            appearance: 0.0,
        }
    }
    #[test]
    fn original_blend_is_bit_exact() {
        let p = RgbaImage::from_pixel(64, 64, image::Rgba([180, 180, 180, 255]));
        let g = RgbaImage::from_pixel(64, 64, image::Rgba([70, 110, 180, 255]));
        let m = GrayImage::from_fn(64, 64, |x, y| {
            Luma([if (10..54).contains(&x) && (10..54).contains(&y) {
                255
            } else {
                0
            }])
        });
        let (actual, mask) = blend(
            &p,
            &g,
            &m,
            None,
            true,
            BlendOptions {
                improved: false,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(actual, crate::heal_blend::blend_generated(&p, &g, &m, 4.0));
        assert_eq!(mask, m);
    }
    #[test]
    fn expansion_stops_at_sky_boundary_and_red_line() {
        let mut p = RgbaImage::from_pixel(128, 128, image::Rgba([170, 180, 190, 255]));
        for y in 0..128 {
            p.put_pixel(65, y, image::Rgba([130, 30, 30, 255]));
        }
        let m = GrayImage::from_fn(128, 128, |x, y| {
            Luma([if (40..60).contains(&x) && (40..80).contains(&y) {
                255
            } else {
                0
            }])
        });
        let sky = GrayImage::from_fn(128, 128, |_, y| Luma([if y < 82 { 255 } else { 0 }]));
        let (_, out) = blend(&p, &p, &m, Some(&sky), true, options()).unwrap();
        assert!(out.get_pixel(35, 60)[0] > 0);
        for y in 0..128 {
            assert_eq!(out.get_pixel(65, y)[0], 0);
            assert_eq!(out.get_pixel(68, y)[0], 0);
        }
        for x in 0..128 {
            assert_eq!(out.get_pixel(x, 82)[0], 0);
        }
        assert_eq!(out.get_pixel(45, 50)[0], 255);
    }
    #[test]
    fn absent_sky_or_negative_refinements_never_expand_selection() {
        let p = RgbaImage::from_pixel(32, 32, image::Rgba([180, 180, 180, 255]));
        let m = GrayImage::from_fn(32, 32, |x, _| Luma([if x < 16 { 255 } else { 0 }]));
        let sky = GrayImage::from_pixel(32, 32, Luma([255]));
        assert_eq!(blend(&p, &p, &m, None, true, options()).unwrap().1, m);
        assert_eq!(
            blend(&p, &p, &m, Some(&sky), false, options()).unwrap().1,
            m
        );
    }
    #[test]
    fn all_white_surroundings_do_not_flatten_cloud_contrast() {
        let p = RgbaImage::from_pixel(128, 128, image::Rgba([255, 255, 255, 255]));
        let g = RgbaImage::from_fn(128, 128, |x, _| {
            image::Rgba([80 + x as u8, 100 + x as u8, 120 + x as u8, 255])
        });
        let m = GrayImage::from_fn(128, 128, |x, y| {
            Luma([if (8..120).contains(&x) && (8..120).contains(&y) {
                255
            } else {
                0
            }])
        });
        let sky = GrayImage::from_pixel(128, 128, Luma([255]));
        let (a, _) = blend(
            &p,
            &g,
            &m,
            Some(&sky),
            true,
            BlendOptions {
                appearance: 100.0,
                ..options()
            },
        )
        .unwrap();
        assert_eq!(a.get_pixel(64, 64), g.get_pixel(64, 64));
        assert!(
            BlendOptions {
                transition: f32::NAN,
                ..options()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn isolated_specks_do_not_grow_into_islands() {
        let p = RgbaImage::from_pixel(128, 128, image::Rgba([180, 180, 180, 255]));
        let m = GrayImage::from_fn(128, 128, |x, y| {
            Luma([if x == 64 && y == 64 { 255 } else { 0 }])
        });
        let sky = GrayImage::from_pixel(128, 128, Luma([255]));
        assert_eq!(blend(&p, &p, &m, Some(&sky), true, options()).unwrap().1, m);
    }
}
