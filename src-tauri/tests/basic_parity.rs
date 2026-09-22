//! V3's Basic sliders are the previous engine's: set to that engine's Basic
//! tone mapper and without Resolve's transforms, v3 must render every Basic
//! slider as it does, to within a level of rounding. `examples/basic_parity`
//! runs the same check on a real photograph.
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
    let picture = ImageBuffer::from_fn(w, h, |x, y| {
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
    let state = rapidraw_lib::AppState::default();

    for extra in [
        json!({}),
        json!({"exposure": 1.0}),
        json!({"brightness": 1.2}),
        json!({"brightness": -1.0}),
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
        let v3 = render_file(&context, &state, path, &edits, None)
            .unwrap()
            .preview_rgba8();
        let diffs: Vec<f32> = v2
            .chunks(4)
            .zip(v3.pixels())
            .flat_map(|(a, b)| (0..3).map(move |c| (a[c] as f32 - b[c] as f32).abs()))
            .collect();
        let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
        let worst = diffs.iter().cloned().fold(0., f32::max);
        // The average is the parity measure. A handful of deep-shadow pixels
        // differ by a few levels: the previous engine holds its input in
        // half-precision floats, and a steep curve magnifies that rounding.
        assert!(
            mean < 0.6 && worst <= 8.,
            "{extra}: v3 differs from the previous engine by {mean:.2} levels on average, {worst} at worst"
        );
    }
}
