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

use anyhow::{Result, bail, ensure};

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
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let Some(head) = parts.next() else { continue };
            match head {
                "LUT_3D_SIZE" => {
                    let value: u32 = parts.next().unwrap_or_default().parse()?;
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
                    for axis in parts.take(3) {
                        let value: f32 = axis.parse()?;
                        ensure!(
                            (value - expected).abs() < 1e-6,
                            "Only a 0..1 cube domain is supported; this one declares {value}"
                        );
                    }
                }
                _ => {
                    let mut channel = [0.0f32; 4];
                    channel[0] = head.parse()?;
                    for (slot, text) in channel[1..3].iter_mut().zip(parts) {
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

    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    /// Reference lookup for tests. The shader does this on the GPU, and
    /// `cube_matches_the_reference_lookup` keeps the two honest.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let last = (self.size - 1) as f32;
        let at =
            |r: u32, g: u32, b: u32| self.entries[(r + (g + b * self.size) * self.size) as usize];
        let scaled = rgb.map(|v| v.clamp(0.0, 1.0) * last);
        let base = scaled.map(|v| v.floor().min(last - 1.0));
        let frac: [f32; 3] = std::array::from_fn(|i| scaled[i] - base[i]);
        let index = base.map(|v| v as u32);
        std::array::from_fn(|c| {
            let corner = |r: u32, g: u32, b: u32| at(index[0] + r, index[1] + g, index[2] + b)[c];
            // Trilinear, which is what the shader's tetrahedral lookup must
            // agree with to within the difference between the two schemes.
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
            let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), frac[0]);
            let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), frac[0]);
            let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), frac[0]);
            let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), frac[0]);
            lerp(lerp(x00, x10, frac[1]), lerp(x01, x11, frac[1]), frac[2])
        })
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
        let path = std::path::Path::new(&std::env::var("HOME").unwrap())
            .join("Library/Application Support/io.github.CyberTimon.RapidRAW/input-transform.cube");
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
