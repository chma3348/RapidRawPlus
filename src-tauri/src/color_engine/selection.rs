//! Global range inspection uses the exact post-primary, pre-selective float
//! stage, not a screen capture or a display-encoded preview pixel.
use super::{application, config::Primaries, controls::ColorRange, spaces};
use crate::{AppState, image_processing::GpuContext};
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ImageBuffer, Luma};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub image: String,
    pub selection: String,
    pub center: Option<[f32; 3]>,
}

pub fn lab(rgb: glam::DVec3) -> [f32; 3] {
    let l = (0.4122214708 * rgb.x + 0.5363325363 * rgb.y + 0.0514459929 * rgb.z).cbrt();
    let m = (0.2119034982 * rgb.x + 0.6806995451 * rgb.y + 0.1073969566 * rgb.z).cbrt();
    let s = (0.0883024619 * rgb.x + 0.2817188376 * rgb.y + 0.6299787005 * rgb.z).cbrt();
    [
        (0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s) as f32,
        (1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s) as f32,
        (0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s) as f32,
    ]
}
fn smooth(x: f32) -> f32 {
    let t = x.clamp(0., 1.);
    t * t * (3. - 2. * t)
}
pub fn weight(lab: [f32; 3], ranges: &[ColorRange], selected: usize) -> f32 {
    let chroma = lab[1].hypot(lab[2]);
    let angle = lab[2].atan2(lab[1]);
    let mut total = 0.;
    let mut selected_weight = 0.;
    for (i, r) in ranges.iter().enumerate() {
        // An untouched range must not dilute other ranges. For its own
        // inspection, show the influence it will have when adjusted.
        if i != selected && r.adjustment == [0.; 3] {
            continue;
        }
        let d = angle - r.center[0].to_radians();
        let w = (1. - smooth(d.sin().atan2(d.cos()).abs() / r.width[0].to_radians()))
            * (1. - smooth((chroma - r.center[1]).abs() / r.width[1]))
            * (1. - smooth((lab[0] - r.center[2]).abs() / r.width[2]));
        total += w;
        if i == selected {
            selected_weight = w;
        }
    }
    let protect = smooth((chroma / lab[0].abs().max(0.01) - 0.005) / 0.045);
    selected_weight / total.max(1.) * protect
}

pub fn inspect(
    context: &GpuContext,
    state: &AppState,
    path: &str,
    edits: &Value,
    index: usize,
    point: Option<[f32; 2]>,
) -> Result<Inspection> {
    ensure!(application::enabled(edits), "Range inspection requires v3");
    let original = application::controls(edits)?;
    ensure!(index < original.ranges.len(), "Select a color range first");
    if let Some(p) = point {
        ensure!(
            p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "Click inside the image"
        );
    }
    let mut primary = original.clone();
    primary.saturation = 0.;
    primary.vibrance = 0.;
    primary.hue = 0.;
    primary.bands = [[0.; 3]; 8];
    primary.grading = [[0.; 3]; 4];
    primary.ranges.clear();
    let mut sampling = edits.clone();
    sampling["v3"] = serde_json::to_value(primary)?;
    // Global color ranges precede every local adjustment.
    sampling["masks"] = serde_json::json!([]);
    let frame =
        application::render_file_with_capture(context, state, path, &sampling, Some(512), true)?;
    let graded = &frame
        .stages
        .as_ref()
        .context("Missing inspection stage")?
        .graded;
    let matrix = spaces::conversion(Primaries::DavinciWideGamut, Primaries::Srgb);
    let labs: Vec<_> = graded
        .pixels()
        .map(|p| lab(matrix * glam::DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)))
        .collect();
    let (width, height) = graded.dimensions();
    let center = if let Some(p) = point {
        let x = ((p[0] * width as f32) as u32).min(width - 1);
        let y = ((p[1] * height as f32) as u32).min(height - 1);
        ensure!(
            graded.get_pixel(x, y)[3] > 0.01,
            "Cannot sample a transparent pixel"
        );
        let v = labs[(y * width + x) as usize];
        let chroma = v[1].hypot(v[2]);
        ensure!(
            chroma / v[0].abs().max(0.01) > 0.005,
            "This pixel is neutral; choose a more colorful pixel"
        );
        ensure!(
            (0.0..=2.0).contains(&v[0]) && chroma <= 1.,
            "Sample exceeds the supported range; reduce primary exposure before sampling"
        );
        Some([v[2].atan2(v[1]).to_degrees().rem_euclid(360.), chroma, v[0]])
    } else {
        None
    };
    let mut ranges = original.ranges;
    if let Some(center) = center {
        ranges[index].center = center;
    }
    let matte = ImageBuffer::from_fn(width, height, |x, y| {
        Luma([(weight(labs[(y * width + x) as usize], &ranges, index)
            * graded.get_pixel(x, y)[3]
            * 255.)
            .round() as u8])
    });
    let mut image = Vec::new();
    frame.write_srgb_png(&mut image, false)?;
    let mut selection = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(matte).write_to(&mut selection, image::ImageFormat::Png)?;
    Ok(Inspection {
        image: format!("data:image/png;base64,{}", STANDARD.encode(image)),
        selection: format!(
            "data:image/png;base64,{}",
            STANDARD.encode(selection.into_inner())
        ),
        center,
    })
}

#[tauri::command]
pub async fn inspect_color_v3(
    path: String,
    edits: Value,
    index: usize,
    point: Option<[f32; 2]>,
    app_handle: tauri::AppHandle,
) -> Result<Inspection, String> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let context = state
            .gpu_context
            .lock()
            .map_err(|_| "GPU unavailable")?
            .clone()
            .ok_or("GPU unavailable")?;
        inspect(&context, &state, &path, &edits, index, point).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weights_wrap_protect_neutrals_and_account_for_active_overlap() {
        let range = ColorRange {
            center: [0., 0.1, 0.6],
            width: [30., 0.2, 0.5],
            adjustment: [10., 0., 0.],
        };
        assert_eq!(weight([0.6, 0., 0.], &[range.clone()], 0), 0.);
        assert!((weight([0.6, 0.1, 0.], &[range.clone()], 0) - 1.).abs() < 1e-6);
        assert!(
            (weight([0.6, 0.1, 0.00001], &[range.clone()], 0)
                - weight([0.6, 0.1, -0.00001], &[range.clone()], 0))
            .abs()
                < 1e-6
        );
        assert!((weight([0.6, 0.1, 0.], &[range.clone(), range.clone()], 0) - 0.5).abs() < 1e-6);
        let untouched = ColorRange {
            adjustment: [0.; 3],
            ..range.clone()
        };
        assert!((weight([0.6, 0.1, 0.], &[range.clone(), untouched.clone()], 0) - 1.).abs() < 1e-6);
        assert!((weight([0.6, 0.1, 0.], &[range, untouched], 1) - 0.5).abs() < 1e-6);
    }
}
