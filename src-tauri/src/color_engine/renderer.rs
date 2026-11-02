use super::plan::RenderPlan;
use crate::image_processing::GpuContext;
use anyhow::{Result, anyhow, ensure};
use image::{DynamicImage, ImageBuffer, ImageEncoder, Rgba, Rgba32FImage};
use wgpu::util::DeviceExt;

pub struct StageCapture {
    /// Scene/display-relative linear DaVinci Wide Gamut, before exposure.
    pub working: Rgba32FImage,
    /// Same space after exposure. Values above one/negative values are retained.
    pub graded: Rgba32FImage,
}

/// A pixel rectangle `(x, y, w, h)`.
pub type Rect = (u32, u32, u32, u32);

pub struct RenderedFrame {
    /// Encoded values in `space` (sRGB unless a render asked for Display P3;
    /// both use the sRGB transfer curve and D65), bounded to [0,1], straight
    /// alpha. Not monitor-converted.
    pub encoded_srgb: Rgba32FImage,
    /// The colour space `encoded_srgb` is in, and the profile files carry.
    pub space: super::config::OutputSpace,
    pub stages: Option<StageCapture>,
    /// The full-resolution size this render stands for; equal to the image's
    /// own size unless a preview was rendered from a downscaled picture.
    pub full_size: (u32, u32),
    /// Where the photo's tones sat when the tone sliders adapted to it, if
    /// they were in use (for calibration tools).
    pub tones: Option<super::plan::PhotoTones>,
    /// For a render of part of the picture: that part's pixel rectangle
    /// `(x, y, w, h)` in the whole picture at this size, and the whole
    /// picture's `(width, height)`.
    pub region: Option<(Rect, (u32, u32))>,
}

/// The pixel rectangle `(x, y, w, h)` of the region at normalised
/// `(x, y, w, h)` of a `width` x `height` picture: whole pixels covering it,
/// never past the edge and never empty.
pub fn region_rect(
    region: (f32, f32, f32, f32),
    (width, height): (u32, u32),
) -> (u32, u32, u32, u32) {
    let (rx, ry, rw, rh) = region;
    let x0 = ((rx.clamp(0.0, 1.0) * width as f32).floor() as u32).min(width - 1);
    let y0 = ((ry.clamp(0.0, 1.0) * height as f32).floor() as u32).min(height - 1);
    let x1 = (((rx + rw).clamp(0.0, 1.0) * width as f32).ceil() as u32).clamp(x0 + 1, width);
    let y1 = (((ry + rh).clamp(0.0, 1.0) * height as f32).ceil() as u32).clamp(y0 + 1, height);
    (x0, y0, x1 - x0, y1 - y0)
}

impl RenderedFrame {
    /// Keep only the region at normalised `(x, y, w, h)` of the picture: a
    /// zoomed-in editor is sent what is on screen, not all of it. Returns
    /// the region's pixel rectangle `(x, y, w, h)` in the full render.
    pub fn crop_to_region(&mut self, region: (f32, f32, f32, f32)) -> (u32, u32, u32, u32) {
        let canvas = self.encoded_srgb.dimensions();
        let rect = region_rect(region, canvas);
        let (x0, y0, w, h) = rect;
        self.encoded_srgb = image::imageops::crop_imm(&self.encoded_srgb, x0, y0, w, h).to_image();
        self.stages = None;
        self.region = Some((rect, canvas));
        rect
    }

    /// The previous engine's clipping warning: red where any channel is at
    /// white, blue where any is at black. For display only; never exported.
    pub fn mark_clipping(&mut self) {
        use rayon::prelude::*;
        self.encoded_srgb.par_chunks_mut(4).for_each(|p| {
            if p[..3].iter().any(|&v| v > 0.998) {
                p[..3].copy_from_slice(&[1., 0., 0.]);
            } else if p[..3].iter().any(|&v| v < 0.002) {
                p[..3].copy_from_slice(&[0., 0., 1.]);
            }
        });
    }

    /// Embed the output profile (sRGB or Display P3, per `space`), so viewers
    /// interpret these values correctly rather than as the monitor's gamut.
    pub fn write_srgb_png(&self, writer: impl std::io::Write, sixteen_bit: bool) -> Result<()> {
        let image = if sixteen_bit {
            self.export_rgba16()
        } else {
            DynamicImage::ImageRgba8(self.preview_rgba8())
        };
        self.encode_png(image::codecs::png::PngEncoder::new(writer), image)
    }

    /// As `write_srgb_png`, for the on-screen image: eight bits, dithered,
    /// with the fast deflate setting, about 30 times
    /// quicker to encode than the default (35 ms against 990 ms for a
    /// 3456-pixel preview) at the same size, and lossless all the same.
    /// One filter for every row ("Up") rather than choosing per row: 40%
    /// quicker (17 ms against 29 ms at 2048 pixels, 55 against 92 at 3584)
    /// for files 4-5% larger.
    pub fn write_display_png(&self, writer: impl std::io::Write) -> Result<()> {
        let encoder = image::codecs::png::PngEncoder::new_with_quality(
            writer,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Up,
        );
        self.encode_png(encoder, DynamicImage::ImageRgba8(self.display_rgba8()))
    }

    /// As `write_display_png`, as a JPEG: a tenth of the data, for views
    /// that go through many photos quickly (Cull).
    pub fn write_display_jpeg(&self, writer: impl std::io::Write, quality: u8) -> Result<()> {
        use image::ImageEncoder;
        let rgba = self.display_rgba8();
        let rgb = DynamicImage::ImageRgba8(rgba).to_rgb8();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(writer, quality);
        encoder.set_icc_profile(self.space.icc_profile()?)?;
        encoder.write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )?;
        Ok(())
    }

    fn encode_png<W: std::io::Write>(
        &self,
        mut encoder: image::codecs::png::PngEncoder<W>,
        image: DynamicImage,
    ) -> Result<()> {
        encoder.set_icc_profile(self.space.icc_profile()?)?;
        image.write_with_encoder(encoder)?;
        Ok(())
    }

    pub fn preview_rgba8(&self) -> image::RgbaImage {
        self.encode_rows(|_, _, pixel| pixel.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8))
    }

    /// Each pixel through `encode` (given its x, y and value), a row per
    /// task across the cores; the same values as one pixel after another.
    fn encode_rows<T: image::Primitive + Send + Sync>(
        &self,
        encode: impl Fn(u32, u32, [f32; 4]) -> [T; 4] + Sync,
    ) -> ImageBuffer<Rgba<T>, Vec<T>>
    where
        Rgba<T>: image::Pixel<Subpixel = T>,
    {
        use rayon::prelude::*;
        let (width, height) = self.encoded_srgb.dimensions();
        let row = width as usize * 4;
        let mut data = vec![T::DEFAULT_MIN_VALUE; row * height as usize];
        data.par_chunks_mut(row)
            .zip(self.encoded_srgb.as_raw().par_chunks(row))
            .enumerate()
            .for_each(|(y, (out, pixels))| {
                for (x, (out, pixel)) in out
                    .chunks_exact_mut(4)
                    .zip(pixels.chunks_exact(4))
                    .enumerate()
                {
                    let pixel = [pixel[0], pixel[1], pixel[2], pixel[3]];
                    out.copy_from_slice(&encode(x as u32, y as u32, pixel));
                }
            });
        ImageBuffer::from_raw(width, height, data).expect("one value per channel")
    }

    /// The same 8-bit encoding, dithered, for the image a person looks at.
    ///
    /// Eight bits cannot hold the gradients this pipeline produces: rounding
    /// alone turns a slow ramp into flat plateaus with visible steps between
    /// them, which reads as a fault in the grade rather than in the encoding.
    /// Triangular noise of one LSB, deterministic per pixel, trades that for
    /// invisible noise and keeps the average exact.
    ///
    /// Deliberately not used for thumbnails, the scopes or the inspection
    /// image: those measure the picture, and should not measure the dither.
    pub fn display_rgba8(&self) -> image::RgbaImage {
        self.encode_rows(|x, y, pixel| {
            std::array::from_fn(|c| {
                let level = pixel[c].clamp(0.0, 1.0) * 255.0;
                // Alpha is a coverage value, not a tone: leave it exact.
                let noise = if c == 3 {
                    0.0
                } else {
                    uniform(x, y, c as u32 * 2) + uniform(x, y, c as u32 * 2 + 1) - 1.0
                };
                (level + noise).round().clamp(0.0, 255.0) as u8
            })
        })
    }

    pub fn export_rgba16(&self) -> DynamicImage {
        DynamicImage::ImageRgba16(
            self.encode_rows(|_, _, pixel| {
                pixel.map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
            }),
        )
    }
}

/// Triangular noise in (-1, 1) for one pixel and channel, fixed per pixel:
/// one LSB of it before rounding to eight bits trades banding for noise too
/// fine to see, and leaves the average exact.
pub(crate) fn tpdf(x: u32, y: u32, channel: u32) -> f32 {
    uniform(x, y, channel * 2) + uniform(x, y, channel * 2 + 1) - 1.0
}

/// Deterministic value in [0,1) for one pixel and channel. Fixed per pixel so
/// the same frame always encodes the same way.
fn uniform(x: u32, y: u32, channel: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9)
        ^ y.wrapping_mul(0x85EB_CA6B)
        ^ channel.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    h as f32 / u32::MAX as f32
}

pub struct ColorEngine {
    context: GpuContext,
    pipeline: wgpu::ComputePipeline,
    /// Largest chunk, in pixels. Only tests change it, to force chunk
    /// boundaries through images small enough to check pixel by pixel.
    chunk_pixels: usize,
    /// The previous engine's measured shadow correction, which the shared
    /// tone functions read. Uploaded once.
    shadow_correction: wgpu::Buffer,
    /// GPU memory kept from one pass to the next.
    resident: std::sync::Mutex<Resident>,
}

/// Past this, a scratch buffer is let go after its pass rather than kept for
/// the next: a preview's (64 MB at most) stay, a full-size stage capture's
/// do not.
const KEEP_SCRATCH: u64 = 160 << 20;
/// The most neighbourhood kept on the GPU between passes: a 4096-pixel
/// preview's is about 360 MB. Larger ones (full-size exports) are sent a
/// chunk at a time, as before.
const KEEP_NEIGHBOURHOOD: u64 = 384 << 20;
/// Pixels per piece when a chunk is copied and checked across threads.
const PIECE_PIXELS: usize = 1 << 16;

/// What a slider tick would otherwise rebuild on every pass. Each tick used
/// to allocate and free about 225 MB of buffers and send the neighbourhood
/// (32 bytes a pixel) and every lattice again, though none had changed.
#[derive(Default)]
struct Resident {
    /// Where the CPU writes a chunk of the source, mapped for writing.
    upload: Option<wgpu::Buffer>,
    source: Option<wgpu::Buffer>,
    results: Option<wgpu::Buffer>,
    readback: Option<wgpu::Buffer>,
    /// A chunk of a neighbourhood too large to keep (`KEEP_NEIGHBOURHOOD`).
    neighbourhood_chunk: Option<wgpu::Buffer>,
    parameters: Option<wgpu::Buffer>,
    /// Bound where a plan has no lattice or neighbourhood.
    dummy: Option<wgpu::Buffer>,
    cube: Lattice,
    look: Lattice,
    domain_in: Lattice,
    domain_out: Lattice,
    /// Whole neighbourhoods, most recent first, each held against the very
    /// allocation it was made from. The `Weak` keeps that address from being
    /// reused, and an `Arc<Vec>` cannot change while a `Weak` to it exists
    /// (`Arc::get_mut` refuses, `make_mut` moves it), so the same pointer
    /// means the same values. When the CPU cache lets one go, so does this.
    neighbourhoods: Vec<(std::sync::Weak<Vec<[f32; 4]>>, wgpu::Buffer)>,
}

/// A read-only lattice on the GPU, sent again only when its values change.
#[derive(Default)]
struct Lattice {
    /// The bytes it was made from, compared exactly on each pass (a few MB:
    /// far cheaper than sending it again).
    made_from: Vec<u8>,
    buffer: Option<wgpu::Buffer>,
}

impl Lattice {
    fn get(
        &mut self,
        device: &wgpu::Device,
        label: &str,
        made_from: &[u8],
        contents: impl FnOnce() -> Vec<[f32; 4]>,
    ) -> wgpu::Buffer {
        if self.buffer.is_none() || self.made_from != made_from {
            self.buffer = None;
            self.buffer = Some(
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: bytemuck::cast_slice(&contents()),
                    usage: wgpu::BufferUsages::STORAGE,
                }),
            );
            self.made_from.clear();
            self.made_from.extend_from_slice(made_from);
        }
        self.buffer.clone().expect("just made")
    }
}

/// A buffer of at least `size` bytes in `slot`, made (or made larger) only
/// when the one there is too small.
fn scratch(
    slot: &mut Option<wgpu::Buffer>,
    device: &wgpu::Device,
    label: &str,
    size: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    if slot.as_ref().is_none_or(|b| b.size() < size) {
        *slot = None;
        *slot = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        }));
    }
    slot.clone().expect("just made")
}

impl Resident {
    /// The neighbourhood on the GPU, sent now if it is not there yet; `None`
    /// when it is too large to keep.
    fn neighbourhood(
        &mut self,
        device: &wgpu::Device,
        blurs: &std::sync::Arc<Vec<[f32; 4]>>,
        limit: u64,
    ) -> Option<wgpu::Buffer> {
        self.neighbourhoods.retain(|(w, _)| w.strong_count() > 0);
        if let Some(i) = self
            .neighbourhoods
            .iter()
            .position(|(w, _)| std::ptr::eq(w.as_ptr(), std::sync::Arc::as_ptr(blurs)))
        {
            let hit = self.neighbourhoods.remove(i);
            let buffer = hit.1.clone();
            self.neighbourhoods.insert(0, hit);
            return Some(buffer);
        }
        let bytes: &[u8] = bytemuck::cast_slice(blurs.as_slice());
        if bytes.len() as u64 > limit || bytes.is_empty() {
            return None;
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("V3 neighbourhood"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        par_copy(
            buffer.slice(..).get_mapped_range_mut().slice(..),
            bytes,
            |_| true,
        );
        buffer.unmap();
        self.neighbourhoods
            .insert(0, (std::sync::Arc::downgrade(blurs), buffer.clone()));
        // The newest always; one more (a thumbnail's, say, between two
        // preview passes) while both fit.
        let mut total = 0;
        let keep = self
            .neighbourhoods
            .iter()
            .take(2)
            .take_while(|(_, b)| {
                total += b.size();
                total == b.size() || total <= KEEP_NEIGHBOURHOOD
            })
            .count();
        self.neighbourhoods.truncate(keep);
        Some(buffer)
    }

    /// Let go of the scratch buffers too large to keep between passes.
    fn trim(&mut self) {
        for slot in [
            &mut self.upload,
            &mut self.source,
            &mut self.results,
            &mut self.readback,
            &mut self.neighbourhood_chunk,
        ] {
            if slot.as_ref().is_some_and(|b| b.size() > KEEP_SCRATCH) {
                *slot = None;
            }
        }
    }
}

/// Copy `src` into `dst` across threads, checking each piece with `check`
/// on the way. False when any piece failed it.
fn par_copy(
    dst: wgpu::WriteOnly<'_, [u8]>,
    src: &[u8],
    check: impl Fn(&[u8]) -> bool + Sync,
) -> bool {
    use rayon::prelude::*;
    const PIECE: usize = PIECE_PIXELS * 16;
    assert_eq!(dst.len(), src.len(), "a copy fills its destination");
    // Whole pieces as fixed-size arrays, which can go to other threads.
    let (whole, mut rest) = dst.into_chunks::<PIECE>();
    let whole: Vec<wgpu::WriteOnly<'_, [u8; PIECE]>> = whole.into_iter().collect();
    let (head, tail) = src.split_at(whole.len() * PIECE);
    let copied = whole
        .into_par_iter()
        .zip(head.par_chunks(PIECE))
        .all(|(out, piece)| {
            let mut out: wgpu::WriteOnly<'_, [u8]> = out.into();
            out.copy_from_slice(piece);
            check(piece)
        });
    rest.copy_from_slice(tail);
    copied && check(tail)
}

/// Map `slice` and wait until it is.
fn map(
    device: &wgpu::Device,
    slice: wgpu::BufferSlice<'_>,
    mode: wgpu::MapMode,
    after: Option<wgpu::SubmissionIndex>,
) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(mode, move |r| {
        let _ = tx.send(r);
    });
    // A buffer no submission is using maps on the next check; one in use
    // waits for its submission.
    if after.is_none() {
        device.poll(wgpu::PollType::Poll)?;
        if let Ok(r) = rx.try_recv() {
            return Ok(r?);
        }
    }
    device.poll(wgpu::PollType::Wait {
        submission_index: after,
        timeout: Some(std::time::Duration::from_secs(30)),
    })?;
    rx.recv_timeout(std::time::Duration::from_secs(30))??;
    Ok(())
}

impl ColorEngine {
    pub fn new(context: GpuContext) -> Result<Self> {
        let device = &context.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Color v3 experimental"),
            source: wgpu::ShaderSource::Wgsl(super::shader_source().into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Color v3 exposure and SDR output"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(anyhow!("V3 shader validation failed: {error}"));
        }
        let table: Vec<f32> = include_bytes!("../shaders/shadow_correction_33.bin")
            .chunks_exact(2)
            .map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32())
            .collect();
        ensure!(
            table.len() == 33 * 33 * 33,
            "Shadow correction table is malformed"
        );
        let shadow_correction = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("V3 shadow correction"),
            contents: bytemuck::cast_slice(&table),
            usage: wgpu::BufferUsages::STORAGE,
        });
        Ok(Self {
            context,
            pipeline,
            chunk_pixels: 1 << 22,
            shadow_correction,
            resident: Default::default(),
        })
    }

    /// Bounded buffer dispatch avoids full-photo intermediate GPU allocations.
    /// No spatial operations yet: chunk boundaries cannot change pixel results.
    /// The same engine with a smaller chunk, so a test can put boundaries
    /// wherever it likes.
    pub fn with_chunk_pixels(mut self, pixels: usize) -> Self {
        self.chunk_pixels = pixels.max(1);
        self
    }

    pub fn render(
        &self,
        input: &Rgba32FImage,
        plan: &RenderPlan,
        capture: bool,
    ) -> Result<RenderedFrame> {
        let (output, stages) = self.run(input, plan, if capture { 1 } else { 0 })?;
        Ok(RenderedFrame {
            full_size: output.dimensions(),
            encoded_srgb: output,
            space: super::config::OutputSpace::Srgb,
            stages,
            tones: None,
            region: None,
        })
    }

    /// Only the graded working-space stage: what local adjustments consume.
    /// Reads back a third of what a full capture does.
    pub fn render_graded(&self, input: &Rgba32FImage, plan: &RenderPlan) -> Result<Rgba32FImage> {
        Ok(self.run(input, plan, 2)?.0)
    }

    /// `mode`: 0 encoded output, 1 every stage, 2 graded only.
    fn run(
        &self,
        input: &Rgba32FImage,
        plan: &RenderPlan,
        mode: u32,
    ) -> Result<(Rgba32FImage, Option<StageCapture>)> {
        let mut resident = self
            .resident
            .lock()
            .map_err(|_| anyhow!("V3 renderer unavailable"))?;
        let result = self.run_with(&mut resident, input, plan, mode);
        if result.is_err() {
            // A pass that stopped part way may have left a buffer mapped or
            // waiting to be: start the next one afresh.
            *resident = Resident::default();
        } else {
            resident.trim();
        }
        result
    }

    fn run_with(
        &self,
        resident: &mut Resident,
        input: &Rgba32FImage,
        plan: &RenderPlan,
        mode: u32,
    ) -> Result<(Rgba32FImage, Option<StageCapture>)> {
        use rayon::prelude::*;
        let capture = mode == 1;
        let (width, height) = input.dimensions();
        ensure!(width > 0 && height > 0, "Cannot render an empty image");
        let device = &self.context.device;
        let queue = &self.context.queue;
        let limits = device.limits();
        // Lattices are not chunked. Refuse an oversized LUT before wgpu
        // creates an invalid binding (a 256^3 vec4 lattice is 256 MiB).
        let lattice_limit = limits
            .max_storage_buffer_binding_size
            .min(limits.max_buffer_size);
        let fits = |len: usize| (len as u64).saturating_mul(16) <= lattice_limit;
        ensure!(
            plan.cube.as_ref().is_none_or(|c| fits(c.entries.len()))
                && plan.look.as_ref().is_none_or(|l| fits(l.data.len() / 3))
                && plan
                    .domain
                    .as_ref()
                    .is_none_or(|(i, o)| fits(i.entries.len()) && fits(o.entries.len())),
            "This LUT exceeds the GPU's storage limit; use a smaller lattice"
        );
        let stride = if capture { 48 } else { 16 };
        let total = input.as_raw().len() / 4;
        // Every chunk is a full CPU/GPU round trip, so chunk size decides the
        // interactive cost: at 65,536 pixels a preview pass made 26 of them
        // and took 44 ms. Four megapixels covers a whole preview in one, and
        // still bounds GPU memory for a full-resolution export.
        let capacity = self
            .chunk_pixels
            .min(limits.max_storage_buffer_binding_size as usize / stride.max(32))
            .min(limits.max_buffer_size as usize / stride)
            .min(limits.max_compute_workgroups_per_dimension as usize * 64)
            .min(total);
        ensure!(capacity > 0, "GPU storage limits are insufficient for v3");
        // Two entries per pixel; one dummy pair when the plan carries no
        // neighbourhood.
        if let Some(n) = &plan.neighbourhood {
            ensure!(
                n.len() == total * 2,
                "Neighbourhood does not match the image"
            );
        }
        let upload = scratch(
            &mut resident.upload,
            device,
            "V3 source upload",
            (capacity * 16) as u64,
            wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
        );
        let source = scratch(
            &mut resident.source,
            device,
            "V3 source chunk",
            (capacity * 16) as u64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let results = scratch(
            &mut resident.results,
            device,
            "V3 captured stages",
            (capacity * stride) as u64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = scratch(
            &mut resident.readback,
            device,
            "V3 stage readback",
            (capacity * stride) as u64,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let parameters = scratch(
            &mut resident.parameters,
            device,
            "V3 parameters",
            std::mem::size_of_val(&plan.parameters) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let dummy = resident
            .dummy
            .get_or_insert_with(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("V3 unused binding"),
                    contents: &[0u8; 32],
                    usage: wgpu::BufferUsages::STORAGE,
                })
            })
            .clone();
        // Always bound: the layout is fixed, so a plan without a captured
        // transform still supplies one entry for the binding to point at.
        let cube = match &plan.cube {
            Some(c) => resident.cube.get(
                device,
                "V3 output transform",
                bytemuck::cast_slice(&c.entries),
                || c.entries.clone(),
            ),
            None => dummy.clone(),
        };
        let look = match &plan.look {
            Some(l) => resident.look.get(
                device,
                "V3 creative LUT",
                bytemuck::cast_slice(&l.data),
                || super::plan::padded_lattice(l),
            ),
            None => dummy.clone(),
        };
        let (idt, odt) = match &plan.domain {
            Some((i, o)) => (
                resident.domain_in.get(
                    device,
                    "V3 input transform",
                    bytemuck::cast_slice(&i.entries),
                    || i.entries.clone(),
                ),
                resident.domain_out.get(
                    device,
                    "V3 display domain output transform",
                    bytemuck::cast_slice(&o.entries),
                    || o.entries.clone(),
                ),
            ),
            None => (dummy.clone(), dummy.clone()),
        };
        // The whole neighbourhood stays on the GPU between passes when it
        // fits; otherwise it goes a chunk at a time with the source.
        let resident_blurs = plan.neighbourhood.as_ref().and_then(|n| {
            let limit = KEEP_NEIGHBOURHOOD
                .min(limits.max_storage_buffer_binding_size)
                .min(limits.max_buffer_size);
            resident.neighbourhood(device, n, limit)
        });
        let chunked_blurs = match (&plan.neighbourhood, &resident_blurs) {
            (Some(_), None) => Some(scratch(
                &mut resident.neighbourhood_chunk,
                device,
                "V3 neighbourhood chunk",
                (capacity * 32) as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            )),
            _ => None,
        };
        let blurs = resident_blurs
            .as_ref()
            .or(chunked_blurs.as_ref())
            .unwrap_or(&dummy);
        let buffers = [
            &source,
            &results,
            &parameters,
            &cube,
            &look,
            blurs,
            &self.shadow_correction,
            &idt,
            &odt,
        ];
        let entries: Vec<_> = buffers
            .iter()
            .enumerate()
            .map(|(binding, b)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("V3 bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut output = vec![0f32; total * 4];
        let mut working = capture.then(|| vec![0f32; total * 4]);
        let mut graded = capture.then(|| vec![0f32; total * 4]);
        for (index, chunk) in input.as_raw().chunks(capacity * 4).enumerate() {
            let count = chunk.len() / 4;
            let first = index * capacity;
            let bytes = (count * 16) as u64;
            // The source goes straight into mapped memory, checked as it
            // goes, on every core: the check was a serial pass of its own.
            let slice = upload.slice(..bytes);
            map(device, slice, wgpu::MapMode::Write, None)?;
            let valid = par_copy(
                slice.get_mapped_range_mut().slice(..),
                bytemuck::cast_slice(chunk),
                |piece| {
                    bytemuck::cast_slice::<u8, f32>(piece)
                        .chunks_exact(4)
                        .all(|p| p.iter().all(|v| v.is_finite()) && (0.0..=1.0).contains(&p[3]))
                },
            );
            upload.unmap();
            ensure!(
                valid,
                "Input must contain finite RGB and straight alpha in [0,1]"
            );
            let mut params = plan.parameters;
            params.modes[2] = count as u32;
            params.modes[3] = mode;
            params.frame = [width, height, first as u32, 0];
            if let (Some(n), Some(buffer)) = (&plan.neighbourhood, &chunked_blurs) {
                params.frame[3] = first as u32;
                queue.write_buffer(
                    buffer,
                    0,
                    bytemuck::cast_slice(&n[first * 2..(first + count) * 2]),
                );
            }
            queue.write_buffer(&parameters, 0, bytemuck::bytes_of(&params));
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(&upload, 0, &source, 0, bytes);
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups((count as u32).div_ceil(64), 1, 1);
            }
            encoder.copy_buffer_to_buffer(&results, 0, &readback, 0, (count * stride) as u64);
            let submission = queue.submit(Some(encoder.finish()));
            let slice = readback.slice(..(count * stride) as u64);
            map(device, slice, wgpu::MapMode::Read, Some(submission))?;
            // Copied out and checked on every core: the serial check over
            // mapped memory was a fifth of a slider tick.
            let finite = {
                let mapped = slice.get_mapped_range();
                let floats: &[f32] = bytemuck::cast_slice(&mapped);
                let piece = PIECE_PIXELS * 4;
                let span = first * 4..(first + count) * 4;
                match (&mut working, &mut graded) {
                    (Some(working), Some(graded)) => floats
                        .par_chunks(piece * 3)
                        .zip(working[span.clone()].par_chunks_mut(piece))
                        .zip(graded[span.clone()].par_chunks_mut(piece))
                        .zip(output[span].par_chunks_mut(piece))
                        .all(|(((from, working), graded), output)| {
                            for (i, pixel) in from.chunks_exact(12).enumerate() {
                                let at = i * 4..i * 4 + 4;
                                working[at.clone()].copy_from_slice(&pixel[..4]);
                                graded[at.clone()].copy_from_slice(&pixel[4..8]);
                                output[at].copy_from_slice(&pixel[8..]);
                            }
                            from.iter().all(|v| v.is_finite())
                        }),
                    _ => floats
                        .par_chunks(piece)
                        .zip(output[span].par_chunks_mut(piece))
                        .all(|(from, to)| {
                            to.copy_from_slice(from);
                            from.iter().all(|v| v.is_finite())
                        }),
                }
            };
            readback.unmap();
            ensure!(finite, "V3 produced a non-finite pixel; check input range");
        }
        let image =
            |data| ImageBuffer::from_raw(width, height, data).expect("validated dimensions");
        Ok((
            image(output),
            working.zip(graded).map(|(w, g)| StageCapture {
                working: image(w),
                graded: image(g),
            }),
        ))
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;

    #[test]
    fn a_region_is_exactly_that_part_of_the_render() {
        let full = Rgba32FImage::from_fn(300, 200, |x, y| {
            image::Rgba([x as f32 / 300.0, y as f32 / 200.0, 0.5, 1.0])
        });
        let mut frame = RenderedFrame {
            encoded_srgb: full.clone(),
            space: super::super::config::OutputSpace::Srgb,
            stages: None,
            full_size: (300, 200),
            tones: None,
            region: None,
        };
        let (x, y, w, h) = frame.crop_to_region((0.25, 0.1, 0.5, 0.3));
        assert_eq!((x, y, w, h), (75, 20, 150, 60));
        for (dx, dy, p) in frame.encoded_srgb.enumerate_pixels() {
            assert_eq!(p, full.get_pixel(x + dx, y + dy));
        }
        // Past the edge, it stops at the edge.
        let mut frame = RenderedFrame {
            encoded_srgb: full.clone(),
            ..frame
        };
        assert_eq!(
            frame.crop_to_region((0.9, 0.9, 0.5, 0.5)),
            (270, 180, 30, 20)
        );
    }
}
