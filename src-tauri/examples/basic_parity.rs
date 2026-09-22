//! Do v3's Basic sliders behave like the previous engine's?
//!
//! Renders one photograph through both engines with each Basic slider moved,
//! v3 set to the previous engine's Basic tone mapper and without Resolve's
//! transforms, so the only differences left are the ones v3 introduces. Both
//! see the same decoded pixels. Prints the difference in 8-bit levels, and
//! with an output directory writes side-by-side sheets (v2 left, v3 right).
//!
//!   cargo run --release --example basic_parity -- PHOTO [OUT_DIR]
use anyhow::Result;
use image::{DynamicImage, ImageBuffer, Luma};
use rapidraw_lib::color_engine::application::render_file;
use rapidraw_lib::gpu_processing::{GpuProcessor, RenderRequest};
use rapidraw_lib::image_processing::{GpuContext, get_all_adjustments_from_json};
use serde_json::json;
use std::sync::{Arc, Mutex};

const SIZE: u32 = 800;

fn main() -> Result<()> {
    let path = std::env::args().nth(1).expect("a photograph");
    let out = std::env::args().nth(2);
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&Default::default()))?;
    let limits = adapter.limits();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits.clone(),
        ..Default::default()
    }))?;
    let context = GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
    };
    let state = rapidraw_lib::AppState::default();

    // The same pixels v3 prepares: its decode, its downscale.
    let bytes = std::fs::read(&path)?;
    let raw = rapidraw_lib::formats::is_raw_file(&path);
    let frame = if raw {
        rapidraw_lib::color_engine::raw::decode_raw(&bytes, false, || Ok(()))?
    } else {
        rapidraw_lib::color_engine::input::decode_profiled_photo(&bytes)?
    };
    let small = rapidraw_lib::image_processing::downscale_f32_image(
        &DynamicImage::ImageRgba32F(frame.pixels),
        SIZE,
        SIZE,
    )
    .to_rgba32f();
    let (w, h) = small.dimensions();
    // The previous engine takes display-referred pictures encoded.
    let encode = |v: f32| {
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.max(0.).powf(1. / 2.4) - 0.055
        }
    };
    let texels: Vec<half::f16> = small
        .pixels()
        .flat_map(|p| {
            let c = |v: f32| half::f16::from_f32(if raw { v } else { encode(v) });
            [c(p[0]), c(p[1]), c(p[2]), half::f16::ONE]
        })
        .collect();
    let texture = context.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("parity input"),
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
    let processor = GpuProcessor::new(context.clone(), w, h).map_err(anyhow::Error::msg)?;
    let blank = ImageBuffer::<Luma<u8>, Vec<u8>>::new(w, h);
    let masks = [blank.clone(), blank];

    let settings: &[(&str, serde_json::Value)] = &[
        ("neutral", json!({})),
        ("EV shift +1", json!({"exposure": 1.0})),
        ("Exposure +1", json!({"brightness": 1.0})),
        ("Exposure -1", json!({"brightness": -1.0})),
        ("Contrast +50", json!({"contrast": 50})),
        ("Contrast -50", json!({"contrast": -50})),
        ("Highlights -60", json!({"highlights": -60})),
        ("Highlights +60", json!({"highlights": 60})),
        ("Shadows +60", json!({"shadows": 60})),
        ("Shadows -60", json!({"shadows": -60})),
        ("Whites +50", json!({"whites": 50})),
        ("Whites -50", json!({"whites": -50})),
        ("Blacks +50", json!({"blacks": 50})),
        ("Blacks -50", json!({"blacks": -50})),
    ];
    println!("{:18} {:>8} {:>8} {:>8}", "setting", "mean", "p99", "max");
    let mut sheet_rows = Vec::new();
    for (name, extra) in settings {
        let mut edits = json!({"processVersion": 2, "toneMapper": "basic"});
        for (k, v) in extra.as_object().unwrap() {
            edits[k] = v.clone();
        }
        let adjustments = get_all_adjustments_from_json(&edits, raw, None);
        let (v2, ..) = processor
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
            .map_err(anyhow::Error::msg)?;
        let mut v3_edits = edits.clone();
        v3_edits["processVersion"] = json!(3);
        let v3 = render_file(&context, &state, &path, &v3_edits, Some(SIZE))?.preview_rgba8();
        assert_eq!(v3.dimensions(), (w, h), "the engines saw different sizes");
        let mut diffs: Vec<f32> = v2
            .chunks(4)
            .zip(v3.pixels())
            .flat_map(|(a, b)| (0..3).map(move |c| (a[c] as f32 - b[c] as f32).abs()))
            .collect();
        diffs.sort_by(f32::total_cmp);
        let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
        println!(
            "{name:18} {mean:>8.2} {:>8.1} {:>8.1}",
            diffs[diffs.len() * 99 / 100],
            diffs[diffs.len() - 1]
        );
        if out.is_some() {
            let left = image::RgbaImage::from_raw(w, h, v2).unwrap();
            let mut row = image::RgbaImage::new(w * 2, h);
            image::imageops::overlay(&mut row, &left, 0, 0);
            image::imageops::overlay(&mut row, &v3, w as i64, 0);
            sheet_rows.push((name.to_string(), row));
        }
    }
    if let Some(dir) = out {
        for (name, row) in sheet_rows {
            let file = format!("{dir}/parity_{}.jpg", name.replace([' ', '+'], "_"));
            DynamicImage::ImageRgba8(row).to_rgb8().save(file)?;
        }
    }
    Ok(())
}
