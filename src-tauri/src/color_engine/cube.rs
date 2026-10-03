//! A captured 3D LUT used as the output transform.
//!
//! The provisional shoulder in `output.wgsl` is a defensible SDR rendering,
//! but it is ours, and nobody is trying to match it. When the goal is for a
//! render to look like Resolve's, the rendering transform is the largest term
//! by far — and it does not have to be reimplemented from guesses about its
//! shape. `tools/resolve_drt.py` samples it on a lattice and writes the result
//! here, so what runs is that transform at lattice resolution rather than an
//! approximation of it.
//!
//! The cube's domain is DaVinci Intermediate, a log encoding: an evenly spaced
//! lattice in it is evenly spaced perceptually, and covers roughly -0.01 to
//! 100 in linear light. Its output is already display-encoded, so the ordinary
//! sRGB encode must not run after it.

use anyhow::{Context, Result, bail, ensure};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct CubeLut {
    pub size: u32,
    /// Lattice entries, red fastest, as the `.cube` format orders them.
    /// Padded to four floats so the GPU can read them as `vec4`.
    pub entries: Vec<[f32; 4]>,
    /// Content hash, so a render cached against one cube is not reused for
    /// another that happens to sit at the same path.
    pub digest: String,
}

impl CubeLut {
    /// Parse the Adobe `.cube` format: a size, an optional domain, and
    /// `size^3` triples. A domain other than 0..1 is refused rather than
    /// silently ignored, because ignoring it would shift every colour.
    pub fn parse(text: &str) -> Result<Self> {
        let mut size = None;
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(head) = parts.next() else { continue };
            match head {
                "LUT_3D_SIZE" => {
                    ensure!(
                        size.is_none() && entries.is_empty(),
                        "Duplicate or misplaced LUT_3D_SIZE"
                    );
                    let value: u32 = parts.next().unwrap_or_default().parse()?;
                    ensure!(parts.next().is_none(), "LUT_3D_SIZE needs exactly one size");
                    ensure!(
                        (2..=128).contains(&value),
                        "A 3D LUT of size {value} is outside the supported 2..=128"
                    );
                    size = Some(value);
                }
                "LUT_1D_SIZE" => bail!("This is a 1D LUT; the output transform needs a 3D cube"),
                "TITLE" => {}
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    let expected = if head == "DOMAIN_MIN" { 0.0 } else { 1.0 };
                    let axes: Vec<_> = parts.collect();
                    ensure!(axes.len() == 3, "{head} needs exactly three values");
                    for axis in axes {
                        let value: f32 = axis.parse()?;
                        ensure!(
                            (value - expected).abs() < 1e-6,
                            "Only a 0..1 cube domain is supported; this one declares {value}"
                        );
                    }
                }
                _ => {
                    let declared = size.context("Cube data precedes LUT_3D_SIZE")?;
                    ensure!(
                        entries.len() < (declared as usize).pow(3),
                        "Too many cube entries"
                    );
                    let channels: Vec<_> = std::iter::once(head).chain(parts).collect();
                    ensure!(
                        channels.len() == 3,
                        "A cube entry needs exactly three values"
                    );
                    let mut channel = [0.0f32; 4];
                    for (slot, text) in channel[..3].iter_mut().zip(channels) {
                        *slot = text.parse()?;
                    }
                    ensure!(
                        channel.iter().all(|v| v.is_finite()),
                        "A cube entry is not a finite number"
                    );
                    entries.push(channel);
                }
            }
        }
        let size = size.ok_or_else(|| anyhow::anyhow!("The cube declares no LUT_3D_SIZE"))?;
        let expected = (size * size * size) as usize;
        ensure!(
            entries.len() == expected,
            "A size {size} cube needs {expected} entries; this one has {}",
            entries.len()
        );
        let mut hash = blake3::Hasher::new();
        for entry in &entries {
            for channel in &entry[..3] {
                hash.update(&channel.to_le_bytes());
            }
        }
        Ok(Self {
            size,
            entries,
            digest: hash.finalize().to_hex().to_string(),
        })
    }

    /// Read and parse a cube, once per file version: a 64-point lattice is a
    /// quarter of a million lines, and renders ask for the same one each time.
    pub fn load(path: &std::path::Path) -> Result<Arc<Self>> {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        type Key = (std::path::PathBuf, super::file_version::Version);
        static CACHE: OnceLock<Mutex<HashMap<Key, Arc<CubeLut>>>> = OnceLock::new();
        let key = (path.to_path_buf(), super::file_version::version(path)?);
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if cfg!(unix)
            && let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned())
        {
            return Ok(hit);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        let cube = Arc::new(Self::parse(&text)?);
        ensure!(
            key.1 == super::file_version::version(path)?,
            "Cube changed while reading; retry"
        );
        if let Ok(mut c) = cache.lock() {
            // Replacing files must not retain every historical lattice forever.
            c.retain(|(p, _), _| p != path);
            if c.len() >= 8 {
                c.clear();
            }
            c.insert(key, cube.clone());
        }
        Ok(cube)
    }

    /// The same tetrahedral lookup as output.wgsl. Also used by input
    /// conversion and the shared tone controls' neighbourhood on the CPU.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let last = (self.size - 1) as f32;
        let at =
            |r: u32, g: u32, b: u32| self.entries[(r + (g + b * self.size) * self.size) as usize];
        let scaled = rgb.map(|v| v.clamp(0.0, 1.0) * last);
        let base = scaled.map(|v| v.floor().min(last - 1.0));
        let frac: [f32; 3] = std::array::from_fn(|i| scaled[i] - base[i]);
        let index = base.map(|v| v as u32);
        let mut axes = [0usize, 1, 2];
        axes.sort_by(|&a, &b| frac[b].total_cmp(&frac[a]));
        let mut first = index;
        first[axes[0]] += 1;
        let mut second = first;
        second[axes[1]] += 1;
        let corners = [
            at(index[0], index[1], index[2]),
            at(first[0], first[1], first[2]),
            at(second[0], second[1], second[2]),
            at(index[0] + 1, index[1] + 1, index[2] + 1),
        ];
        let [a, b, c] = axes.map(|i| frac[i]);
        let weights = [1. - a, a - b, b - c, c];
        std::array::from_fn(|c| (0..4).map(|i| corners[i][c] * weights[i]).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2x2 cube that doubles red, halves green and leaves blue alone.
    fn tiny() -> String {
        let mut text =
            String::from("TITLE \"tiny\"\nLUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!("{} {} {}\n", r as f32 * 0.5, g as f32 * 0.25, b));
                }
            }
        }
        text
    }

    #[test]
    fn parses_and_interpolates() {
        let cube = CubeLut::parse(&tiny()).unwrap();
        assert_eq!(cube.size, 2);
        assert_eq!(cube.entries.len(), 8);
        assert_eq!(cube.sample([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
        assert_eq!(cube.sample([1.0, 1.0, 1.0]), [0.5, 0.25, 1.0]);
        let mid = cube.sample([0.5, 0.5, 0.5]);
        for (got, want) in mid.iter().zip([0.25, 0.125, 0.5]) {
            assert!((got - want).abs() < 1e-6, "{mid:?}");
        }
        // Out of domain is clamped, not wrapped or extrapolated.
        assert_eq!(cube.sample([2.0, -1.0, 0.0]), [0.5, 0.0, 0.0]);
    }

    #[test]
    fn refuses_what_it_cannot_honour() {
        assert!(CubeLut::parse("LUT_1D_SIZE 32\n0 0 0\n").is_err());
        assert!(
            CubeLut::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err(),
            "short cube"
        );
        assert!(CubeLut::parse("0 0 0\n").is_err(), "no declared size");
        let shifted = tiny().replace("DOMAIN_MAX 1 1 1", "DOMAIN_MAX 2 2 2");
        assert!(CubeLut::parse(&shifted).is_err(), "domain must be honoured");
        for bad in [
            tiny().replace("DOMAIN_MIN 0 0 0", "DOMAIN_MIN 0 0"),
            tiny().replace("0.5 0 0", "0.5 0"),
            tiny().replace("0.5 0 0", "0.5 0 0 7"),
            tiny().replace("LUT_3D_SIZE 2", "LUT_3D_SIZE 2\nLUT_3D_SIZE 2"),
        ] {
            assert!(CubeLut::parse(&bad).is_err(), "malformed cube was accepted");
        }
    }

    #[test]
    fn cpu_lookup_uses_the_gpu_tetrahedra() {
        // Only the white corner is lit: tetrahedral interpolation returns
        // min(r,g,b); trilinear returns r*g*b and fails this decisively.
        let cube = CubeLut::parse(
            "LUT_3D_SIZE 2\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n1 1 1\n",
        )
        .unwrap();
        for rgb in [
            [0.8, 0.5, 0.2],
            [0.8, 0.2, 0.5],
            [0.5, 0.2, 0.8],
            [0.2, 0.5, 0.8],
            [0.2, 0.8, 0.5],
            [0.5, 0.8, 0.2],
            [0.5; 3],
        ] {
            let expected = rgb.into_iter().fold(f32::INFINITY, f32::min);
            for got in cube.sample(rgb) {
                assert!(
                    (got - expected).abs() < 1e-6,
                    "{rgb:?}: {got} != {expected}"
                );
            }
        }
    }

    #[test]
    fn the_digest_follows_the_contents() {
        let a = CubeLut::parse(&tiny()).unwrap();
        let b = CubeLut::parse(&tiny().replace("TITLE \"tiny\"", "TITLE \"other\"")).unwrap();
        assert_eq!(a.digest, b.digest, "a title is not part of the transform");
        let c = CubeLut::parse(&tiny().replace("0.5 0 0", "0.6 0 0")).unwrap();
        assert_ne!(a.digest, c.digest, "an entry is");
    }
}

/// Bring a rendered photograph into the scene-referred working space through
/// a captured input transform.
///
/// Resolve's rendering transform expects scene values. A photograph has
/// already been through someone's rendering, so applying it directly
/// tone-maps the picture twice and darkens everything. Resolve pairs its
/// output transform with an input one that undoes the first rendering, and
/// this is that step: sRGB display values go in, linear DaVinci Wide Gamut
/// scene values come out, and from there the pipeline is the same as a RAW's.
///
/// The cube's own domain is sRGB-encoded, so the linear pixels the input
/// adapter produced are re-encoded on the way in — an exact inverse — and the
/// Intermediate values it returns are decoded on the way out.
/// Which captured input transform a photograph goes through, and what had to
/// happen to its colours first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputDomain {
    /// sRGB code values: the photo's colours all fit.
    Srgb,
    /// A capture made with the lattice tagged Display P3, for photos whose
    /// colours exceed sRGB.
    DisplayP3,
    /// Only an sRGB capture is installed: colours beyond sRGB were compressed
    /// into it first, the way the output stage compresses toward the display.
    SrgbCompressed,
}

/// Linear sRGB primaries to linear Display P3 primaries (both D65).
const SRGB_TO_P3: [[f32; 3]; 3] = [
    [0.822_462_1, 0.177_538, 0.0],
    [0.033_194_1, 0.966_805_9, 0.0],
    [0.017_082_7, 0.072_397_4, 0.910_519_9],
];

/// Does any pixel lie outside sRGB's 0..1? A hair of tolerance for the
/// rounding of the profile conversion.
pub fn exceeds_srgb(pixels: &image::Rgba32FImage) -> bool {
    pixels
        .pixels()
        .any(|p| p.0[..3].iter().any(|v| !(-2e-6..=1.000_002).contains(v)))
}

/// Pull colours that lie outside sRGB back inside it, hue-preservingly, in
/// linear light: each pixel's chroma is scaled toward its luminance. Like the
/// output stage's `gamut_compress`, chroma up to 85% of the way to the
/// boundary is untouched and everything beyond is squeezed into the last 15%
/// with u/(1+u), so saturated colours keep their order and separation
/// instead of piling onto the boundary. Luminance above white is clipped.
pub fn compress_into_srgb(pixels: &mut image::Rgba32FImage) {
    use rayon::prelude::*;
    const THRESHOLD: f32 = 0.85;
    pixels.as_mut().par_chunks_mut(4).for_each(|p| {
        let y = 0.212_639 * p[0] + 0.715_169 * p[1] + 0.072_192 * p[2];
        if y <= 0. {
            p[..3].copy_from_slice(&[0.; 3]);
            return;
        }
        if y >= 1. {
            p[..3].copy_from_slice(&[1.; 3]);
            return;
        }
        // How far along the ray from luminance to this colour the gamut
        // boundary sits, in units of the colour's own chroma: reach > 1 is
        // inside, < 1 outside.
        let mut reach = f32::INFINITY;
        for v in &p[..3] {
            let d = v - y;
            if d > 0. {
                reach = reach.min((1. - y) / d);
            } else if d < 0. {
                reach = reach.min(y / -d);
            }
        }
        if !reach.is_finite() {
            return;
        }
        let over = 1. / reach;
        if over <= THRESHOLD {
            return;
        }
        let u = (over - THRESHOLD) / (1. - THRESHOLD);
        let mapped = THRESHOLD + (1. - THRESHOLD) * u / (1. + u);
        let scale = mapped / over;
        for v in &mut p[..3] {
            *v = (y + (*v - y) * scale).clamp(0., 1.);
        }
    });
}

/// A Display P3 capture must agree with the sRGB capture on greys, where the
/// two spaces coincide; if it does not, the lattice was tagged with a
/// different transfer function than Display P3's and would shift every tone.
pub fn p3_capture_matches(srgb: &CubeLut, p3: &CubeLut) -> Result<()> {
    let mut worst = 0f32;
    for i in 1..16 {
        let g = i as f32 / 16.;
        let a = srgb.sample([g; 3]);
        let b = p3.sample([g; 3]);
        for c in 0..3 {
            worst = worst.max((a[c] - b[c]).abs());
        }
    }
    ensure!(
        worst < 0.004,
        "The Display P3 input transform disagrees with the sRGB one on greys by {worst:.4} (Intermediate); it was probably captured with the lattice tagged as a different colour space (gamma 2.6 P3 rather than Display P3)"
    );
    Ok(())
}

/// A Display P3 output capture must render greys exactly as the sRGB one
/// does: same white, same sRGB transfer curve. A different result means it
/// was captured with another output gamma (P3-D65's 2.6, say) or white.
pub fn p3_output_matches(srgb: &CubeLut, p3: &CubeLut) -> Result<()> {
    let mut worst = 0f32;
    for i in 1..16 {
        let g = i as f32 / 16.;
        let a = srgb.sample([g; 3]);
        let b = p3.sample([g; 3]);
        for c in 0..3 {
            worst = worst.max((a[c] - b[c]).abs());
        }
    }
    ensure!(
        worst < 0.004,
        "The Display P3 output transform disagrees with the sRGB one on greys by {worst:.4}; it was probably captured with a different output gamma or white point (P3-D65 at gamma 2.6 rather than Display P3)"
    );
    Ok(())
}

/// Encoded sRGB to encoded Display P3, in place: the same colours, stored
/// in the wider space. Both use the sRGB transfer curve and D65.
pub fn srgb_encoded_to_p3(pixels: &mut image::Rgba32FImage) {
    use rayon::prelude::*;
    let decode = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |v: f32| {
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    pixels.as_mut().par_chunks_mut(4).for_each(|p| {
        let linear = [decode(p[0]), decode(p[1]), decode(p[2])];
        for (c, row) in SRGB_TO_P3.iter().enumerate() {
            let v = row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2];
            p[c] = encode(v.clamp(0.0, 1.0));
        }
    });
}

/// The installed input captures, and the rule for choosing between them.
pub struct CapturedInput {
    pub srgb: Arc<CubeLut>,
    pub p3: Option<Arc<CubeLut>>,
}

impl CapturedInput {
    /// Which capture these pixels take. Colours within sRGB take the sRGB
    /// capture; beyond it, the Display P3 capture when installed, otherwise
    /// they are compressed into sRGB first. A photo never leaves Resolve's
    /// path because of a few saturated pixels.
    pub fn domain_for(&self, pixels: &image::Rgba32FImage) -> InputDomain {
        if !exceeds_srgb(pixels) {
            InputDomain::Srgb
        } else if self.p3.is_some() {
            InputDomain::DisplayP3
        } else {
            InputDomain::SrgbCompressed
        }
    }

    /// Apply the capture `domain` names. Patches use this with the domain the
    /// photograph itself took, so a heal lands in the same place.
    pub fn apply_as(&self, domain: InputDomain, pixels: &mut image::Rgba32FImage) {
        match domain {
            InputDomain::Srgb => apply_input_transform(&self.srgb, pixels),
            InputDomain::DisplayP3 => {
                let p3 = self.p3.as_ref().expect("P3 domain without a P3 capture");
                // Colours beyond even P3 (rare: profile rounding, Adobe RGB
                // greens) are compressed into it the same way.
                to_p3(pixels);
                if exceeds_srgb(pixels) {
                    compress_into_srgb(pixels);
                }
                apply_input_transform(p3, pixels);
            }
            InputDomain::SrgbCompressed => {
                compress_into_srgb(pixels);
                apply_input_transform(&self.srgb, pixels);
            }
        }
    }

    pub fn digest(&self) -> String {
        match &self.p3 {
            Some(p3) => format!("{}+{}", self.srgb.digest, p3.digest),
            None => self.srgb.digest.clone(),
        }
    }
}

fn to_p3(pixels: &mut image::Rgba32FImage) {
    use rayon::prelude::*;
    pixels.as_mut().par_chunks_mut(4).for_each(|p| {
        let rgb = [p[0], p[1], p[2]];
        for (c, row) in SRGB_TO_P3.iter().enumerate() {
            p[c] = row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2];
        }
    });
}

pub fn apply_input_transform(cube: &CubeLut, pixels: &mut image::Rgba32FImage) {
    let encode = |v: f32| {
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    let decode_intermediate = |v: f32| {
        if v <= 0.02740668 {
            v / 10.444_268
        } else {
            (v / 0.07329248 - 7.0).exp2() - 0.0075
        }
    };
    // Once per photograph, but on every photograph opened in v3 — in
    // parallel, it costs a fraction of the decode it follows.
    use rayon::prelude::*;
    pixels.as_mut().par_chunks_mut(4).for_each(|pixel| {
        let encoded: [f32; 3] = std::array::from_fn(|c| encode(pixel[c].clamp(0.0, 1.0)));
        let logged = cube.sample(encoded);
        for c in 0..3 {
            pixel[c] = decode_intermediate(logged[c]);
        }
    });
}

#[cfg(test)]
mod compression_tests {
    use super::*;

    #[test]
    fn in_place_p3_matches_the_previous_copying_path_exactly() {
        let cube = std::sync::Arc::new(
            CubeLut::parse(
                "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n",
            )
            .unwrap(),
        );
        let capture = CapturedInput {
            srgb: cube.clone(),
            p3: Some(cube.clone()),
        };
        for scale in [0.5, 1.0, 3.0] {
            let mut actual = image::Rgba32FImage::from_fn(128, 3, |x, y| {
                image::Rgba([(x as f32 / 127.0 - 0.1) * scale, y as f32 * 0.3, 0.4, 0.7])
            });
            let mut expected = actual.clone();
            to_p3(&mut expected);
            if exceeds_srgb(&expected) {
                compress_into_srgb(&mut expected);
            }
            apply_input_transform(&cube, &mut expected);
            capture.apply_as(InputDomain::DisplayP3, &mut actual);
            assert_eq!(actual.as_raw(), expected.as_raw());
        }
    }

    #[test]
    fn compression_leaves_ordinary_colour_alone_and_keeps_saturated_colour_ordered() {
        let mut image = image::Rgba32FImage::from_fn(4, 1, |x, _| match x {
            0 => image::Rgba([0.5, 0.3, 0.2, 1.]),
            1 => image::Rgba([1.0, -0.05, 0.1, 1.]),
            2 => image::Rgba([1.2, -0.1, 0.1, 1.]),
            _ => image::Rgba([0.9, 0.05, 0.05, 1.]),
        });
        let before = image.clone();
        compress_into_srgb(&mut image);
        assert_eq!(
            image.get_pixel(0, 0),
            before.get_pixel(0, 0),
            "an in-gamut colour moved"
        );
        for x in 1..4 {
            let p = image.get_pixel(x, 0);
            assert!(
                p.0[..3].iter().all(|v| (0.0..=1.0).contains(v)),
                "{p:?} not in gamut"
            );
        }
        // The two out-of-gamut reds stay distinct, the further one further.
        let sat = |p: &image::Rgba<f32>| p[0] - p[1];
        assert!(sat(image.get_pixel(2, 0)) > sat(image.get_pixel(1, 0)) + 1e-4);
        // Luminance is preserved by the chroma scaling.
        for x in 1..3 {
            let (a, b) = (before.get_pixel(x, 0), image.get_pixel(x, 0));
            let y = |p: &image::Rgba<f32>| 0.212_639 * p[0] + 0.715_169 * p[1] + 0.072_192 * p[2];
            assert!((y(a) - y(b)).abs() < 1e-5);
        }
    }

    #[test]
    fn p3_capture_check_accepts_matching_greys_and_rejects_a_gamma_mismatch() {
        let size = 9usize;
        let mut same = format!("LUT_3D_SIZE {size}\n");
        let mut gamma = same.clone();
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let c = [r, g, b].map(|i| i as f32 / (size - 1) as f32);
                    same.push_str(&format!("{} {} {}\n", c[0], c[1], c[2]));
                    let d = c.map(|v| v.powf(1.2));
                    gamma.push_str(&format!("{} {} {}\n", d[0], d[1], d[2]));
                }
            }
        }
        let a = CubeLut::parse(&same).unwrap();
        assert!(p3_capture_matches(&a, &a).is_ok());
        assert!(p3_capture_matches(&a, &CubeLut::parse(&gamma).unwrap()).is_err());
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    /// An identity cube must leave a photograph where it found it, once the
    /// sRGB and Intermediate encodings either side have cancelled.
    #[test]
    fn an_identity_cube_is_an_encoding_change_only() {
        let size = 32usize;
        let mut text = format!("LUT_3D_SIZE {size}\n");
        let axis = |i: usize| i as f64 / (size - 1) as f64;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    // sRGB in, the same colour expressed in Intermediate out.
                    let convert = |v: f64| {
                        let lin = if v <= 0.04045 {
                            v / 12.92
                        } else {
                            ((v + 0.055) / 1.055).powf(2.4)
                        };
                        if lin <= 0.00262409 {
                            lin * 10.44426855
                        } else {
                            ((lin + 0.0075).log2() + 7.0) * 0.07329248
                        }
                    };
                    text.push_str(&format!(
                        "{} {} {}\n",
                        convert(axis(r)),
                        convert(axis(g)),
                        convert(axis(b))
                    ));
                }
            }
        }
        let cube = CubeLut::parse(&text).unwrap();
        let mut image = image::ImageBuffer::from_fn(8, 1, |x, _| {
            let v = x as f32 / 7.0;
            image::Rgba([v * 0.9 + 0.02, v * 0.5 + 0.1, 0.4, 1.0])
        });
        let before = image.clone();
        apply_input_transform(&cube, &mut image);
        for (a, b) in before.pixels().zip(image.pixels()) {
            for c in 0..3 {
                assert!(
                    (a[c] - b[c]).abs() < 6e-3,
                    "identity input transform moved a pixel: {a:?} -> {b:?}"
                );
            }
            assert_eq!(a[3], b[3], "alpha is not colour");
        }
    }

    /// What the input transform costs on a 33-megapixel photograph.
    /// `cargo test --release --lib input_transform_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn input_transform_cost() {
        let path = std::path::Path::new(&std::env::var("HOME").unwrap()).join(
            "Library/Application Support/io.github.chma3348.DarkroomIndex/input-transform.cube",
        );
        let Ok(cube) = CubeLut::load(&path) else {
            return;
        };
        let mut image = image::ImageBuffer::from_fn(7008, 4672, |x, y| {
            image::Rgba([(x % 256) as f32 / 300.0, (y % 256) as f32 / 300.0, 0.2, 1.0])
        });
        let start = std::time::Instant::now();
        apply_input_transform(&cube, &mut image);
        println!(
            "input transform, 33 MP: {:.0} ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}
