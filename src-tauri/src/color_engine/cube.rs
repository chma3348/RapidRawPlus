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
        let at = |r: u32, g: u32, b: u32| {
            self.entries[(r + (g + b * self.size) * self.size) as usize]
        };
        let scaled = rgb.map(|v| v.clamp(0.0, 1.0) * last);
        let base = scaled.map(|v| v.floor().min(last - 1.0));
        let frac: [f32; 3] = std::array::from_fn(|i| scaled[i] - base[i]);
        let index = base.map(|v| v as u32);
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let corner = |r: u32, g: u32, b: u32| at(index[0] + r, index[1] + g, index[2] + b)[c];
            // Trilinear, which is what the shader's tetrahedral lookup must
            // agree with to within the difference between the two schemes.
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
            let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), frac[0]);
            let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), frac[0]);
            let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), frac[0]);
            let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), frac[0]);
            out[c] = lerp(
                lerp(x00, x10, frac[1]),
                lerp(x01, x11, frac[1]),
                frac[2],
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2x2 cube that doubles red, halves green and leaves blue alone.
    fn tiny() -> String {
        let mut text = String::from("TITLE \"tiny\"\nLUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1\n");
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
        assert!(CubeLut::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err(), "short cube");
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
