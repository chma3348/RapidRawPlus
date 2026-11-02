//! The Magic Wand (contiguous colour select) against the global colour range,
//! on real photos, through the app's own mask code. Each case writes:
//!
//! - `<n>_side.jpg`: contiguous | colour range, at preview size, with selected
//!   % and timings;
//! - `<n>_zoom.png`: preview-size crops along the wand's edge, 3x, at the
//!   Retouch defaults and with the matte filters off;
//! - a line of numbers: timings (preview and full size, first run and with the
//!   flood grid cached), the edge error against a full-resolution flood, and
//!   the fill-mask check on a turned, flipped and cropped copy of the photo
//!   (the mask a fill builds, placed on the photo, against the render's).
//!
//! cargo run --release --example wand_select -- <out dir> "<photo>|x,y x,y-|tolerance" ...
//!
//! Points are fractions of the upright photo; a trailing `-` makes a point a
//! subtract (Option-click) sample.
use std::time::Instant;

use darkroom_index::mask_generation::{
    MaskDefinition, Orientation, WandOptions, clicks_to_source, generate_mask_bitmap,
    generate_photo_mask_bitmap, override_wand_options, place_source_mask,
};
use image::{DynamicImage, GrayImage, ImageReader, Luma, Rgb, RgbImage};
use serde_json::{Value, json};

const PREVIEW_LONG: u32 = 1920;

fn mask_def(params: Value) -> MaskDefinition {
    serde_json::from_value(json!({
        "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100,
        "adjustments": {},
        "subMasks": [{"id": "s", "type": "color", "visible": true, "mode": "additive",
            "parameters": params}],
    }))
    .unwrap()
}

/// Retouch's defaults for a new colour selection.
fn params(points: &[(f64, f64, bool)], tolerance: f32, contiguous: bool, filters: bool) -> Value {
    let samples: Vec<Value> = points
        .iter()
        .map(|(x, y, sub)| json!({"x": x, "y": y, "subtract": sub}))
        .collect();
    let (solidify, clean, feather) = if filters { (40, 15, 25) } else { (0, 0, 0) };
    json!({
        "targetX": points[0].0, "targetY": points[0].1, "samples": samples,
        "tolerance": tolerance, "solidify": solidify, "clean": clean, "grow": 0,
        "feather": feather, "contiguous": contiguous,
        "rotation": 0.0, "flipHorizontal": false, "flipVertical": false, "orientationSteps": 0,
    })
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed().as_secs_f64() * 1000.0)
}

fn coverage(mask: &GrayImage) -> f64 {
    mask.pixels().map(|p| p[0] as f64).sum::<f64>() / (255.0 * mask.len() as f64) * 100.0
}

fn iou(a: &GrayImage, b: &GrayImage) -> f64 {
    let (mut both, mut either) = (0u64, 0u64);
    for (p, q) in a.pixels().zip(b.pixels()) {
        both += (p[0] > 127 && q[0] > 127) as u64;
        either += (p[0] > 127 || q[0] > 127) as u64;
    }
    both as f64 / either.max(1) as f64
}

/// Pixels that differ by more than a quarter, as a share of the selection.
fn edge_error(a: &GrayImage, reference: &GrayImage) -> f64 {
    let mut off = 0u64;
    let mut sel = 0u64;
    for (p, q) in a.pixels().zip(reference.pixels()) {
        off += ((p[0] as i32 - q[0] as i32).abs() > 64) as u64;
        sel += (q[0] > 127) as u64;
    }
    off as f64 / sel.max(1) as f64 * 100.0
}

fn tint(photo: &RgbImage, mask: &GrayImage) -> RgbImage {
    let mut out = photo.clone();
    for (x, y, p) in out.enumerate_pixels_mut() {
        let a = mask.get_pixel(x, y)[0] as f32 / 255.0 * 0.6;
        p[0] = (p[0] as f32 * (1.0 - a) + 255.0 * a) as u8;
        p[1] = (p[1] as f32 * (1.0 - a)) as u8;
        p[2] = (p[2] as f32 * (1.0 - a) + 40.0 * a) as u8;
    }
    out
}

fn mark(img: &mut RgbImage, x: f64, y: f64, colour: Rgb<u8>) {
    let (cx, cy) = (x as i64, y as i64);
    for d in -9i64..=9 {
        for t in -1i64..=1 {
            for (px, py) in [(cx + d, cy + t), (cx + t, cy + d)] {
                if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                    img.put_pixel(px as u32, py as u32, colour);
                }
            }
        }
    }
}

/// A bar of `fraction` (0..1) along the bottom, as a crude readout that
/// survives without fonts: red = contiguous, blue = range.
fn bar(img: &mut RgbImage, fraction: f64, colour: Rgb<u8>) {
    let h = img.height();
    let len = (fraction.clamp(0.0, 1.0) * img.width() as f64) as u32;
    for y in h.saturating_sub(10)..h {
        for x in 0..len {
            img.put_pixel(x, y, colour);
        }
    }
}

/// Up to six points spread along the mask's edge.
fn edge_points(mask: &GrayImage) -> Vec<(u32, u32)> {
    let (w, h) = mask.dimensions();
    let mut edge = Vec::new();
    for y in (8..h.saturating_sub(8)).step_by(3) {
        for x in (8..w.saturating_sub(8)).step_by(3) {
            let v = mask.get_pixel(x, y)[0];
            if (60..196).contains(&v) {
                edge.push((x, y));
            }
        }
    }
    let mut chosen: Vec<(u32, u32)> = Vec::new();
    // Spread over the whole edge: the last points found first (the edge
    // lowest in the frame, a skyline), then evenly through the rest.
    let stride = (edge.len() / 48).max(1);
    let order: Vec<&(u32, u32)> = edge.iter().rev().step_by(stride).collect();
    for p in order {
        if chosen.len() >= 6 {
            break;
        }
        let far = chosen.iter().all(|c| {
            let dx = c.0 as i64 - p.0 as i64;
            let dy = c.1 as i64 - p.1 as i64;
            dx * dx + dy * dy > 200 * 200
        });
        if far {
            chosen.push(*p);
        }
    }
    chosen
}

fn zoom_sheet(photo: &RgbImage, masks: &[&GrayImage], at: &[(u32, u32)]) -> RgbImage {
    const R: u32 = 60;
    const Z: u32 = 3;
    let tile = 2 * R * Z;
    let mut sheet = RgbImage::from_pixel(
        tile * at.len().max(1) as u32,
        tile * (masks.len() as u32 + 1),
        Rgb([0, 0, 0]),
    );
    let (w, h) = photo.dimensions();
    for (i, (cx, cy)) in at.iter().enumerate() {
        let x0 = cx.saturating_sub(R).min(w - 2 * R);
        let y0 = cy.saturating_sub(R).min(h - 2 * R);
        for y in 0..2 * R * Z {
            for x in 0..2 * R * Z {
                let (sx, sy) = (x0 + x / Z, y0 + y / Z);
                let p = *photo.get_pixel(sx, sy);
                sheet.put_pixel(i as u32 * tile + x, y, p);
                for (row, mask) in masks.iter().enumerate() {
                    let a = mask.get_pixel(sx, sy)[0] as f32 / 255.0;
                    let q = Rgb([
                        (p[0] as f32 * (1.0 - a * 0.7) + 255.0 * a * 0.7) as u8,
                        (p[1] as f32 * (1.0 - a * 0.7)) as u8,
                        (p[2] as f32 * (1.0 - a * 0.7)) as u8,
                    ]);
                    sheet.put_pixel(i as u32 * tile + x, (row as u32 + 1) * tile + y, q);
                }
            }
        }
    }
    sheet
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let out = std::path::Path::new(&args[1]);
    std::fs::create_dir_all(out)?;
    // WAND_CORE / WAND_RIM / WAND_GAP try other flood settings than the app's.
    let mut app = WandOptions::default();
    if let Some(v) = std::env::var("WAND_CORE").ok().and_then(|v| v.parse().ok()) {
        app.core = v;
    }
    if let Some(v) = std::env::var("WAND_RIM").ok().and_then(|v| v.parse().ok()) {
        app.rim = v;
    }
    if let Some(v) = std::env::var("WAND_GAP").ok().and_then(|v| v.parse().ok()) {
        app.gap_close = v;
    }
    if let Some(v) = std::env::var("WAND_EDGE").ok().and_then(|v| v.parse().ok()) {
        app.edge_k = v;
    }
    println!("flood settings: {app:?}");
    override_wand_options(Some(app));
    for (n, case) in args[2..].iter().enumerate() {
        let parts: Vec<&str> = case.split('|').collect();
        let name = format!(
            "{n}_{}",
            std::path::Path::new(parts[0])
                .file_stem()
                .unwrap()
                .to_string_lossy()
        );
        let mut decoder = ImageReader::open(parts[0])?.into_decoder()?;
        let orientation = image::ImageDecoder::orientation(&mut decoder)?;
        let mut photo = DynamicImage::from_decoder(decoder)?;
        photo.apply_orientation(orientation);
        // The app samples a float picture.
        let warped = DynamicImage::ImageRgb32F(photo.to_rgb32f());
        let (w, h) = (warped.width(), warped.height());
        let tolerance: f32 = parts[2].parse()?;
        let points: Vec<(f64, f64, bool)> = parts[1]
            .split_whitespace()
            .map(|p| {
                let sub = p.ends_with('-');
                let (x, y) = p.trim_end_matches(['-', '+']).split_once(',').unwrap();
                (
                    x.parse::<f64>().unwrap() * w as f64,
                    y.parse::<f64>().unwrap() * h as f64,
                    sub,
                )
            })
            .collect();
        let rgb = photo.to_rgb8();
        for (x, y, _) in &points {
            println!(
                "{name}: sample at ({x:.0}, {y:.0}) is {:?}",
                rgb.get_pixel(*x as u32, *y as u32).0
            );
        }

        let scale = PREVIEW_LONG as f32 / w.max(h) as f32;
        let (pw, ph) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let upright = Orientation::default();
        let render = |p: Value, sw: u32, sh: u32, s: f32| {
            generate_photo_mask_bitmap(&mask_def(p), sw, sh, s, (0.0, 0.0), Some(&warped), upright)
                .unwrap_or_else(|| GrayImage::new(sw, sh))
        };

        // Timings: the first run builds the flood grid; later ones (another
        // tolerance) reuse it.
        let (wand, ms_first) =
            timed(|| render(params(&points, tolerance, true, true), pw, ph, scale));
        let mut ms_live = Vec::new();
        for t in [tolerance - 5.0, tolerance + 5.0, tolerance] {
            let (_, ms) = timed(|| render(params(&points, t.max(1.0), true, true), pw, ph, scale));
            ms_live.push(ms);
        }
        let (range, ms_range) =
            timed(|| render(params(&points, tolerance, false, true), pw, ph, scale));
        let wand_raw = render(params(&points, tolerance, true, false), pw, ph, scale);
        let (wand_full, ms_full) =
            timed(|| render(params(&points, tolerance, true, false), w, h, 1.0));
        let (range_full, ms_range_full) =
            timed(|| render(params(&points, tolerance, false, false), w, h, 1.0));

        // Edge error: the grid flood against a flood at full resolution
        // without gap closing, filters off, at full size and at preview.
        // The rim reaches as far in source pixels as the app's does.
        let cell = w.max(h).div_ceil(app.grid_long);
        override_wand_options(Some(WandOptions {
            grid_long: w.max(h),
            gap_close: 0,
            rim: app.rim * cell,
            ..app
        }));
        let (reference_full, ms_ref) =
            timed(|| render(params(&points, tolerance, true, false), w, h, 1.0));
        let reference_raw = render(params(&points, tolerance, true, false), pw, ph, scale);
        override_wand_options(Some(WandOptions {
            gap_close: 0,
            ..app
        }));
        let no_stop_raw = render(params(&points, tolerance, true, false), pw, ph, scale);
        override_wand_options(Some(app));

        // Fills on a turned, flipped, straightened and cropped photo.
        let geometry = json!({"orientationSteps": 1, "flipHorizontal": true,
            "flipVertical": false, "rotation": 3.5});
        let shown = Orientation::of_photo(&geometry);
        // Where each sample point is on the photo as shown: carry a dot.
        let display_points: Vec<(f64, f64, bool)> = points
            .iter()
            .map(|&(x, y, sub)| {
                let mut dot = GrayImage::new(w, h);
                for dy in 0..9 {
                    for dx in 0..9 {
                        let (px, py) = (x as u32 + dx - 4, y as u32 + dy - 4);
                        if px < w && py < h {
                            dot.put_pixel(px, py, Luma([255]));
                        }
                    }
                }
                let placed = place_source_mask(&dot, h, w, 1.0, (0.0, 0.0), shown);
                let (mut sx, mut sy, mut sw) = (0f64, 0f64, 0f64);
                for (px, py, v) in placed.enumerate_pixels() {
                    sx += px as f64 * v[0] as f64;
                    sy += py as f64 * v[0] as f64;
                    sw += v[0] as f64;
                }
                (sx / sw, sy / sw, sub)
            })
            .collect();
        // Uncropped frame is h x w after the quarter turn.
        let crop = (h as f32 * 0.06, w as f32 * 0.05);
        let (cw, ch) = (h as f32 * 0.86, w as f32 * 0.88);
        let s2 = PREVIEW_LONG as f32 / cw.max(ch);
        let (dw, dh) = ((cw * s2) as u32, (ch * s2) as u32);
        let mut turned = params(&display_points, tolerance, true, true);
        for (k, v) in geometry.as_object().unwrap() {
            turned[k] = v.clone();
        }
        let render_mask = generate_photo_mask_bitmap(
            &mask_def(turned.clone()),
            dw,
            dh,
            s2,
            (crop.0 * s2, crop.1 * s2),
            Some(&warped),
            shown,
        )
        .unwrap_or_else(|| GrayImage::new(dw, dh));
        let fill_mask = |p: Value| {
            let src = generate_mask_bitmap(&mask_def(p), w, h, 1.0, (0.0, 0.0), Some(&warped))
                .unwrap_or_else(|| GrayImage::new(w, h));
            place_source_mask(&src, dw, dh, s2, (crop.0 * s2, crop.1 * s2), shown)
        };
        let mut before = turned.clone();
        for k in ["rotation", "orientationSteps"] {
            before[k] = json!(0);
        }
        before["flipHorizontal"] = json!(false);
        before["flipVertical"] = json!(false);
        let fill_before = fill_mask(before);
        let mut after = turned.clone();
        clicks_to_source(&mut after, (w, h));
        let fill_after = fill_mask(after);

        println!(
            "{name}: tol {tolerance} | wand {:.1}% vs range {:.1}% (preview) | preview ms: wand first {ms_first:.0}, live {:?}, range {ms_range:.0} | full ms: wand {ms_full:.0}, range {ms_range_full:.0}, full-res flood {ms_ref:.0} | edge vs full-res flood: full IoU {:.4} err {:.2}%, preview IoU {:.4} err {:.2}% | gap closing: preview IoU vs none {:.4} | full-size wand {:.1}% range {:.1}% | fill vs render on turned+cropped: before IoU {:.3}, after IoU {:.3} (render {:.1}%)",
            coverage(&wand),
            coverage(&range),
            ms_live.iter().map(|m| m.round() as i64).collect::<Vec<_>>(),
            iou(&wand_full, &reference_full),
            edge_error(&wand_full, &reference_full),
            iou(&wand_raw, &reference_raw),
            edge_error(&wand_raw, &reference_raw),
            iou(&wand_raw, &no_stop_raw),
            coverage(&wand_full),
            coverage(&range_full),
            iou(&fill_before, &render_mask),
            iou(&fill_after, &render_mask),
            coverage(&render_mask),
        );

        let preview = image::imageops::resize(
            &photo.to_rgb8(),
            pw,
            ph,
            image::imageops::FilterType::Triangle,
        );
        let mut left = tint(&preview, &wand);
        let mut right = tint(&preview, &range);
        bar(&mut left, coverage(&wand) / 100.0, Rgb([255, 40, 40]));
        bar(&mut right, coverage(&range) / 100.0, Rgb([60, 120, 255]));
        for (x, y, sub) in &points {
            let c = if *sub {
                Rgb([0, 160, 255])
            } else {
                Rgb([0, 255, 0])
            };
            mark(&mut left, x * scale as f64, y * scale as f64, c);
            mark(&mut right, x * scale as f64, y * scale as f64, c);
        }
        let mut side = RgbImage::new(pw * 2 + 8, ph);
        image::imageops::replace(&mut side, &left, 0, 0);
        image::imageops::replace(&mut side, &right, pw as i64 + 8, 0);
        side.save(out.join(format!("{name}_side.jpg")))?;

        let at = edge_points(&wand_raw);
        zoom_sheet(&preview, &[&wand, &wand_raw, &range], &at)
            .save(out.join(format!("{name}_zoom.png")))?;

        // The turned photo: render | fill before | fill after.
        let mut turn_sheet = RgbImage::from_pixel(dw * 3 + 16, dh, Rgb([0, 0, 0]));
        for (i, m) in [&render_mask, &fill_before, &fill_after].iter().enumerate() {
            let g = DynamicImage::ImageLuma8((*m).clone()).to_rgb8();
            image::imageops::replace(&mut turn_sheet, &g, (i as u32 * (dw + 8)) as i64, 0);
        }
        image::imageops::resize(
            &turn_sheet,
            turn_sheet.width() / 2,
            turn_sheet.height() / 2,
            image::imageops::FilterType::Triangle,
        )
        .save(out.join(format!("{name}_turned.jpg")))?;
    }
    Ok(())
}
