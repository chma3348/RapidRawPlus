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
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test input"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
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
            bytes_per_row: Some(W * 8),
            rows_per_image: Some(H),
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&Default::default());
    let dummy_mask = ImageBuffer::<Luma<u8>, Vec<u8>>::new(W, H);
    let masks = [dummy_mask.clone(), dummy_mask];
    let (data, out_w, out_h, _, _) = processor
        .run(
            &view,
            W,
            H,
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
    assert_eq!((out_w, out_h), (W, H));
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
}
