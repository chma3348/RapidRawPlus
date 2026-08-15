//! Measurable quality guarantees for the v2 rendering engine, executed on
//! the real GPU pipeline (same shader, same processor as the app):
//!  1. Hue stability — strong contrast on a blue ramp must not skew hue in
//!     v2, and must beat v1's drift.
//!  2. Filmic rolloff — an HDR ramp must reach white smoothly (fewer hard
//!     clips than the basic mapper, shoulder engaged below clip).

use image::{ImageBuffer, Luma};
use rapidraw_lib::gpu_processing::{GpuProcessor, RenderRequest};
use rapidraw_lib::image_processing::{AllAdjustments, GpuContext};
use std::sync::Arc;

const W: u32 = 64;
const H: u32 = 64;

fn f32_to_f16_bits(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x007f_ffff;
    if exp <= 112 {
        return sign; // flush tiny to zero — fine for test data
    }
    if exp >= 143 {
        return sign | 0x7c00; // inf
    }
    sign | (((exp - 112) as u16) << 10) | ((mant >> 13) as u16)
}

fn make_device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::Limits)> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .ok()?;
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue, limits))
}

/// Renders `pixels` (linear RGBA f32, W×H) through the full pipeline.
fn render(processor: &GpuProcessor, device: &wgpu::Device, queue: &wgpu::Queue, pixels: &[f32], adjustments: AllAdjustments) -> Vec<u8> {
    render_sized(processor, device, queue, pixels, adjustments, W, H)
}

fn render_sized(
    processor: &GpuProcessor,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pixels: &[f32],
    adjustments: AllAdjustments,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test input"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let half_pixels: Vec<u16> = pixels.iter().map(|v| f32_to_f16_bits(*v)).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&half_pixels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 8),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&Default::default());
    let dummy_mask = ImageBuffer::<Luma<u8>, Vec<u8>>::new(w, h);
    let masks = [dummy_mask.clone(), dummy_mask];
    let (data, out_w, out_h, _, _) = processor
        .run(
            &view,
            w,
            h,
            RenderRequest {
                adjustments,
                mask_bitmaps: &masks,
                lut: None,
                roi: None,
            },
            false,
            false,
        )
        .expect("render");
    assert_eq!((out_w, out_h), (w, h));
    data
}

fn srgb_to_linear1(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Perceptual (Oklab) hue angle in degrees — the metric that matches what
/// eyes call "the same color, brighter". RGB/HSV hue intentionally differs
/// (Abney correction), so it is the wrong yardstick for hue stability.
fn oklab_hue_deg(r8: f32, g8: f32, b8: f32) -> (f32, f32) {
    let (r, g, b) = (srgb_to_linear1(r8), srgb_to_linear1(g8), srgb_to_linear1(b8));
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();
    let a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;
    let chroma = (a * a + bb * bb).sqrt();
    (bb.atan2(a).to_degrees(), chroma)
}

fn hue_deg(r: f32, g: f32, b: f32) -> f32 {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if d < 1e-6 {
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h * 60.0
}

#[test]
fn v2_contrast_is_hue_stable_and_filmic_rolls_off() {
    let Some((device, queue, limits)) = make_device() else {
        eprintln!("no GPU; skipping");
        return;
    };
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(std::sync::Mutex::new(None)),
    };
    let device = context.device.clone();
    let queue = context.queue.clone();
    let processor = GpuProcessor::new(context, W, H).expect("processor");

    // ---- 1. hue stability under strong contrast ----
    // Saturated sRGB-encoded blue ramp; drift is measured against an
    // identity render (contrast 0) of the same engine version, so the only
    // variable is the contrast operator itself, and only well-saturated
    // output pixels count (8-bit hue estimates are noise near neutral).
    let mut blue = vec![0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / (W - 1) as f32;
            blue[i] = 0.05;
            blue[i + 1] = 0.15;
            blue[i + 2] = 0.30 + 0.60 * t;
            blue[i + 3] = 1.0;
        }
    }

    let render_row = |pv: u32, contrast: f32| -> Vec<(f32, f32, f32)> {
        let mut adj = AllAdjustments::default();
        adj.global.contrast = contrast;
        adj.global.contrast_pivot = 0.5;
        adj.global.process_version = pv;
        adj.global.tonemapper_mode = 0;
        let out = render(&processor, &device, &queue, &blue, adj);
        let y_mid = (H / 2) as usize;
        (0..W as usize)
            .map(|x| {
                let i = (y_mid * W as usize + x) * 4;
                (
                    out[i] as f32 / 255.0,
                    out[i + 1] as f32 / 255.0,
                    out[i + 2] as f32 / 255.0,
                )
            })
            .collect()
    };

    let drift_for = |pv: u32| -> f32 {
        let base = render_row(pv, 0.0);
        let contrasted = render_row(pv, 0.8);
        let mut max_drift = 0f32;
        for x in 4..(W as usize - 4) {
            let (r0, g0, b0) = base[x];
            let (r1, g1, b1) = contrasted[x];
            let (h0, c0) = oklab_hue_deg(r0, g0, b0);
            let (h1, c1) = oklab_hue_deg(r1, g1, b1);
            if c0 < 0.03 || c1 < 0.03 {
                continue; // hue is meaningless near neutral
            }
            let mut d = (h1 - h0).abs();
            if d > 180.0 {
                d = 360.0 - d;
            }
            max_drift = max_drift.max(d);
        }
        max_drift
    };

    let v1_drift = drift_for(1);
    let v2_drift = drift_for(2);
    println!("hue drift under +80 contrast: v1 {v1_drift:.2}°, v2 {v2_drift:.2}°");
    assert!(
        v2_drift < 4.0,
        "v2 hue drift too large: {v2_drift:.2}° (v1 was {v1_drift:.2}°)"
    );
    assert!(
        v2_drift < v1_drift || v1_drift < 1.0,
        "v2 ({v2_drift:.2}°) should beat v1 ({v1_drift:.2}°)"
    );

    // ---- 2. filmic rolloff on an HDR ramp ----
    let mut ramp = vec![0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / (W - 1) as f32;
            let v = t * 4.0; // scene-linear, up to 4x white
            ramp[i] = v;
            ramp[i + 1] = v;
            ramp[i + 2] = v;
            ramp[i + 3] = 1.0;
        }
    }
    let luma_row = |mode: u32| -> Vec<u8> {
        let mut adj = AllAdjustments::default();
        adj.global.is_raw_image = 1; // scene-linear input
        adj.global.process_version = 2;
        adj.global.tonemapper_mode = mode;
        adj.global.contrast_pivot = 0.5;
        let out = render(&processor, &device, &queue, &ramp, adj);
        let y_mid = (H / 2) as usize;
        (0..W as usize)
            .map(|x| out[(y_mid * W as usize + x) * 4])
            .collect()
    };
    let filmic = luma_row(2);
    let basic = luma_row(0);

    // Monotone (allowing 8-bit dither wobble of 1).
    for x in 1..filmic.len() {
        assert!(
            filmic[x] as u16 + 1 >= filmic[x - 1] as u16,
            "filmic ramp not monotone at {x}: {} -> {}",
            filmic[x - 1],
            filmic[x]
        );
    }
    // Fewer hard-clipped pixels than basic = the shoulder is real.
    let clipped = |row: &[u8]| row.iter().filter(|v| **v >= 254).count();
    let (cf, cb) = (clipped(&filmic), clipped(&basic));
    println!("clipped pixels: filmic {cf}, basic {cb}");
    assert!(
        cf < cb,
        "filmic should clip fewer pixels than basic (filmic {cf} vs basic {cb})"
    );
    // Shoulder engaged: scene white (input 1.0 at x=16) maps below clip.
    let x_white = (W as usize - 1) / 4;
    assert!(
        filmic[x_white] < 252,
        "no shoulder: input 1.0 rendered at {}",
        filmic[x_white]
    );

    // ---- 3. highlight recovery reconstructs clipped color ----
    // Orange scene with a blown white disc in the middle; pulling
    // highlights down must give the disc back its surroundings' color
    // instead of leaving a gray/white blotch.
    const RW: u32 = 256;
    const RH: u32 = 256;
    // Fresh processor sized so blur radii are physically meaningful.
    let Some((d2, q2, l2)) = make_device() else {
        return;
    };
    let rdevice = Arc::new(d2);
    let rqueue = Arc::new(q2);
    let recovery_processor = GpuProcessor::new(
        GpuContext {
            device: rdevice.clone(),
            queue: rqueue.clone(),
            limits: l2,
            display: Arc::new(std::sync::Mutex::new(None)),
        },
        RW,
        RH,
    )
    .expect("recovery processor");
    let mut scene = vec![0f32; (RW * RH * 4) as usize];
    let (cx, cy, radius) = (RW as f32 / 2.0, RH as f32 / 2.0, 7.0f32);
    for y in 0..RH {
        for x in 0..RW {
            let i = ((y * RW + x) * 4) as usize;
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d < radius {
                scene[i] = 2.5;
                scene[i + 1] = 2.5;
                scene[i + 2] = 2.5;
            } else {
                scene[i] = 0.9;
                scene[i + 1] = 0.45;
                scene[i + 2] = 0.15;
            }
            scene[i + 3] = 1.0;
        }
    }
    let center_chroma = |pv: u32| -> f32 {
        let mut adj = AllAdjustments::default();
        adj.global.is_raw_image = 1;
        adj.global.process_version = pv;
        adj.global.highlights = -0.9;
        adj.global.contrast_pivot = 0.5;
        adj.global.tonemapper_mode = 0;
        let out = render_sized(&recovery_processor, &rdevice, &rqueue, &scene, adj, RW, RH);
        let i = (((RH / 2) * RW + RW / 2) * 4) as usize;
        let (_, c) = oklab_hue_deg(
            out[i] as f32 / 255.0,
            out[i + 1] as f32 / 255.0,
            out[i + 2] as f32 / 255.0,
        );
        c
    };
    let c1 = center_chroma(1);
    let c2 = center_chroma(2);
    println!("recovered disc chroma: v1 {c1:.4}, v2 {c2:.4}");
    assert!(
        c2 > c1 + 0.01,
        "v2 recovery should reconstruct color in the blown disc (v1 {c1:.4}, v2 {c2:.4})"
    );
    assert!(
        c2 > 0.02,
        "recovered disc still gray (chroma {c2:.4})"
    );

    // ---- 4. shadow lift strength + detail preservation ----
    // Left half: deep-shadow checkerboard (0.035 / 0.09 linear). Shadows
    // +100 must (a) actually lift it, (b) keep the checker contrast — the
    // "pulls detail out" property — and beat v1 on lift.
    let mut shscene = vec![0f32; (RW * RH * 4) as usize];
    for y in 0..RH {
        for x in 0..RW {
            let i = ((y * RW + x) * 4) as usize;
            let v = if x < RW / 2 {
                if ((x / 8) + (y / 8)) % 2 == 0 { 0.035 } else { 0.09 }
            } else {
                0.5
            };
            shscene[i] = v;
            shscene[i + 1] = v;
            shscene[i + 2] = v;
            shscene[i + 3] = 1.0;
        }
    }
    // Sample two adjacent cell centers well inside the dark half.
    let dark_cell = (36usize, 132usize); // (x, y) in a 0.035 cell region
    let lite_cell = (44usize, 132usize);
    let sample_lin = |out: &[u8], x: usize, y: usize| -> f32 {
        let i = (y * RW as usize + x) * 4;
        srgb_to_linear1(out[i] as f32 / 255.0)
    };
    let shadows_run = |pv: u32, amount: f32| -> (f32, f32) {
        let mut adj = AllAdjustments::default();
        adj.global.is_raw_image = 1;
        adj.global.process_version = pv;
        adj.global.shadows = amount;
        adj.global.contrast_pivot = 0.5;
        adj.global.tonemapper_mode = 0;
        let out = render_sized(&recovery_processor, &rdevice, &rqueue, &shscene, adj, RW, RH);
        (
            sample_lin(&out, dark_cell.0, dark_cell.1),
            sample_lin(&out, lite_cell.0, lite_cell.1),
        )
    };
    let (d0, l0) = shadows_run(2, 0.0);
    let (d1, l1) = shadows_run(1, 1.0);
    let (d2, l2) = shadows_run(2, 1.0);
    let mean0 = (d0 + l0) / 2.0;
    let lift_v1 = (d1 + l1) / 2.0 / mean0.max(1e-5);
    let lift_v2 = (d2 + l2) / 2.0 / mean0.max(1e-5);
    let contrast = |d: f32, l: f32| (l - d) / (l + d).max(1e-5);
    let c_in = contrast(d0, l0);
    let c_v2 = contrast(d2, l2);
    println!(
        "shadow lift: v1 {lift_v1:.2}x, v2 {lift_v2:.2}x | checker contrast in {c_in:.3} -> v2 {c_v2:.3}"
    );
    assert!(
        lift_v2 > 1.6,
        "v2 shadows too weak: only {lift_v2:.2}x lift at +100"
    );
    assert!(
        lift_v2 > lift_v1 * 1.25,
        "v2 ({lift_v2:.2}x) should clearly out-lift v1 ({lift_v1:.2}x)"
    );
    assert!(
        c_v2 > c_in * 0.7,
        "v2 lift crushed the checker detail: contrast {c_in:.3} -> {c_v2:.3}"
    );
}

/// A Point Color chip with ZERO deltas must be pixel-invisible — the user
/// saw a global "white balance" shift the instant a chip was created.
#[test]
fn zero_delta_point_color_is_identity() {
    let Some((device, queue, limits)) = make_device() else {
        eprintln!("no GPU; skipping");
        return;
    };
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(std::sync::Mutex::new(None)),
    };
    let device = context.device.clone();
    let queue = context.queue.clone();
    let processor = GpuProcessor::new(context, W, H).expect("processor");

    // Gradient spanning warm dark tones (the user's wash palette) through
    // brights, so chip windows overlap real pixels.
    let mut img = vec![0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / (W - 1) as f32;
            let u = y as f32 / (H - 1) as f32;
            img[i] = 0.05 + 0.6 * t;
            img[i + 1] = 0.04 + 0.35 * t * u;
            img[i + 2] = 0.03 + 0.25 * u;
            img[i + 3] = 1.0;
        }
    }

    let mut base_adj = AllAdjustments::default();
    base_adj.global.process_version = 2;
    let base = render(&processor, &device, &queue, &img, base_adj);

    let mut chip_adj = base_adj;
    // Chip at hue 20°, sampled sat 0.5 / val 0.25 — matches the user's
    // logged picks; all deltas zero.
    chip_adj.global.point_colors[0] = [20.0, 0.0, 0.0, 0.0];
    chip_adj.global.point_color_meta[0] = [22.0, 1.0, 0.5, 0.25];
    let with_chip = render(&processor, &device, &queue, &img, chip_adj);

    let max_diff = base
        .iter()
        .zip(with_chip.iter())
        .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs())
        .max()
        .unwrap_or(0);
    assert!(
        max_diff <= 1,
        "zero-delta chip must not change ANY pixel (max channel diff {max_diff})"
    );
}

/// The user's screenshot: a pale chip's luminance edit checkerboarded a
/// low-quality JPEG — near-neutral blocks swing wildly in HUE while
/// looking identical, and a hue gate switched whole blocks in/out of the
/// window. A pale chip must weight look-alike pale blocks EQUALLY, so a
/// luminance shift moves them together instead of amplifying their
/// differences.
#[test]
fn pale_chip_luminance_does_not_amplify_blocks() {
    let Some((device, queue, limits)) = make_device() else {
        eprintln!("no GPU; skipping");
        return;
    };
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(std::sync::Mutex::new(None)),
    };
    let device = context.device.clone();
    let queue = context.queue.clone();
    let processor = GpuProcessor::new(context, W, H).expect("processor");

    // Two "JPEG blocks": both pale, nearly identical to the eye, but with
    // hue-noise (left leans warm, right leans cool) — the block structure
    // of a compressed wash.
    let mut img = vec![0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if x < W / 2 {
                img[i] = 0.62;
                img[i + 1] = 0.58;
                img[i + 2] = 0.55; // warm pale, sat ~0.11, hue ~26°
            } else {
                img[i] = 0.56;
                img[i + 1] = 0.58;
                img[i + 2] = 0.61; // cool pale, sat ~0.08, hue ~216°
            }
            img[i + 3] = 1.0;
        }
    }

    let mut adj = AllAdjustments::default();
    adj.global.process_version = 2;
    let base = render(&processor, &device, &queue, &img, adj);

    // Pale chip (sat 0.10, val 0.78 — the user's actual pick) with a
    // strong negative luminance shift.
    adj.global.point_colors[0] = [20.0, 0.0, 0.0, -0.5];
    adj.global.point_color_meta[0] = [22.0, 1.0, 0.10, 0.78];
    let shifted = render(&processor, &device, &queue, &img, adj);

    let y_mid = (H / 2) as usize;
    let luma = |data: &Vec<u8>, x: usize| -> f32 {
        let i = (y_mid * W as usize + x) * 4;
        0.2126 * data[i] as f32 + 0.7152 * data[i + 1] as f32 + 0.0722 * data[i + 2] as f32
    };
    let (xl, xr) = ((W / 4) as usize, (3 * W / 4) as usize);
    let drop_left = luma(&base, xl) - luma(&shifted, xl);
    let drop_right = luma(&base, xr) - luma(&shifted, xr);

    assert!(
        drop_left > 8.0 && drop_right > 8.0,
        "the pale chip must affect BOTH look-alike blocks (drops {drop_left:.1}/{drop_right:.1})"
    );
    let imbalance = (drop_left - drop_right).abs() / drop_left.max(drop_right);
    assert!(
        imbalance < 0.25,
        "look-alike blocks must move together, not checkerboard (imbalance {imbalance:.2})"
    );
}

/// Devignetting (positive vignette) must LIFT dark corners as an exposure
/// gain — the old code blended toward solid white, painting "white mist"
/// over the corners and destroying their color. A dark red corner must
/// come out brighter but still RED.
#[test]
fn devignette_lifts_corners_without_white_mist() {
    let Some((device, queue, limits)) = make_device() else {
        eprintln!("no GPU; skipping");
        return;
    };
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(std::sync::Mutex::new(None)),
    };
    let device = context.device.clone();
    let queue = context.queue.clone();
    let processor = GpuProcessor::new(context, W, H).expect("processor");

    // Dark red frame corners, mid-gray center.
    let mut img = vec![0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            img[i] = 0.16;
            img[i + 1] = 0.02;
            img[i + 2] = 0.02;
            img[i + 3] = 1.0;
        }
    }

    let mut adj = AllAdjustments::default();
    adj.global.process_version = 2;
    let base = render(&processor, &device, &queue, &img, adj);

    // Working strength: chroma must survive (white mist pushed g/r -> 1;
    // an exposure gain preserves it up to the tonemapper's shoulder).
    adj.global.vignette_amount = 0.6;
    adj.global.vignette_midpoint = 0.3;
    adj.global.vignette_feather = 0.5;
    let lifted = render(&processor, &device, &queue, &img, adj);

    let idx = ((2 * W + 2) * 4) as usize;
    let (r0, g0) = (base[idx] as f32, base[idx + 1] as f32);
    let (r1, g1) = (lifted[idx] as f32, lifted[idx + 1] as f32);

    assert!(r1 > r0 + 10.0, "corner must brighten (r {r0} -> {r1})");
    // Contract: the corner stays predominantly RED. A small neutral
    // floor-lift is deliberate (it's what recovers camera-crushed black
    // corners — measured 1 -> 60 instead of 1 -> 12), but the white-mist
    // bug blended toward solid white (display g/r ~0.9). The threshold
    // separates the accepted floor from the mist failure mode.
    let ratio0 = g0 / r0.max(1.0);
    let ratio1 = g1 / r1.max(1.0);
    assert!(
        ratio1 < 0.6,
        "corner must stay predominantly red (g/r {ratio0:.2} -> {ratio1:.2}; mist was ~0.9)"
    );

    // Full deflection: rescue power — a severely vignetted corner must be
    // lifted dramatically (~4 stops), not politely.
    adj.global.vignette_amount = 1.0;
    let full = render(&processor, &device, &queue, &img, adj);
    let r_full = full[idx] as f32;
    assert!(
        r_full > r1 + 20.0,
        "full deflection must lift far beyond working strength (r {r1} -> {r_full})"
    );
}

/// Measurement (not an assertion suite): print what full-strength
/// devignette actually does to corners at several darkness levels.
#[test]
fn measure_devignette_response() {
    let Some((device, queue, limits)) = make_device() else {
        eprintln!("no GPU; skipping");
        return;
    };
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(std::sync::Mutex::new(None)),
    };
    let device = context.device.clone();
    let queue = context.queue.clone();
    let processor = GpuProcessor::new(context, W, H).expect("processor");

    for corner_level in [0.18f32, 0.05, 0.02, 0.005] {
        let mut img = vec![0f32; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                let v = corner_level;
                img[i] = v;
                img[i + 1] = v;
                img[i + 2] = v;
                img[i + 3] = 1.0;
            }
        }
        let mut adj = AllAdjustments::default();
        adj.global.process_version = 2;
        let base = render(&processor, &device, &queue, &img, adj);
        adj.global.vignette_amount = 1.0;
        adj.global.vignette_midpoint = 0.5;
        adj.global.vignette_feather = 0.5;
        let lifted = render(&processor, &device, &queue, &img, adj);
        let idx = ((2 * W + 2) * 4) as usize;
        eprintln!(
            "corner linear {:.3}: display {} -> {} (of 255)",
            corner_level, base[idx], lifted[idx]
        );
    }
}
