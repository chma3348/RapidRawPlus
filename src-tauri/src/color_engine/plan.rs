use super::{config::*, cube::CubeLut, spaces};
use anyhow::{Result, ensure};
use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct GpuParameters {
    pub source_to_work: [[f32; 4]; 3],
    pub work_to_output: [[f32; 4]; 3],
    pub modes: [u32; 4], // transfer, rendering, pixel count, capture stages
    /// Stage switches, so the shader never has to read "is this control
    /// enabled" out of a value that also means something else:
    /// any control active, tone chain active, colour stage active, unused.
    pub flags: [u32; 4],
    pub work_to_lms: [[f32; 4]; 3],
    pub lms_to_work: [[f32; 4]; 3],
    pub white_balance: [[f32; 4]; 3],
    pub tone: [f32; 4],
    pub zones: [f32; 4],
    pub color: [f32; 4],
    pub bands: [[f32; 4]; 8],
    pub grading: [[f32; 4]; 4],
    pub curve: [[f32; 4]; 5],
    pub range_center: [[f32; 4]; 8],
    pub range_width: [[f32; 4]; 8],
    pub range_adjustment: [[f32; 4]; 8],
    /// Width, height, and the first pixel of the chunk being rendered — the
    /// pass walks the image as a flat list, and effects need to know where a
    /// pixel is. Set by the renderer per chunk.
    pub frame: [u32; 4],
    /// [vignette amount, midpoint, roundness, feather],
    /// [grain amplitude, cell size in full-resolution pixels, roughness,
    ///  render scale].
    pub effects: [[f32; 4]; 2],
    /// Red, green, blue curve knots, five each: [value, slope, 0, 0].
    pub channel_curves: [[f32; 4]; 15],
    /// x: channel curves active.
    pub curve_flags: [u32; 4],
}

pub struct RenderPlan {
    config: PipelineConfig,
    pub(crate) parameters: GpuParameters,
    pub(crate) cube: Option<CubeLut>,
}

impl RenderPlan {
    pub fn build(config: PipelineConfig) -> Result<Self> {
        config.controls.validate()?;
        ensure!(
            config.process_version == 3,
            "V3 render plan requires process_version 3"
        );
        ensure!(
            config.working_space == Primaries::DavinciWideGamut,
            "This prototype uses linear DaVinci Wide Gamut internally"
        );
        ensure!(
            matches!(
                (config.source.reference, config.output_rendering),
                (ReferenceDomain::Scene, OutputRendering::SceneShoulderV1)
                    | (ReferenceDomain::Scene, OutputRendering::SceneLuminanceV1)
                    | (ReferenceDomain::Scene, OutputRendering::SceneLuminanceV2)
                    | (ReferenceDomain::Display, OutputRendering::DisplayGamutV1)
                    | (ReferenceDomain::Display, OutputRendering::DisplayGamutV2)
                    // A captured transform stands in for the whole rendering
                    // step, so it is the right end for either kind of source:
                    // it is what Resolve itself applies after its own input
                    // transform has brought the source into the timeline.
                    | (ReferenceDomain::Scene, OutputRendering::ResolveCubeV1)
                    | (ReferenceDomain::Display, OutputRendering::ResolveCubeV1)
                    | (
                        ReferenceDomain::Display,
                        OutputRendering::DisplayPassthroughV1
                    )
            ),
            "Output rendering must match the source reference domain; refusing a double/missing display transform"
        );
        let wants_cube = config.output_rendering == OutputRendering::ResolveCubeV1;
        ensure!(
            wants_cube == config.output_lut.is_some(),
            "ResolveCubeV1 needs an output_lut, and the other renderings must not carry one"
        );
        let cube = config
            .output_lut
            .as_ref()
            .map(|path| CubeLut::load(path))
            .transpose()?;
        let packed = |matrix: glam::DMat3| {
            matrix
                .to_cols_array_2d()
                .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32, 0.0])
        };
        let c = &config.controls;
        // Relative white-balance correction in Bradford cone coordinates.
        // This is not a Kelvin control and does not reapply as-shot camera WB.
        let bradford = glam::DMat3::from_cols_array(&[
            0.8951, -0.7502, 0.0389, 0.2664, 1.7135, -0.0685, -0.1614, 0.0367, 1.0296,
        ]);
        let gains = glam::DVec3::new(
            (c.temperature as f64 * 0.006 + c.tint as f64 * 0.002).exp2(),
            (-c.tint as f64 * 0.004).exp2(),
            (-c.temperature as f64 * 0.006 + c.tint as f64 * 0.002).exp2(),
        );
        let xyz = spaces::rgb_to_xyz(config.working_space);
        let wb = if c.temperature == 0. && c.tint == 0. {
            glam::DMat3::IDENTITY
        } else {
            xyz.inverse() * bradford.inverse() * glam::DMat3::from_diagonal(gains) * bradford * xyz
        };
        let parameters = GpuParameters {
            source_to_work: packed(spaces::conversion(
                config.source.primaries,
                config.working_space,
            )),
            work_to_output: packed(spaces::conversion(config.working_space, Primaries::Srgb)),
            modes: [
                match config.source.transfer {
                    Transfer::Linear => 0,
                    Transfer::Srgb => 1,
                    Transfer::DavinciIntermediate => 2,
                },
                match config.output_rendering {
                    OutputRendering::DisplayPassthroughV1 => 0,
                    OutputRendering::SceneShoulderV1 => 1,
                    OutputRendering::SceneLuminanceV1 => 2,
                    OutputRendering::DisplayGamutV1 => 3,
                    OutputRendering::SceneLuminanceV2 => 4,
                    OutputRendering::DisplayGamutV2 => 5,
                    OutputRendering::ResolveCubeV1 => 6,
                },
                0,
                0,
            ],
            work_to_lms: packed(spaces::lms_from_rgb(config.working_space)),
            lms_to_work: packed(spaces::rgb_from_lms(config.working_space)),
            white_balance: packed(wb),
            flags: [
                u32::from(!c.is_neutral()),
                u32::from(!c.tone_is_neutral()),
                u32::from(!c.color_is_neutral()),
                cube.as_ref().map_or(0, |c| c.size),
            ],
            tone: [
                (c.contrast / 100.).exp2(),
                c.pivot,
                c.exposure.exp2(),
                0.,
            ],
            zones: [
                c.shadows * 0.02,
                c.highlights * 0.02,
                c.blacks * 0.02,
                c.whites * 0.02,
            ],
            color: [
                1. + c.saturation / 100.,
                c.vibrance / 100.,
                c.hue.to_radians(),
                0.,
            ],
            bands: c
                .bands
                .map(|b| [b[0].to_radians(), b[1] / 100., b[2] / 100., 0.]),
            grading: c.grading.map(|g| {
                let h = g[0].to_radians();
                [
                    h.cos() * g[1] * 0.001,
                    h.sin() * g[1] * 0.001,
                    g[2] * 0.002,
                    0.,
                ]
            }),
            curve: super::controls::curve_parameters(c.curve),
            range_center: std::array::from_fn(|i| {
                c.ranges.get(i).map_or([0.; 4], |r| {
                    [
                        r.center[0].to_radians(),
                        r.center[1],
                        r.center[2],
                        if r.adjustment == [0.; 3] { 0. } else { 1. },
                    ]
                })
            }),
            range_width: std::array::from_fn(|i| {
                c.ranges.get(i).map_or([1.; 4], |r| {
                    [r.width[0].to_radians(), r.width[1], r.width[2], 0.]
                })
            }),
            frame: [0; 4],
            channel_curves: {
                let mut knots = [[0.0f32; 4]; 15];
                for (c, curve) in c.channel_curves.iter().enumerate() {
                    for (k, knot) in super::controls::curve_parameters(*curve).into_iter().enumerate() {
                        knots[c * 5 + k] = knot;
                    }
                }
                knots
            },
            curve_flags: [u32::from(!c.channel_curves_are_neutral()), 0, 0, 0],
            effects: [
                [
                    c.effects.vignette_amount / 100.,
                    c.effects.vignette_midpoint / 100.,
                    c.effects.vignette_roundness / 100.,
                    c.effects.vignette_feather / 100.,
                ],
                [
                    c.effects.grain_amount / 200. * 0.5,
                    c.effects.grain_size / 50.,
                    c.effects.grain_roughness / 100.,
                    1.0,
                ],
            ],
            range_adjustment: std::array::from_fn(|i| {
                c.ranges.get(i).map_or([0.; 4], |r| {
                    [
                        r.adjustment[0].to_radians(),
                        r.adjustment[1] / 100.,
                        r.adjustment[2] / 100.,
                        0.,
                    ]
                })
            }),
        };
        Ok(Self {
            config,
            parameters,
            cube,
        })
    }

    /// How much smaller than the full-resolution photograph the image being
    /// rendered is, so grain keeps its size relative to the photograph.
    pub fn set_render_scale(&mut self, scale: f32) {
        self.parameters.effects[1][3] = scale.max(1e-4);
    }

    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    /// Caller supplies a content/decoder revision, not merely a filename.
    /// Output is fixed to encoded sRGB in this revision; future destinations
    /// must be added to this key before introducing a render cache.
    pub fn fingerprint(&self, source_revision: &str) -> String {
        let mut hash = blake3::Hasher::new();
        hash.update(b"rapidraw-color-v3-controls-1\0");
        hash.update(&serde_json::to_vec(&self.config).expect("validated finite config"));
        // The cube's contents, not the path it was read from.
        if let Some(cube) = &self.cube {
            hash.update(cube.digest.as_bytes());
        }
        hash.update(source_revision.as_bytes());
        hash.finalize().to_hex().to_string()
    }
}
