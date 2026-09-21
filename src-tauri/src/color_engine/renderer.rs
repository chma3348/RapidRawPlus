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

pub struct RenderedFrame {
    /// Encoded sRGB/D65, bounded to [0,1], straight alpha. Not monitor-converted.
    pub encoded_srgb: Rgba32FImage,
    pub stages: Option<StageCapture>,
}

impl RenderedFrame {
    /// Embed the output profile, so supporting viewers interpret these values
    /// as sRGB rather than the monitor's native gamut.
    pub fn write_srgb_png(&self, writer: impl std::io::Write, sixteen_bit: bool) -> Result<()> {
        let image = if sixteen_bit {
            self.export_rgba16()
        } else {
            DynamicImage::ImageRgba8(self.preview_rgba8())
        };
        self.encode_png(writer, image)
    }

    /// As `write_srgb_png`, for the on-screen image: eight bits, dithered.
    pub fn write_display_png(&self, writer: impl std::io::Write) -> Result<()> {
        self.encode_png(writer, DynamicImage::ImageRgba8(self.display_rgba8()))
    }

    fn encode_png(&self, writer: impl std::io::Write, image: DynamicImage) -> Result<()> {
        let mut encoder = image::codecs::png::PngEncoder::new(writer);
        encoder.set_icc_profile(moxcms::ColorProfile::new_srgb().encode()?)?;
        image.write_with_encoder(encoder)?;
        Ok(())
    }

    pub fn preview_rgba8(&self) -> image::RgbaImage {
        ImageBuffer::from_fn(
            self.encoded_srgb.width(),
            self.encoded_srgb.height(),
            |x, y| {
                Rgba(
                    self.encoded_srgb
                        .get_pixel(x, y)
                        .0
                        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
                )
            },
        )
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
        ImageBuffer::from_fn(
            self.encoded_srgb.width(),
            self.encoded_srgb.height(),
            |x, y| {
                let pixel = self.encoded_srgb.get_pixel(x, y).0;
                Rgba(std::array::from_fn(|c| {
                    let level = pixel[c].clamp(0.0, 1.0) * 255.0;
                    // Alpha is a coverage value, not a tone: leave it exact.
                    let noise = if c == 3 {
                        0.0
                    } else {
                        uniform(x, y, c as u32 * 2) + uniform(x, y, c as u32 * 2 + 1) - 1.0
                    };
                    (level + noise).round().clamp(0.0, 255.0) as u8
                }))
            },
        )
    }

    pub fn export_rgba16(&self) -> DynamicImage {
        DynamicImage::ImageRgba16(ImageBuffer::from_fn(
            self.encoded_srgb.width(),
            self.encoded_srgb.height(),
            |x, y| {
                Rgba(
                    self.encoded_srgb
                        .get_pixel(x, y)
                        .0
                        .map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16),
                )
            },
        ))
    }
}

/// Deterministic value in [0,1) for one pixel and channel. Fixed per pixel so
/// the same frame always encodes the same way.
fn uniform(x: u32, y: u32, channel: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x9E37_79B9)
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
        Ok(Self { context, pipeline })
    }

    /// Bounded buffer dispatch avoids full-photo intermediate GPU allocations.
    /// No spatial operations yet: chunk boundaries cannot change pixel results.
    pub fn render(
        &self,
        input: &Rgba32FImage,
        plan: &RenderPlan,
        capture: bool,
    ) -> Result<RenderedFrame> {
        let (width, height) = input.dimensions();
        ensure!(width > 0 && height > 0, "Cannot render an empty image");
        ensure!(
            input
                .pixels()
                .all(|p| p.0.iter().all(|v| v.is_finite()) && (0.0..=1.0).contains(&p[3])),
            "Input must contain finite RGB and straight alpha in [0,1]"
        );
        let device = &self.context.device;
        let queue = &self.context.queue;
        let limits = device.limits();
        let stride = if capture { 48 } else { 16 };
        let capacity = 65536usize
            .min(limits.max_storage_buffer_binding_size as usize / stride)
            .min(limits.max_buffer_size as usize / stride)
            .min(limits.max_compute_workgroups_per_dimension as usize * 64)
            .min(input.as_raw().len() / 4);
        ensure!(capacity > 0, "GPU storage limits are insufficient for v3");
        let source = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("V3 source chunk"),
            size: (capacity * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let results = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("V3 captured stages"),
            size: (capacity * stride) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("V3 stage readback"),
            size: (capacity * stride) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Always bound: the layout is fixed, so a plan without a captured
        // transform still supplies one entry for the binding to point at.
        let cube = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("V3 output transform"),
            contents: bytemuck::cast_slice(
                plan.cube
                    .as_ref()
                    .map_or(&[[0.0f32; 4]][..], |c| c.entries.as_slice()),
            ),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let parameters = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("V3 parameters"),
            contents: bytemuck::bytes_of(&plan.parameters),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("V3 bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: source.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: results.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: cube.as_entire_binding(),
                },
            ],
        });
        let mut output = Vec::with_capacity(input.as_raw().len());
        let mut working = capture.then(|| Vec::with_capacity(input.as_raw().len()));
        let mut graded = capture.then(|| Vec::with_capacity(input.as_raw().len()));
        for chunk in input.as_raw().chunks(capacity * 4) {
            let count = chunk.len() / 4;
            let mut params = plan.parameters;
            params.modes[2] = count as u32;
            params.modes[3] = u32::from(capture);
            queue.write_buffer(&source, 0, bytemuck::cast_slice(chunk));
            queue.write_buffer(&parameters, 0, bytemuck::bytes_of(&params));
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups((count as u32).div_ceil(64), 1, 1);
            }
            encoder.copy_buffer_to_buffer(&results, 0, &readback, 0, (count * stride) as u64);
            let submission = queue.submit(Some(encoder.finish()));
            let slice = readback.slice(..(count * stride) as u64);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(std::time::Duration::from_secs(30)),
            })?;
            rx.recv_timeout(std::time::Duration::from_secs(30))??;
            {
                let mapped = slice.get_mapped_range();
                for pixel in mapped.chunks_exact(stride) {
                    let mut channels = [0.0; 12];
                    for (dst, src) in channels.iter_mut().zip(pixel.chunks_exact(4)) {
                        *dst = f32::from_le_bytes(src.try_into().expect("four bytes"));
                    }
                    ensure!(
                        channels.iter().all(|v| v.is_finite()),
                        "V3 produced a non-finite pixel; check input range"
                    );
                    if let Some(v) = &mut working {
                        v.extend_from_slice(&channels[..4]);
                    }
                    if let Some(v) = &mut graded {
                        v.extend_from_slice(&channels[4..8]);
                    }
                    output.extend_from_slice(if capture {
                        &channels[8..]
                    } else {
                        &channels[..4]
                    });
                }
            }
            readback.unmap();
        }
        let image =
            |data| ImageBuffer::from_raw(width, height, data).expect("validated dimensions");
        Ok(RenderedFrame {
            encoded_srgb: image(output),
            stages: working.zip(graded).map(|(w, g)| StageCapture {
                working: image(w),
                graded: image(g),
            }),
        })
    }
}
