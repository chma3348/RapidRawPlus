//! V3's Basic sliders are the previous engine's: v3 must render every Basic
//! slider as it does, to within a level of rounding — both set to that
//! engine's Basic tone mapper, and as the app renders when Resolve's
//! transforms are installed. `examples/basic_parity` runs the same check on
//! real photographs (`AS_APP=1` for the installed transforms).
use image::{ImageBuffer, Luma, Rgba};
use rapidraw_lib::color_engine::application::render_file;
use rapidraw_lib::gpu_processing::{GpuProcessor, RenderRequest};
use rapidraw_lib::image_processing::{GpuContext, get_all_adjustments_from_json};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn basic_sliders_match_the_previous_engine() {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&Default::default())).expect("GPU required");
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .unwrap();
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
    };
    // A display-referred picture with texture, colour and a full tonal range,
    // so the neighbourhood-driven controls have something to see.
    let (w, h) = (192u32, 128u32);
    // The top two thirds: gradients with fine texture. The bottom third: a
    // row of saturated, skin-like, white and black patches, as a colour chart
    // would have.
    let patches: [[f32; 3]; 12] = [
        [0.9, 0.1, 0.1],
        [0.1, 0.8, 0.2],
        [0.15, 0.2, 0.9],
        [0.95, 0.85, 0.1],
        [0.9, 0.2, 0.8],
        [0.1, 0.8, 0.85],
        [0.85, 0.6, 0.45],
        [0.55, 0.38, 0.28],
        [1.0, 1.0, 1.0],
        [0.02, 0.02, 0.02],
        [0.45, 0.3, 0.6],
        [0.3, 0.5, 0.2],
    ];
    let picture = ImageBuffer::from_fn(w, h, |x, y| {
        if y >= h * 2 / 3 {
            let p = patches[(x as usize * patches.len()) / w as usize];
            let v = |c: f32| (c * 255.).round() as u8;
            return Rgba([v(p[0]), v(p[1]), v(p[2]), 255]);
        }
        let t = x as f32 / (w - 1) as f32;
        let s = y as f32 / (h - 1) as f32;
        let texture = if (x / 4 + y / 4) % 2 == 0 {
            0.06
        } else {
            -0.06
        };
        let v = |c: f32| ((c + texture).clamp(0., 1.) * 255.).round() as u8;
        Rgba([v(t), v(t * (1. - s) + 0.3 * s), v(s * 0.9), 255])
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parity.png");
    picture.save(&path).unwrap();
    let path = path.to_str().unwrap();

    let texels: Vec<half::f16> = picture
        .pixels()
        .flat_map(|p| [p[0], p[1], p[2], 255].map(|v| half::f16::from_f32(v as f32 / 255.)))
        .collect();
    let texture = context.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
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
    context.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&texels),
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
    let processor = GpuProcessor::new(context.clone(), w, h).unwrap();
    let blank = ImageBuffer::<Luma<u8>, Vec<u8>>::new(w, h);
    let masks = [blank.clone(), blank];
    // Twice: as a display picture, and as the app renders one when Resolve's
    // transforms are installed — turned into scene data on the way in and
    // rendered back out, with the Basic controls run on its display values.
    // The stand-in transforms are the encoding change between sRGB and
    // DaVinci Intermediate, so a correct round trip gives the previous
    // engine's picture back. It is the case where values past white used to
    // reach the brightness curve and break it into bands.
    let plain = rapidraw_lib::AppState::default();
    let resolve = rapidraw_lib::AppState::default();
    let (input, output) = (dir.path().join("in.cube"), dir.path().join("out.cube"));
    write_transforms(&input, &output);
    *resolve.input_transform.lock().unwrap() = Some(input);
    *resolve.output_transform.lock().unwrap() = Some(output);
    for (state, tone_mapper) in [(&plain, "basic"), (&resolve, "resolve")] {
        // Through the transforms, near-pure colours cannot be carried
        // exactly: a 64-point lattice interpolated beside a channel near
        // zero, where the sRGB encoding is steepest, misses by up to ~30
        // levels even with every slider at zero (photo-like colours: ~0.3 on
        // average). That is the capture's resolution, not these controls, so
        // in that mode the saturated patches — checked exactly without the
        // transforms — and channels near zero are left out.
        let mut exact: Option<Vec<bool>> = None;
        // What the transforms cost with every slider at zero (the first
        // setting), so a slider is judged on what it adds.
        let mut baseline: Option<(f32, f32)> = None;
        for extra in [
            json!({}),
            json!({"exposure": 1.0}),
            json!({"exposure": -2.0}),
            json!({"brightness": 2.0}),
            json!({"brightness": 1.2}),
            json!({"brightness": -1.0}),
            json!({"brightness": -2.0}),
            json!({"contrast": 60, "contrastPivot": 35}),
            json!({"contrast": -60}),
            json!({"highlights": -80}),
            json!({"highlights": 70}),
            json!({"shadows": 80}),
            json!({"shadows": -70}),
            json!({"whites": 60}),
            json!({"whites": -60}),
            json!({"blacks": 70}),
            json!({"blacks": -70}),
        ] {
            let mut edits = json!({"processVersion": 2, "toneMapper": "basic"});
            for (k, v) in extra.as_object().unwrap() {
                edits[k] = v.clone();
            }
            let (v2, ..) = processor
                .run(
                    &view,
                    w,
                    h,
                    RenderRequest {
                        adjustments: get_all_adjustments_from_json(&edits, false, None),
                        mask_bitmaps: &masks,
                        lut: None,
                        roi: None,
                    },
                    false,
                    false,
                )
                .unwrap();
            edits["processVersion"] = json!(3);
            edits["toneMapper"] = json!(tone_mapper);
            let v3 = render_file(&context, state, path, &edits, None)
                .unwrap()
                .preview_rgba8();
            let exact = exact.get_or_insert_with(|| {
                picture
                    .enumerate_pixels()
                    .map(|(_, y, p)| {
                        tone_mapper != "resolve"
                            || (y < h * 2 / 3 && p.0[..3].iter().all(|v| *v >= 24))
                    })
                    .collect()
            });
            // And, per setting, results pushed to the lattice's edges (a
            // channel clipped to white or near black): the same limit.
            let inside = |a: &[u8]| {
                tone_mapper != "resolve" || a[..3].iter().all(|v| (24..=250).contains(v))
            };
            let diffs: Vec<f32> = v2
                .chunks(4)
                .zip(v3.pixels())
                .zip(exact.iter())
                .filter(|((a, _), keep)| **keep && inside(a))
                .flat_map(|((a, b), _)| (0..3).map(move |c| (a[c] as f32 - b[c] as f32).abs()))
                .collect();
            let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
            let worst = diffs.iter().cloned().fold(0., f32::max);
            if std::env::var_os("PARITY_DEBUG").is_some() && worst > 8. {
                for (i, (a, b)) in v2.chunks(4).zip(v3.pixels()).enumerate() {
                    if exact[i]
                        && inside(a)
                        && (0..3).any(|c| (a[c] as f32 - b[c] as f32).abs() > 8.)
                    {
                        let (x, y) = (i as u32 % w, i as u32 / w);
                        eprintln!(
                            "{tone_mapper} {extra} at {x},{y}: in {:?} v2 {:?} v3 {:?}",
                            picture.get_pixel(x, y).0,
                            &a[..3],
                            &b.0[..3]
                        );
                        break;
                    }
                }
            }
            // The average is the parity measure. A handful of deep-shadow pixels
            // differ by a few levels: the previous engine holds its input in
            // half-precision floats, and a steep curve magnifies that rounding.
            // Through the transforms, what a slider adds to the transforms'
            // own error at neutral, on the bulk of the picture.
            let mut sorted = diffs.clone();
            sorted.sort_by(f32::total_cmp);
            let p99 = sorted[sorted.len() * 99 / 100];
            let (base_mean, base_p99) = *baseline.get_or_insert((mean, p99));
            let within = if tone_mapper == "resolve" {
                // p99 may grow by 4: a strong shadow lift multiplies dark
                // values — and the transforms' error in them — by up to 3x.
                mean <= base_mean + 0.5 && p99 <= base_p99 + 4.
            } else {
                mean < 0.8 && worst <= 8.
            };
            assert!(
                within,
                "{tone_mapper} {extra}: v3 differs from the previous engine by {mean:.2} levels on average, {p99} at the 99th percentile, {worst} at worst"
            );
        }
    }
}

/// A stand-in for Resolve's transforms with the property that matters here:
/// like Resolve's, the input transform puts display white well past scene
/// white (at 4.0), with a shoulder the output transform undoes exactly —
/// `display = x·5 / (4·(x + 1))`, per channel in linear sRGB. On 65-point
/// lattices, input in sRGB code values, scene side in DaVinci Intermediate
/// and DaVinci Wide Gamut, as the captured ones are.
fn write_transforms(input: &std::path::Path, output: &std::path::Path) {
    use rapidraw_lib::color_engine::{
        config::{Primaries, Transfer},
        spaces,
    };
    let n = 65usize;
    let to_dwg = spaces::conversion(Primaries::Srgb, Primaries::DavinciWideGamut);
    let to_srgb = to_dwg.inverse();
    let decode_srgb = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode_srgb = |v: f64| {
        let v = v.clamp(0., 1.);
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    };
    let to_scene = |d: f64| 4. * d / (5. - 4. * d.min(1.));
    let to_display = |x: f64| x.max(0.) * 5. / (4. * (x.max(0.) + 1.));
    let (mut a, mut b) = (format!("LUT_3D_SIZE {n}\n"), format!("LUT_3D_SIZE {n}\n"));
    for bi in 0..n {
        for gi in 0..n {
            for ri in 0..n {
                let c = [ri, gi, bi].map(|i| i as f64 / (n - 1) as f64);
                let scene = c.map(|v| to_scene(decode_srgb(v)));
                let lin = to_dwg * glam::DVec3::from_array(scene);
                let e = lin.to_array().map(spaces::encode_intermediate);
                a.push_str(&format!("{} {} {}\n", e[0], e[1], e[2]));
                let dwg = c.map(|v| spaces::decode(v, Transfer::DavinciIntermediate));
                let srgb = (to_srgb * glam::DVec3::from_array(dwg)).to_array();
                let d = srgb.map(|x| encode_srgb(to_display(x)));
                b.push_str(&format!("{} {} {}\n", d[0], d[1], d[2]));
            }
        }
    }
    std::fs::write(input, a).unwrap();
    std::fs::write(output, b).unwrap();
}
