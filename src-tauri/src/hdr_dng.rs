//! Floating-point linear DNGs: the merged result of a RAW bracket, and the
//! ones other programs write (Lightroom's HDR and panorama merges).
//!
//! A merge holds more range than any one frame, so it is kept as linear
//! light in floating point, 16-bit half floats, deflate-compressed with
//! TIFF's floating-point predictor — the form Lightroom uses — rather than
//! integers, which would round away the shadows the bright frame recovered.
//! The pixels are already demosaiced and white balanced (`LinearRaw`), in
//! linear sRGB primaries (the colour matrix says so, so every DNG reader
//! interprets them alike), scaled so the brightest fit under 1.0, with
//! `BaselineExposure` restoring the reference frame's brightness.
//!
//! rawler, the RAW decoder, reads neither LinearRaw nor floating-point
//! DNGs, so these are read here and handed to the colour engine as a RAW
//! would be: scene-referred, opened with the RAW look.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail, ensure};
use glam::{DMat3, DVec3};
use image::Rgb32FImage;
use rayon::prelude::*;

use crate::color_engine::config::{Primaries, ReferenceDomain, SourceColor, Transfer};
use crate::color_engine::input::{DecodedFrame, InputProvenance};

const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;
const ILLUMINANT_D65: u32 = 21;
/// Rows per compressed strip: small enough to compress strips in parallel
/// and to keep each one's buffer modest.
const ROWS_PER_STRIP: u32 = 32;

/// XYZ (D65) to linear sRGB: the "camera" of a merged DNG is linear sRGB.
const XYZ_TO_SRGB: [f64; 9] = [
    3.2404542, -1.5371385, -0.4985314, //
    -0.9692660, 1.8760108, 0.0415560, //
    0.0556434, -0.2040259, 1.0572252,
];

/// Where a merge came from, for the file's camera fields.
pub struct DngCamera {
    pub make: String,
    pub model: String,
}

/// How a merge's pixels are stored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Compression {
    /// Half floats, deflate with the floating-point predictor: exact (to
    /// half-float precision), and large (about 145 MB for 33 MP).
    Lossless,
    /// JPEG XL in 512-pixel tiles (DNG 1.7, as Lightroom 13 writes its
    /// merges): a tenth of the size or less, at `distance` (JPEG XL's
    /// quality scale; 1.0 is "visually lossless" for viewing).
    Compact { distance: f32 },
}

/// The compact setting merges use: chosen on real photos, pushed and
/// zoomed in (docs/hdr-merge.md).
pub const COMPACT: Compression = Compression::Compact { distance: 0.3 };

const TILE: u32 = 512;
const COMPRESSION_DEFLATE: u16 = 8;
const COMPRESSION_JXL: u16 = 52546;

/// Write `linear` (linear sRGB, white balanced, in the reference frame's
/// units: 1.0 is where its sensor clipped) as a floating-point DNG.
pub fn write_float_dng(
    path: &std::path::Path,
    linear: &Rgb32FImage,
    camera: &DngCamera,
    compression: Compression,
    preview: Option<&image::RgbImage>,
) -> Result<()> {
    let (width, height) = linear.dimensions();
    ensure!(width > 0 && height > 0, "Empty merge");
    // Stored as it is, in the reference frame's units: its midtones near
    // 0.18, highlights past 1.0 kept (half floats reach 65504). Scaling the
    // brightest light under 1.0 instead made JPEG XL take a sunset for deep
    // shadow and spend almost nothing on it (28% of pixels off by more than
    // 4 levels with Exposure +3), and pushed half floats' shadows toward
    // their coarse subnormal range.
    let scale = 1.0f32;
    let baseline_exposure = 0.0f64;

    let mut tiff = TiffWriter::new();
    let ifd = match compression {
        Compression::Lossless => {
            let strips: Vec<Vec<u8>> = (0..height.div_ceil(ROWS_PER_STRIP))
                .into_par_iter()
                .map(|strip| -> Result<Vec<u8>> {
                    let first = strip * ROWS_PER_STRIP;
                    let rows = ROWS_PER_STRIP.min(height - first);
                    let row_len = width as usize * 3;
                    let mut raw = Vec::with_capacity(row_len * 2 * rows as usize);
                    for y in first..first + rows {
                        let start = y as usize * row_len;
                        raw.extend(encode_row_f16(
                            &linear.as_raw()[start..start + row_len],
                            3,
                            scale,
                        ));
                    }
                    let mut encoder =
                        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
                    encoder.write_all(&raw)?;
                    Ok(encoder.finish()?)
                })
                .collect::<Result<_>>()?;
            let offsets: Vec<u32> = strips.iter().map(|s| tiff.data(s)).collect();
            let counts: Vec<u32> = strips.iter().map(|s| s.len() as u32).collect();
            vec![
                Tag::short(259, &[COMPRESSION_DEFLATE]),
                Tag::long(273, &offsets),
                Tag::long(278, &[ROWS_PER_STRIP]),
                Tag::long(279, &counts),
                Tag::short(317, &[3]),
            ]
        }
        Compression::Compact { distance } => {
            let across = width.div_ceil(TILE);
            let tiles: Vec<Vec<u8>> = (0..across * height.div_ceil(TILE))
                .into_par_iter()
                .map(|i| -> Result<Vec<u8>> {
                    let (x0, y0) = ((i % across) * TILE, (i / across) * TILE);
                    // Every tile is a full tile, as DNG lays them out; past
                    // the picture's edge it repeats the last pixel.
                    let (tw, th) = (TILE, TILE);
                    let mut data = Vec::with_capacity((tw * th * 12) as usize);
                    for dy in 0..th {
                        let y = (y0 + dy).min(height - 1);
                        for dx in 0..tw {
                            let x = (x0 + dx).min(width - 1);
                            for v in linear.get_pixel(x, y).0 {
                                let v = v * scale;
                                let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
                                data.extend_from_slice(&v.to_ne_bytes());
                            }
                        }
                    }
                    // No HDR intensity target: decoders tone-map a picture
                    // that declares one, which pulled its brightest values
                    // down (measured: 0.85 read back as 0.78).
                    jxl_encoder::LossyConfig::new(distance)
                        .with_threads(1)
                        .encode(&data, tw, th, jxl_encoder::PixelLayout::RgbLinearF32)
                        .map_err(|e| anyhow::anyhow!("JPEG XL: {e}"))
                })
                .collect::<Result<_>>()?;
            let offsets: Vec<u32> = tiles.iter().map(|t| tiff.data(t)).collect();
            let counts: Vec<u32> = tiles.iter().map(|t| t.len() as u32).collect();
            vec![
                Tag::short(259, &[COMPRESSION_JXL]),
                Tag::long(322, &[TILE]),
                Tag::long(323, &[TILE]),
                Tag::long(324, &offsets),
                Tag::long(325, &counts),
                Tag::float(52553, &[distance]),
                Tag::long(52554, &[7]),
                Tag::long(52555, &[4]),
            ]
        }
    };

    let unique = format!("Darkroom Index HDR ({} {})", camera.make, camera.model);
    let srational = |v: f64| {
        let d = 1_000_000i32;
        ((v * d as f64).round() as i32, d)
    };
    // The linear pixels: their own directory (a SubIFD) when there is a
    // preview to head the file, as Lightroom lays a DNG out; otherwise the
    // file's only directory.
    let mut raw = ifd;
    raw.extend([
        Tag::long(254, &[0]),
        Tag::long(256, &[width]),
        Tag::long(257, &[height]),
        Tag::short(258, &[16, 16, 16]),
        Tag::short(262, &[PHOTOMETRIC_LINEAR_RAW as u16]),
        Tag::short(277, &[3]),
        Tag::short(284, &[1]),
        Tag::short(339, &[3, 3, 3]),
    ]);
    // What the whole file says: the camera, the DNG version, the colour.
    let mut root = vec![
        Tag::ascii(271, &camera.make),
        Tag::ascii(272, &camera.model),
        Tag::short(274, &[1]),
        Tag::ascii(305, "Darkroom Index"),
        Tag::byte(50706, &[1, 7, 0, 0]),
        Tag::byte(
            50707,
            &if matches!(compression, Compression::Compact { .. }) {
                [1, 7, 0, 0]
            } else {
                [1, 4, 0, 0]
            },
        ),
        Tag::ascii(50708, &unique),
        Tag::srational(50721, &XYZ_TO_SRGB.map(srational)),
        Tag::rational(50728, &[(1, 1), (1, 1), (1, 1)]),
        Tag::srational(50730, &[srational(baseline_exposure)]),
        Tag::short(50778, &[ILLUMINANT_D65 as u16]),
    ];
    if let Some(preview) = preview {
        // A rendered preview heads the file (what Finder, Quick Look and
        // Nextcloud's web view show): baseline JPEG in one strip.
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 88)
            .encode_image(preview)
            .context("Encoding the DNG's preview")?;
        let at = tiff.data(&jpeg);
        raw.sort_by_key(|t| t.code);
        let sub = tiff.directory(&raw);
        root.extend([
            Tag::long(254, &[1]),
            Tag::long(256, &[preview.width()]),
            Tag::long(257, &[preview.height()]),
            Tag::short(258, &[8, 8, 8]),
            Tag::short(259, &[7]),
            Tag::short(262, &[6]),
            Tag::long(273, &[at]),
            Tag::short(277, &[3]),
            Tag::long(278, &[preview.height()]),
            Tag::long(279, &[jpeg.len() as u32]),
            Tag::short(284, &[1]),
            Tag::short(530, &[2, 2]),
            Tag::long(330, &[sub]),
        ]);
    } else {
        root.extend(raw);
    }
    root.sort_by_key(|t| t.code);
    std::fs::write(path, tiff.finish(&root))?;
    Ok(())
}

/// One row of float samples, times `scale`, as TIFF's floating-point
/// predictor (3) stores them in half floats: each value's bytes split into
/// planes, most significant first, then each byte differenced from the one
/// a pixel before.
fn encode_row_f16(row: &[f32], samples: usize, scale: f32) -> Vec<u8> {
    let n = row.len();
    let mut out = vec![0u8; n * 2];
    for (i, v) in row.iter().enumerate() {
        let v = v * scale;
        let bits = half::f16::from_f32(if v.is_finite() { v.max(0.0) } else { 0.0 }).to_bits();
        out[i] = (bits >> 8) as u8;
        out[n + i] = bits as u8;
    }
    for i in (samples..out.len()).rev() {
        out[i] = out[i].wrapping_sub(out[i - samples]);
    }
    out
}

/// A tag for the writer: its code, TIFF type, count and value bytes
/// (little-endian).
struct Tag {
    code: u16,
    kind: u16,
    count: u32,
    bytes: Vec<u8>,
}

impl Tag {
    fn byte(code: u16, v: &[u8]) -> Self {
        Self {
            code,
            kind: 1,
            count: v.len() as u32,
            bytes: v.to_vec(),
        }
    }
    fn ascii(code: u16, v: &str) -> Self {
        let mut bytes = v.as_bytes().to_vec();
        bytes.push(0);
        Self {
            code,
            kind: 2,
            count: bytes.len() as u32,
            bytes,
        }
    }
    fn short(code: u16, v: &[u16]) -> Self {
        Self {
            code,
            kind: 3,
            count: v.len() as u32,
            bytes: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        }
    }
    fn long(code: u16, v: &[u32]) -> Self {
        Self {
            code,
            kind: 4,
            count: v.len() as u32,
            bytes: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        }
    }
    fn rational(code: u16, v: &[(u32, u32)]) -> Self {
        let bytes = v
            .iter()
            .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
            .collect();
        Self {
            code,
            kind: 5,
            count: v.len() as u32,
            bytes,
        }
    }
    fn float(code: u16, v: &[f32]) -> Self {
        Self {
            code,
            kind: 11,
            count: v.len() as u32,
            bytes: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        }
    }
    fn srational(code: u16, v: &[(i32, i32)]) -> Self {
        let bytes = v
            .iter()
            .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
            .collect();
        Self {
            code,
            kind: 10,
            count: v.len() as u32,
            bytes,
        }
    }
}

/// A classic little-endian TIFF built in memory: data blocks first, the
/// one IFD last.
struct TiffWriter {
    buf: Vec<u8>,
}

impl TiffWriter {
    fn new() -> Self {
        // Header; the IFD offset is filled in by `finish`.
        Self {
            buf: vec![b'I', b'I', 42, 0, 0, 0, 0, 0],
        }
    }
    /// Append a block (word-aligned) and return its offset.
    fn data(&mut self, bytes: &[u8]) -> u32 {
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        let offset = self.buf.len() as u32;
        self.buf.extend_from_slice(bytes);
        offset
    }
    /// The first directory (the header points to it), last.
    fn finish(mut self, tags: &[Tag]) -> Vec<u8> {
        let ifd = self.directory(tags);
        self.buf[4..8].copy_from_slice(&ifd.to_le_bytes());
        self.buf
    }

    /// Append a directory (tags sorted, next-directory offset 0) and return
    /// its offset.
    fn directory(&mut self, tags: &[Tag]) -> u32 {
        // Values too long to sit in an entry go before the IFD.
        let mut placed: Vec<Option<u32>> = Vec::with_capacity(tags.len());
        for tag in tags {
            placed.push((tag.bytes.len() > 4).then(|| self.data(&tag.bytes)));
        }
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
        let ifd = self.buf.len() as u32;
        self.buf
            .extend_from_slice(&(tags.len() as u16).to_le_bytes());
        for (tag, offset) in tags.iter().zip(placed) {
            self.buf.extend_from_slice(&tag.code.to_le_bytes());
            self.buf.extend_from_slice(&tag.kind.to_le_bytes());
            self.buf.extend_from_slice(&tag.count.to_le_bytes());
            match offset {
                Some(at) => self.buf.extend_from_slice(&at.to_le_bytes()),
                None => {
                    let mut inline = tag.bytes.clone();
                    inline.resize(4, 0);
                    self.buf.extend_from_slice(&inline);
                }
            }
        }
        self.buf.extend_from_slice(&0u32.to_le_bytes());
        ifd
    }
}

// ---------------------------------------------------------------- reading

/// A parsed TIFF directory entry's values, widened.
#[derive(Clone, Debug)]
enum Values {
    Ints(Vec<u64>),
    Ratios(Vec<f64>),
    Text(String),
}

impl Values {
    fn int(&self, i: usize) -> Option<u64> {
        match self {
            Values::Ints(v) => v.get(i).copied(),
            Values::Ratios(v) => v.get(i).map(|x| *x as u64),
            Values::Text(_) => None,
        }
    }
    fn ints(&self) -> Vec<u64> {
        match self {
            Values::Ints(v) => v.clone(),
            Values::Ratios(v) => v.iter().map(|x| *x as u64).collect(),
            Values::Text(_) => vec![],
        }
    }
    fn floats(&self) -> Vec<f64> {
        match self {
            Values::Ints(v) => v.iter().map(|x| *x as f64).collect(),
            Values::Ratios(v) => v.clone(),
            Values::Text(_) => vec![],
        }
    }
}

type Ifd = std::collections::BTreeMap<u16, Values>;

struct Reader<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl Reader<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b = self.bytes.get(at..at + 2)?;
        Some(if self.little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }
    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
    fn ifd(&self, at: usize) -> Option<(Ifd, u32)> {
        let count = self.u16(at)? as usize;
        let mut ifd = Ifd::new();
        for e in 0..count {
            let entry = at + 2 + e * 12;
            let code = self.u16(entry)?;
            let kind = self.u16(entry + 2)?;
            let n = self.u32(entry + 4)? as usize;
            let size = match kind {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => continue,
            };
            let total = size * n;
            let data = if total <= 4 {
                entry + 8
            } else {
                self.u32(entry + 8)? as usize
            };
            if self.bytes.len() < data + total {
                continue;
            }
            let values = match kind {
                2 => Values::Text(
                    String::from_utf8_lossy(&self.bytes[data..data + n])
                        .trim_end_matches('\0')
                        .to_string(),
                ),
                1 | 7 | 6 => Values::Ints(
                    self.bytes[data..data + n]
                        .iter()
                        .map(|b| *b as u64)
                        .collect(),
                ),
                3 | 8 => Values::Ints(
                    (0..n)
                        .filter_map(|i| self.u16(data + 2 * i).map(|v| v as u64))
                        .collect(),
                ),
                4 | 9 | 13 => Values::Ints(
                    (0..n)
                        .filter_map(|i| self.u32(data + 4 * i).map(|v| v as u64))
                        .collect(),
                ),
                5 => Values::Ratios(
                    (0..n)
                        .filter_map(|i| {
                            let (num, den) = (self.u32(data + 8 * i)?, self.u32(data + 8 * i + 4)?);
                            Some(if den == 0 {
                                0.0
                            } else {
                                num as f64 / den as f64
                            })
                        })
                        .collect(),
                ),
                10 => Values::Ratios(
                    (0..n)
                        .filter_map(|i| {
                            let (num, den) = (
                                self.u32(data + 8 * i)? as i32,
                                self.u32(data + 8 * i + 4)? as i32,
                            );
                            Some(if den == 0 {
                                0.0
                            } else {
                                num as f64 / den as f64
                            })
                        })
                        .collect(),
                ),
                11 => Values::Ratios(
                    (0..n)
                        .filter_map(|i| self.u32(data + 4 * i).map(|b| f32::from_bits(b) as f64))
                        .collect(),
                ),
                12 => Values::Ratios(
                    (0..n)
                        .filter_map(|i| {
                            let b: [u8; 8] = self
                                .bytes
                                .get(data + 8 * i..data + 8 * i + 8)?
                                .try_into()
                                .ok()?;
                            Some(if self.little {
                                f64::from_le_bytes(b)
                            } else {
                                f64::from_be_bytes(b)
                            })
                        })
                        .collect(),
                ),
                _ => continue,
            };
            ifd.insert(code, values);
        }
        let next = self.u32(at + 2 + count * 12).unwrap_or(0);
        Some((ifd, next))
    }
}

/// The main image's directory of a DNG holding floating-point LinearRaw
/// pixels, with the root directory (where the colour tags live), if this
/// is one. Anything else — a Bayer DNG, an ordinary TIFF — is `None`, for
/// the usual decoders.
fn float_linear_dng(bytes: &[u8]) -> Option<(Reader<'_>, Ifd, Ifd)> {
    let little = match bytes.get(0..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let reader = Reader { bytes, little };
    let (root, _) = reader.ifd(reader.u32(4)? as usize)?;
    root.get(&50706)?; // DNGVersion: a DNG at all
    // Floating-point samples, or JPEG XL tiles (which rawler cannot read
    // either, whatever their sample format).
    let is_main = |ifd: &Ifd| {
        ifd.get(&254).and_then(|v| v.int(0)).unwrap_or(0) == 0
            && ifd.get(&262).and_then(|v| v.int(0)) == Some(PHOTOMETRIC_LINEAR_RAW as u64)
            && (ifd.get(&339).and_then(|v| v.int(0)) == Some(3)
                || ifd.get(&259).and_then(|v| v.int(0)) == Some(COMPRESSION_JXL as u64))
    };
    if is_main(&root) {
        return Some((reader, root.clone(), root));
    }
    // Lightroom keeps the raw image in a SubIFD, a preview in the root.
    for offset in root.get(&330).map(|v| v.ints()).unwrap_or_default() {
        if let Some((sub, _)) = reader.ifd(offset as usize)
            && is_main(&sub)
        {
            return Some((reader, sub, root));
        }
    }
    None
}

/// Whether `bytes` is a floating-point LinearRaw DNG (read here, not by
/// rawler).
pub fn is_float_linear_dng(bytes: &[u8]) -> bool {
    float_linear_dng(bytes).is_some()
}

/// Read a floating-point LinearRaw DNG as the colour engine's decoded
/// frame: linear sRGB, white balanced, scene-referred, BaselineExposure
/// applied.
pub fn decode_float_dng(bytes: &[u8]) -> Result<DecodedFrame> {
    let (reader, ifd, root) = float_linear_dng(bytes).context("Not a floating-point linear DNG")?;
    let get = |k: u16| ifd.get(&k).or_else(|| root.get(&k));
    let width = get(256).and_then(|v| v.int(0)).context("No width")? as usize;
    let height = get(257).and_then(|v| v.int(0)).context("No height")? as usize;
    let spp = get(277).and_then(|v| v.int(0)).unwrap_or(1) as usize;
    ensure!(
        spp == 3,
        "Floating-point DNGs with {spp} samples per pixel are not supported"
    );
    let bits = get(258).and_then(|v| v.int(0)).unwrap_or(32) as usize;
    ensure!(
        matches!(bits, 8 | 10 | 12 | 14 | 16 | 24 | 32),
        "Unsupported sample depth {bits}"
    );
    let compression = get(259).and_then(|v| v.int(0)).unwrap_or(1);
    ensure!(
        matches!(compression, 1 | 8 | 32946 | 52546),
        "Unsupported DNG compression {compression} for floating-point data"
    );
    let jxl = compression == COMPRESSION_JXL as u64;
    let float = get(339).and_then(|v| v.int(0)) == Some(3);
    ensure!(
        float || jxl,
        "Integer samples are read here only from JPEG XL tiles"
    );
    // JPEG XL hands integer samples back as 0..1: back to the file's scale,
    // which its black and white levels are given in.
    let integer_scale = if jxl && !float {
        ((1u64 << bits.min(32)) - 1) as f32
    } else {
        1.0
    };
    let predictor = get(317).and_then(|v| v.int(0)).unwrap_or(1);
    let factor = match predictor {
        1 => 0,
        3 => 1,
        34894 => 2,
        34895 => 4,
        other => bail!("Unsupported DNG predictor {other}"),
    };
    ensure!(
        get(284).and_then(|v| v.int(0)).unwrap_or(1) == 1,
        "Planar DNGs are not supported"
    );
    let bytes_per = bits / 8;

    // Strips or tiles: each a block of `bw` x `bh` pixels at (bx, by).
    let blocks: Vec<(usize, usize, usize, usize, u64, u64)> = if let Some(offsets) = get(324) {
        let tw = get(322).and_then(|v| v.int(0)).context("No tile width")? as usize;
        let th = get(323).and_then(|v| v.int(0)).context("No tile length")? as usize;
        let counts = get(325).context("No tile byte counts")?.ints();
        let across = width.div_ceil(tw);
        offsets
            .ints()
            .into_iter()
            .zip(counts)
            .enumerate()
            .map(|(i, (o, c))| ((i % across) * tw, (i / across) * th, tw, th, o, c))
            .collect()
    } else {
        let offsets = get(273).context("No strip offsets")?.ints();
        let counts = get(279).context("No strip byte counts")?.ints();
        let rows = get(278)
            .and_then(|v| v.int(0))
            .map(|r| r as usize)
            .unwrap_or(height);
        offsets
            .into_iter()
            .zip(counts)
            .enumerate()
            .map(|(i, (o, c))| {
                (
                    0,
                    i * rows,
                    width,
                    rows.min(height.saturating_sub(i * rows)),
                    o,
                    c,
                )
            })
            .collect()
    };

    let little = reader.little;
    let decoded: Vec<(usize, usize, usize, usize, Vec<f32>)> = blocks
        .par_iter()
        .map(|&(bx, by, bw, bh, offset, count)| -> Result<_> {
            let raw = bytes
                .get(offset as usize..(offset + count) as usize)
                .context("DNG block outside the file")?;
            if jxl {
                // One JPEG XL codestream per tile, decoded as linear light.
                let mut image = jxl_oxide::JxlImage::read_with_defaults(raw)
                    .map_err(|e| anyhow::anyhow!("JPEG XL tile: {e}"))?;
                image.request_color_encoding(jxl_oxide::EnumColourEncoding::srgb_linear(
                    jxl_oxide::RenderingIntent::Relative,
                ));
                let render = image
                    .render_frame(0)
                    .map_err(|e| anyhow::anyhow!("JPEG XL tile: {e}"))?;
                let buffer = render.image_all_channels();
                let (jw, ch) = (buffer.width(), buffer.channels());
                ensure!(ch >= 3, "JPEG XL tile without three channels");
                let rows = bh.min(buffer.height());
                let cols = bw.min(jw);
                let mut values = vec![0.0f32; bw * bh * spp];
                for y in 0..rows {
                    for x in 0..cols {
                        let p = &buffer.buf()[(y * jw + x) * ch..];
                        for c in 0..3 {
                            let v = p[c] * integer_scale;
                            values[(y * bw + x) * 3 + c] = if v.is_finite() { v } else { 0.0 };
                        }
                    }
                }
                return Ok((bx, by, bw, bh, values));
            }
            let mut data = if compression == 1 {
                raw.to_vec()
            } else {
                let mut out = Vec::with_capacity(bw * bh * spp * bytes_per);
                flate2::read::ZlibDecoder::new(raw).read_to_end(&mut out)?;
                out
            };
            let row_bytes = bw * spp * bytes_per;
            ensure!(data.len() >= row_bytes * bh, "Short DNG block");
            let mut values = Vec::with_capacity(bw * bh * spp);
            for row in data.chunks_mut(row_bytes).take(bh) {
                if factor > 0 {
                    let stride = spp * factor;
                    for i in stride..row.len() {
                        row[i] = row[i].wrapping_add(row[i - stride]);
                    }
                }
                let n = bw * spp;
                for i in 0..n {
                    let byte = |b: usize| {
                        if factor > 0 {
                            // Byte planes, most significant first.
                            row[b * n + i]
                        } else if little {
                            row[i * bytes_per + (bytes_per - 1 - b)]
                        } else {
                            row[i * bytes_per + b]
                        }
                    };
                    let v = match bytes_per {
                        2 => {
                            half::f16::from_bits(((byte(0) as u16) << 8) | byte(1) as u16).to_f32()
                        }
                        3 => f24_to_f32(
                            ((byte(0) as u32) << 16) | ((byte(1) as u32) << 8) | byte(2) as u32,
                        ),
                        _ => f32::from_bits(
                            ((byte(0) as u32) << 24)
                                | ((byte(1) as u32) << 16)
                                | ((byte(2) as u32) << 8)
                                | byte(3) as u32,
                        ),
                    };
                    values.push(if v.is_finite() { v } else { 0.0 });
                }
            }
            Ok((bx, by, bw, bh, values))
        })
        .collect::<Result<_>>()?;

    // Camera to linear sRGB: the D65 colour matrix and the as-shot neutral.
    let illuminants = [get(50778), get(50779)].map(|v| v.and_then(|v| v.int(0)));
    let matrices = [get(50721), get(50722)].map(|v| v.map(|v| v.floats()));
    let slot = (0..2)
        .find(|&i| {
            illuminants[i] == Some(ILLUMINANT_D65 as u64)
                && matrices[i].as_ref().is_some_and(|m| m.len() == 9)
        })
        .or_else(|| (0..2).find(|&i| matrices[i].as_ref().is_some_and(|m| m.len() == 9)))
        .context("No colour matrix")?;
    let mut warnings = vec![];
    if illuminants[slot] != Some(ILLUMINANT_D65 as u64) {
        warnings.push("No D65 colour matrix; using the file's other calibration.".to_string());
    }
    let m = matrices[slot].clone().unwrap();
    let xyz_to_camera =
        DMat3::from_cols_array(&[m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8]]);
    let rgb_to_camera = xyz_to_camera * crate::color_engine::spaces::rgb_to_xyz(Primaries::Srgb);
    let sums = rgb_to_camera * DVec3::ONE;
    ensure!(
        sums.to_array().iter().all(|v| *v > 1e-10),
        "Invalid DNG colour matrix"
    );
    let normalized = DMat3::from_diagonal(sums.recip()) * rgb_to_camera;
    ensure!(
        normalized.determinant().abs() > 1e-10,
        "Singular DNG colour matrix"
    );
    let neutral = get(50728)
        .map(|v| v.floats())
        .filter(|v| v.len() >= 3)
        .unwrap_or(vec![1.0; 3]);
    let gains = DVec3::new(1.0 / neutral[0], 1.0 / neutral[1], 1.0 / neutral[2]) * neutral[1];
    let white = get(50717)
        .and_then(|v| v.floats().first().copied())
        .filter(|w| *w > 0.0)
        .unwrap_or(1.0);
    let black = get(50714)
        .and_then(|v| v.floats().first().copied())
        .unwrap_or(0.0);
    let exposure = 2f64.powf(
        get(50730)
            .and_then(|v| v.floats().first().copied())
            .unwrap_or(0.0),
    );
    let transform =
        normalized.inverse() * DMat3::from_diagonal(gains) * (exposure / (white - black));

    let mut pixels = image::Rgba32FImage::new(width as u32, height as u32);
    for (bx, by, bw, bh, values) in decoded {
        for y in 0..bh {
            if by + y >= height {
                break;
            }
            for x in 0..bw {
                if bx + x >= width {
                    break;
                }
                let i = (y * bw + x) * 3;
                let camera = DVec3::new(
                    values[i] as f64 - black,
                    values[i + 1] as f64 - black,
                    values[i + 2] as f64 - black,
                );
                let rgb = transform * camera;
                pixels.put_pixel(
                    (bx + x) as u32,
                    (by + y) as u32,
                    image::Rgba([rgb.x as f32, rgb.y as f32, rgb.z as f32, 1.0]),
                );
            }
        }
    }
    let orientation = get(274).and_then(|v| v.int(0)).unwrap_or(1) as u8;
    if orientation != 1
        && let Some(o) = image::metadata::Orientation::from_exif(orientation)
    {
        let mut image = image::DynamicImage::ImageRgba32F(pixels);
        image.apply_orientation(o);
        pixels = image.into_rgba32f();
    }
    let camera = get(50708).map(|v| match v {
        Values::Text(t) => t.clone(),
        _ => String::new(),
    });
    Ok(DecodedFrame {
        pixels,
        unrecovered: None,
        color: SourceColor {
            primaries: Primaries::Srgb,
            transfer: Transfer::Linear,
            reference: ReferenceDomain::Scene,
        },
        provenance: InputProvenance {
            source_hash: blake3::hash(bytes).to_hex().to_string(),
            transform_decision: "float_linear_dng_to_linear_srgb_scene".into(),
            decoder_revision: "v3-float-linear-dng-1",
            interpretation: format!(
                "floating-point LinearRaw DNG ({})",
                camera.unwrap_or_default()
            ),
            profile_hash: None,
            warnings,
            calibration: None,
        },
        source_profile: None,
        rendered_origin: false,
        input_domain: None,
    })
}

/// DNG's 24-bit float: sign, 7-bit exponent (bias 63), 16-bit mantissa.
fn f24_to_f32(v: u32) -> f32 {
    let sign = (v >> 23) & 1;
    let exponent = ((v >> 16) & 0x7f) as i32;
    let mantissa = v & 0xffff;
    let bits = if exponent == 0 {
        if mantissa == 0 {
            sign << 31
        } else {
            // Subnormal: normalise.
            let mut e = -62i32;
            let mut m = mantissa;
            while m & 0x10000 == 0 {
                m <<= 1;
                e -= 1;
            }
            (sign << 31) | (((e + 127) as u32) << 23) | ((m & 0xffff) << 7)
        }
    } else if exponent == 0x7f {
        (sign << 31) | (0xff << 23) | (mantissa << 7)
    } else {
        (sign << 31) | (((exponent - 63 + 127) as u32) << 23) | (mantissa << 7)
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A merge written and read back keeps its light (to half-float
    /// precision) and its brightness past white.
    #[test]
    fn a_float_dng_round_trips() {
        let (w, h) = (71u32, 45u32);
        let linear = Rgb32FImage::from_fn(w, h, |x, y| {
            let v = 0.0005 * 1.12f32.powi((x + y) as i32);
            image::Rgb([v * 0.9, v, v * 1.1])
        });
        let peak = linear.as_raw().iter().copied().fold(0.0f32, f32::max);
        assert!(peak > 1.0, "the test needs light past white");
        let dir = std::env::temp_dir().join("darkroom-float-dng-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("merge.dng");
        let camera = DngCamera {
            make: "Sony".into(),
            model: "ILCE-7M3".into(),
        };
        write_float_dng(&path, &linear, &camera, Compression::Lossless, None).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(is_float_linear_dng(&bytes));
        let frame = decode_float_dng(&bytes).unwrap();
        assert_eq!(frame.pixels.dimensions(), (w, h));
        // Half floats hold 11 bits of precision down to 6.1e-5 of the
        // stored peak (about 14 stops below the brightest light), then fewer.
        let floor = peak * 6.2e-5;
        let (mut worst, mut worst_deep) = (0.0f32, 0.0f32);
        for (x, y, p) in linear.enumerate_pixels() {
            let q = frame.pixels.get_pixel(x, y);
            for c in 0..3 {
                let error = ((q[c] - p[c]) / p[c]).abs();
                if p[c] >= floor {
                    worst = worst.max(error);
                } else {
                    worst_deep = worst_deep.max(error);
                }
            }
        }
        assert!(worst < 1e-3, "round trip differs by {worst}");
        assert!(worst_deep < 0.05, "deepest shadows differ by {worst_deep}");
    }

    /// A compact merge reads back close to the original, past white too,
    /// at a fraction of the size.
    #[test]
    fn a_compact_dng_round_trips() {
        let (w, h) = (700u32, 530u32);
        let linear = Rgb32FImage::from_fn(w, h, |x, y| {
            let v = 0.08 + 3.0 * (x as f32 / w as f32).powi(2) + 0.05 * ((y as f32) * 0.05).sin();
            image::Rgb([v * 0.8, v, v * 1.2])
        });
        let dir = std::env::temp_dir().join("darkroom-float-dng-test");
        let _ = std::fs::create_dir_all(&dir);
        let camera = DngCamera {
            make: "Sony".into(),
            model: "ILCE-7M3".into(),
        };
        let (lossless, compact) = (dir.join("lossless.dng"), dir.join("compact.dng"));
        write_float_dng(&lossless, &linear, &camera, Compression::Lossless, None).unwrap();
        write_float_dng(
            &compact,
            &linear,
            &camera,
            COMPACT,
            Some(&image::RgbImage::new(64, 48)),
        )
        .unwrap();
        let bytes = std::fs::read(&compact).unwrap();
        assert!(is_float_linear_dng(&bytes));
        assert!(bytes.len() * 4 < std::fs::metadata(&lossless).unwrap().len() as usize);
        let frame = decode_float_dng(&bytes).unwrap();
        assert_eq!(frame.pixels.dimensions(), (w, h));
        // Lossy: judged on the typical pixel and the 99th percentile.
        let mut errors: Vec<f32> = linear
            .enumerate_pixels()
            .map(|(x, y, p)| (frame.pixels.get_pixel(x, y)[1] / p[1]).log2().abs())
            .collect();
        errors.sort_by(f32::total_cmp);
        let mean = errors.iter().sum::<f32>() / errors.len() as f32;
        let p99 = errors[errors.len() * 99 / 100];
        eprintln!("compact: mean {mean:.4}, p99 {p99:.4} stops");
        assert!(
            mean < 0.02 && p99 < 0.08,
            "compact differs: mean {mean}, p99 {p99} stops"
        );
    }

    #[test]
    fn f24_matches_known_values() {
        assert_eq!(f24_to_f32(0), 0.0);
        // 1.0: exponent 63, mantissa 0.
        assert_eq!(f24_to_f32(63 << 16), 1.0);
        // 1.5 and -2.0.
        assert_eq!(f24_to_f32((63 << 16) | 0x8000), 1.5);
        assert_eq!(f24_to_f32((1 << 23) | (64 << 16)), -2.0);
    }
}
