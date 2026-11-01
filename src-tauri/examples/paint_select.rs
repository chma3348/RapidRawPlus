//! Remove's "Detect objects" brush, before and after holding the selection to
//! the paint. Each case is `photo|x,y x,y ...|radius`, points as fractions of
//! the upright photo; each run writes old | new side by side.
//!
//! ORT_DYLIB_PATH=resources/libonnxruntime.dylib cargo run --release --example paint_select -- \
//!   <models dir> <out dir> "<photo>|0.5,0.4 0.5,0.6|40" ...
use std::sync::Mutex;

use darkroom_index::ai_processing::generate_image_embeddings;
use darkroom_index::subject_selection::{paint_points, select, select_painted};
use image::{DynamicImage, GrayImage, ImageReader, Luma, Rgb, RgbImage};
use ort::session::Session;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let models = std::path::Path::new(&args[1]);
    let out = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(out)?;
    let encoder = Mutex::new(
        Session::builder()?.commit_from_file(models.join("sam_vit_b_01ec64_encoder.onnx"))?,
    );
    let decoder = Mutex::new(
        Session::builder()?.commit_from_file(models.join("sam_vit_b_01ec64_decoder.onnx"))?,
    );
    for (n, case) in args[3..].iter().enumerate() {
        let parts: Vec<&str> = case.split('|').collect();
        let mut decoder_img = ImageReader::open(parts[0])?.into_decoder()?;
        let orientation = image::ImageDecoder::orientation(&mut decoder_img)?;
        let mut photo = DynamicImage::from_decoder(decoder_img)?;
        photo.apply_orientation(orientation);
        let (w, h) = (photo.width(), photo.height());
        let radius: f64 = parts[2].parse()?;
        let pts: Vec<(f64, f64)> = parts[1]
            .split_whitespace()
            .map(|p| {
                let (x, y) = p.split_once(',').unwrap();
                (
                    x.parse::<f64>().unwrap() * w as f64,
                    y.parse::<f64>().unwrap() * h as f64,
                )
            })
            .collect();
        let mut paint = GrayImage::new(w, h);
        for pair in pts.windows(2) {
            let dist = ((pair[1].0 - pair[0].0).powi(2) + (pair[1].1 - pair[0].1).powi(2)).sqrt();
            let steps = (dist / (radius / 2.0)).ceil().max(1.0) as usize;
            for s in 0..=steps {
                let t = s as f64 / steps as f64;
                let c = (
                    pair[0].0 + (pair[1].0 - pair[0].0) * t,
                    pair[0].1 + (pair[1].1 - pair[0].1) * t,
                );
                imageproc::drawing::draw_filled_circle_mut(
                    &mut paint,
                    (c.0 as i32, c.1 as i32),
                    radius as i32,
                    Luma([255]),
                );
            }
        }
        let excluded = GrayImage::new(w, h);
        let embeddings = generate_image_embeddings(&photo, &encoder)?;
        let t = std::time::Instant::now();
        let old = select(
            &decoder,
            &embeddings,
            &paint_points(&paint, &excluded),
            None,
            true,
        )?;
        let old_ms = t.elapsed().as_millis();
        let t = std::time::Instant::now();
        let new = select_painted(&decoder, &embeddings, &paint, &excluded, radius)?;
        let new_ms = t.elapsed().as_millis();
        let share = |m: &GrayImage| {
            100.0 * m.pixels().filter(|p| p[0] > 127).count() as f64 / (w * h) as f64
        };
        println!(
            "{n}: {}  painted {:.1}%  old {:.1}% ({old_ms} ms)  new {:.1}% ({new_ms} ms)",
            parts[0].rsplit('/').next().unwrap(),
            share(&paint),
            share(&old),
            share(&new),
        );
        let rgb = photo.to_rgb8();
        let view = |m: &GrayImage| {
            RgbImage::from_fn(w, h, |x, y| {
                let p = rgb.get_pixel(x, y);
                let a = m.get_pixel(x, y)[0] as f32 / 255.0 * 0.55;
                let mut c = [
                    p[0] as f32 * (1.0 - a) + 255.0 * a,
                    p[1] as f32 * (1.0 - a),
                    p[2] as f32 * (1.0 - a),
                ];
                if paint.get_pixel(x, y)[0] > 0 {
                    c = [c[0] * 0.6 + 100.0, c[1] * 0.6 + 100.0, c[2] * 0.6];
                }
                Rgb(c.map(|v| v.clamp(0.0, 255.0) as u8))
            })
        };
        let (a, b) = (view(&old), view(&new));
        let tw = 900u32;
        let th = (h as f64 * tw as f64 / w as f64) as u32;
        let a = image::imageops::resize(&a, tw, th, image::imageops::FilterType::Triangle);
        let b = image::imageops::resize(&b, tw, th, image::imageops::FilterType::Triangle);
        let mut sheet = RgbImage::new(tw * 2 + 10, th);
        image::imageops::replace(&mut sheet, &a, 0, 0);
        image::imageops::replace(&mut sheet, &b, (tw + 10) as i64, 0);
        sheet.save(out.join(format!("case{n}.jpg")))?;
    }
    Ok(())
}
