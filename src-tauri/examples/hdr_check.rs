//! What an HDR merge holds against its frames: clipped and noisy light.
//!
//!   hdr_check MERGE.dng FRAME FRAME [FRAME...]
//!
//! Each frame and the merge are decoded as the colour engine decodes them
//! (linear, scene-referred), the frames scaled to the middle frame's
//! exposure. Reports per image the share of pixels clipped, the brightest
//! light, and where the merge sits against the middle frame.
use darkroom_index::{color_engine::raw::decode_raw, hdr_output};

fn exposure(bytes: &[u8]) -> f32 {
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(bytes))
        .unwrap();
    let rational = |tag| match exif.get_field(tag, exif::In::PRIMARY).map(|f| &f.value) {
        Some(exif::Value::Rational(v)) => v[0].to_f64() as f32,
        Some(exif::Value::Short(v)) => v[0] as f32,
        _ => f32::NAN,
    };
    let iso = match exif
        .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
        .map(|f| &f.value)
    {
        Some(exif::Value::Short(v)) => v[0] as f32,
        Some(exif::Value::Long(v)) => v[0] as f32,
        _ => 100.0,
    };
    rational(exif::Tag::ExposureTime) * iso
}

fn luma(p: &[f32]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

fn stats(name: &str, px: &[f32], scale: f32) {
    let mut l: Vec<f32> = px.chunks(4).map(|p| luma(p) * scale).collect();
    let n = l.len();
    let clipped = px
        .chunks(4)
        .filter(|p| p[0].max(p[1]).max(p[2]) >= 0.98)
        .count() as f32
        / n as f32;
    l.sort_by(f32::total_cmp);
    let q = |f: f64| l[((n as f64 * f) as usize).min(n - 1)];
    println!(
        "{name:28} clipped {:5.2}%  p1 {:.5}  p50 {:.4}  p99 {:.3}  p99.9 {:.3}  max {:.2}",
        clipped * 100.0,
        q(0.01),
        q(0.5),
        q(0.99),
        q(0.999),
        l[n - 1]
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let merge = std::fs::read(&args[0]).unwrap();
    let frames: Vec<(String, Vec<u8>)> = args[1..]
        .iter()
        .map(|p| (p.clone(), std::fs::read(p).unwrap()))
        .collect();
    let exposures: Vec<f32> = frames.iter().map(|(_, b)| exposure(b)).collect();
    let reference = hdr_output::reference_exposure(&exposures);
    let mut decoded = Vec::new();
    for ((path, bytes), e) in frames.iter().zip(&exposures) {
        let f = decode_raw(bytes, false, || Ok(())).unwrap();
        let name = std::path::Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        println!(
            "{name}: exposure x ISO {e:.4}, x{:.2} of the middle",
            e / reference
        );
        // In the middle frame's units: what this frame says the light is.
        stats(
            &format!("  {name} (as middle)"),
            f.pixels.as_raw(),
            reference / e,
        );
        decoded.push(f);
    }
    let m = decode_raw(&merge, false, || Ok(())).unwrap();
    stats("MERGE", m.pixels.as_raw(), 1.0);
    let mid = decoded
        .iter()
        .zip(&exposures)
        .find(|(_, e)| **e == reference)
        .map(|(f, _)| f)
        .unwrap();
    if mid.pixels.dimensions() == m.pixels.dimensions() {
        // Where the middle frame clipped, what does the merge hold?
        let (mut n, mut above, mut sum) = (0usize, 0usize, 0f64);
        // In the darkest tenth of the middle frame, the merge's ratio.
        let mut dark: Vec<(f32, f32)> = Vec::new();
        for (a, b) in mid
            .pixels
            .as_raw()
            .chunks(4)
            .zip(m.pixels.as_raw().chunks(4))
        {
            if a[0].max(a[1]).max(a[2]) >= 0.98 {
                n += 1;
                let mb = b[0].max(b[1]).max(b[2]);
                if mb > 1.05 {
                    above += 1;
                }
                sum += mb as f64;
            }
            dark.push((luma(a), luma(b)));
        }
        println!(
            "where the middle frame clips ({n} px): merge above 1.05 in {:.1}%, mean brightest channel {:.2}",
            100.0 * above as f32 / n.max(1) as f32,
            sum / n.max(1) as f64
        );
        dark.sort_by(|x, y| x.0.total_cmp(&y.0));
        let tenth = &dark[..dark.len() / 10];
        let (sa, sb): (f64, f64) = tenth
            .iter()
            .fold((0.0, 0.0), |(x, y), (a, b)| (x + *a as f64, y + *b as f64));
        println!(
            "darkest tenth: middle mean {:.5}, merge mean {:.5} (ratio {:.3})",
            sa / tenth.len() as f64,
            sb / tenth.len() as f64,
            sb / sa
        );
    } else {
        println!(
            "merge is {:?}, middle {:?}",
            m.pixels.dimensions(),
            mid.pixels.dimensions()
        );
    }
}
