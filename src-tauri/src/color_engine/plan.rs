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
    /// [0, 0, exposure gain, 0].
    pub tone: [f32; 4],
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
    ///  render scale],
    /// [centre amount, film saturation, 0, 0].
    pub effects: [[f32; 4]; 3],
    /// Red, green, blue curve knots, five each: [value, slope, 0, 0].
    pub channel_curves: [[f32; 4]; 15],
    /// x: channel curves active.
    pub curve_flags: [u32; 4],
    /// A creative LUT: [intensity, exposure gain into the film simulation,
    /// 0, 0].
    pub look: [f32; 4],
    /// [lattice size (0 = none), input space: 1 display sRGB, 2 F-Log2 C,
    ///  3 DaVinci Intermediate, 0, 0].
    pub look_flags: [u32; 4],
    /// Working space to the film simulation's F-Gamut C.
    pub work_to_look: [[f32; 4]; 3],
    /// Calibration: [shadows tint, red hue, red sat, green hue],
    /// [green sat, blue hue, blue sat, active].
    pub calibration: [[f32; 4]; 2],
    /// Linear sRGB to the working space, where calibration is defined.
    pub srgb_to_work: [[f32; 4]; 3],
    /// The Basic panel, shared with the previous engine:
    /// [brightness, contrast, pivot, highlights], [shadows, whites, blacks, 0].
    pub basic: [[f32; 4]; 2],
    /// [active, scene-referred (the previous engine's RAW path),
    ///  neighbourhood bound, 0].
    pub basic_flags: [u32; 4],
    /// AgX's matrices, linear sRGB to its rendering space and back, as the
    /// previous engine computes them.
    pub agx_to: [[f32; 4]; 3],
    pub agx_from: [[f32; 4]; 3],
}

/// What a creative LUT expects to be fed, and therefore where in the
/// pipeline it goes. A LUT is a function from one encoding to another; using
/// one without saying which is how a look ends up rendered twice or not at
/// all, so each is placed where its input actually exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookSpace {
    /// sRGB display code values in and out: after the output rendering, as
    /// the previous engine applied every LUT and most downloadable looks
    /// expect.
    Display,
    /// DaVinci Intermediate in and out: a look made in a Resolve
    /// DaVinci Wide Gamut timeline. It runs on the graded scene data before
    /// the output rendering, as its node would in Resolve.
    Intermediate,
    /// F-Log2 C in, finished BT.709 display out: a Fujifilm film simulation.
    /// It *is* a rendering, so it takes the place of the output rendering.
    FLog2C,
}

impl LookSpace {
    pub fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("flog2c") => Self::FLog2C,
            Some("intermediate") => Self::Intermediate,
            _ => Self::Display,
        }
    }
}

/// A creative LUT and how to apply it.
#[derive(Clone)]
pub struct Look {
    pub lut: std::sync::Arc<crate::lut_processing::Lut>,
    pub space: LookSpace,
    /// 0..1: mixed with what the pipeline would otherwise have produced.
    pub intensity: f32,
    /// Stops, into a film simulation only: what the camera's exposure would
    /// have been, which such a LUT is very sensitive to.
    pub exposure: f32,
}

pub struct RenderPlan {
    config: PipelineConfig,
    pub(crate) parameters: GpuParameters,
    pub(crate) cube: Option<CubeLut>,
    /// The creative LUT's lattice, padded to `vec4`, when one is set.
    pub(crate) look: Option<Vec<[f32; 4]>>,
    /// Per pixel, the previous engine's tonal (3.5 px) and structure (40 px)
    /// blurs of the unedited picture, which its local tone controls read.
    pub(crate) neighbourhood: Option<std::sync::Arc<Vec<[f32; 4]>>>,
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
                    // The previous engine rendered both kinds of source with
                    // these, choosing its RAW or display path itself.
                    | (_, OutputRendering::PreviousBasic)
                    | (_, OutputRendering::PreviousAgx)
                    | (_, OutputRendering::PreviousFilmic)
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
                    OutputRendering::PreviousBasic => 7,
                    OutputRendering::PreviousAgx => 8,
                    OutputRendering::PreviousFilmic => 9,
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
            tone: [0., 0., c.tone.exposure.exp2(), 0.],
            basic: [
                [
                    c.tone.brightness,
                    c.tone.contrast,
                    c.tone.pivot,
                    c.tone.highlights,
                ],
                [c.tone.shadows, c.tone.whites, c.tone.blacks, 0.],
            ],
            basic_flags: [
                u32::from(!c.tone.is_neutral()),
                u32::from(config.source.reference == ReferenceDomain::Scene),
                0,
                0,
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
                    for (k, knot) in super::controls::curve_parameters(*curve)
                        .into_iter()
                        .enumerate()
                    {
                        knots[c * 5 + k] = knot;
                    }
                }
                knots
            },
            curve_flags: [u32::from(!c.channel_curves_are_neutral()), 0, 0, 0],
            calibration: {
                let k = &c.calibration;
                [
                    [
                        k.shadows_tint / 400.,
                        k.red_hue / 400.,
                        k.red_saturation / 120.,
                        k.green_hue / 400.,
                    ],
                    [
                        k.green_saturation / 120.,
                        k.blue_hue / 400.,
                        k.blue_saturation / 120.,
                        if k.is_neutral() { 0. } else { 1. },
                    ],
                ]
            },
            srgb_to_work: packed(spaces::conversion(Primaries::Srgb, config.working_space)),
            agx_to: packed(
                crate::image_processing::calculate_agx_matrices_glam()
                    .0
                    .as_dmat3(),
            ),
            agx_from: packed(
                crate::image_processing::calculate_agx_matrices_glam()
                    .1
                    .as_dmat3(),
            ),
            look: [0.; 4],
            look_flags: [0; 4],
            work_to_look: packed(
                glam::DMat3::from_cols_array_2d(
                    &crate::flog2c::SRGB_TO_FGAMUT_C.map(|r| r.map(f64::from)),
                )
                .transpose()
                    * spaces::conversion(config.working_space, Primaries::Srgb),
            ),
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
                [
                    c.effects.centre / super::optics::CENTRE_SCALE,
                    c.effects.film_saturation / 100.,
                    0.,
                    0.,
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
            look: None,
            neighbourhood: None,
        })
    }

    /// Apply a creative LUT on this plan's output. Only the pass that
    /// produces the finished picture should carry one.
    pub fn set_look(&mut self, look: &Look) -> Result<()> {
        let size = look.lut.size;
        ensure!(
            (2..=256).contains(&size) && look.lut.data.len() == (size as usize).pow(3) * 3,
            "The LUT's lattice is malformed"
        );
        ensure!(
            look.lut.data.iter().all(|v| v.is_finite()),
            "The LUT contains non-finite values"
        );
        ensure!(
            look.intensity.is_finite() && look.exposure.is_finite(),
            "Invalid LUT settings"
        );
        self.look = Some(
            look.lut
                .data
                .chunks_exact(3)
                .map(|c| [c[0], c[1], c[2], 0.])
                .collect(),
        );
        self.parameters.look = [
            look.intensity.clamp(0., 1.),
            look.exposure.clamp(-3., 3.).exp2(),
            0.,
            0.,
        ];
        self.parameters.look_flags = [
            size,
            match look.space {
                LookSpace::Display => 1,
                LookSpace::FLog2C => 2,
                LookSpace::Intermediate => 3,
            },
            0,
            0,
        ];
        Ok(())
    }

    /// Bind the neighbourhood the Basic tone controls read: two entries per
    /// pixel, tonal blur then structure blur, in the encoding the previous
    /// engine blurred in (see `neighbourhood` in application.rs).
    pub fn set_neighbourhood(&mut self, blurs: std::sync::Arc<Vec<[f32; 4]>>) {
        self.neighbourhood = Some(blurs);
        self.parameters.basic_flags[2] = 1;
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
        if let Some(look) = &self.look {
            hash.update(bytemuck::cast_slice(look));
            hash.update(bytemuck::bytes_of(&self.parameters.look));
            hash.update(bytemuck::bytes_of(&self.parameters.look_flags));
        }
        hash.update(source_revision.as_bytes());
        hash.finalize().to_hex().to_string()
    }
}
