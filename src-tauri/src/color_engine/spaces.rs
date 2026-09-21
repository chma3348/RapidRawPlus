use super::config::{Primaries, Transfer};
use glam::DMat3;

// DWG matrices: Blackmagic specification revision 1.1, August 2021, p.3.
// https://documents.blackmagicdesign.com/InformationNotes/DaVinci_Resolve_17_Wide_Gamut_Intermediate.pdf
pub fn rgb_to_xyz(primaries: Primaries) -> DMat3 {
    let rows = match primaries {
        Primaries::Srgb => [
            [0.4123907992659595, 0.35758433938387796, 0.1804807884018343],
            [0.21263900587151036, 0.7151686787677559, 0.07219231536073371],
            [0.01933081871559185, 0.11919477979462599, 0.9505321522496607],
        ],
        Primaries::DavinciWideGamut => [
            [0.70062239, 0.14877482, 0.10105872],
            [0.27411851, 0.87363190, -0.14775041],
            [-0.09896291, -0.13789533, 1.32591599],
        ],
    };
    DMat3::from_cols_array_2d(&rows).transpose()
}

pub fn conversion(from: Primaries, to: Primaries) -> DMat3 {
    if from == to {
        DMat3::IDENTITY
    } else {
        rgb_to_xyz(to).inverse() * rgb_to_xyz(from)
    }
}

/// Reference math for tests; production pixel operations execute in WGSL.
pub fn decode(value: f64, transfer: Transfer) -> f64 {
    match transfer {
        Transfer::Linear => value,
        Transfer::Srgb if value <= 0.04045 => value / 12.92,
        Transfer::Srgb => ((value + 0.055) / 1.055).powf(2.4),
        Transfer::DavinciIntermediate if value <= 0.02740668 => value / 10.44426855,
        Transfer::DavinciIntermediate => 2.0f64.powf(value / 0.07329248 - 7.0) - 0.0075,
    }
}

pub fn encode_intermediate(value: f64) -> f64 {
    if value <= 0.00262409 {
        value * 10.44426855
    } else {
        ((value + 0.0075).log2() + 7.0) * 0.07329248
    }
}

// Oklab's linear-sRGB -> LMS matrix, from Björn Ottosson's definition.
// https://bottosson.github.io/posts/oklab/
//
// Anchoring on the published *sRGB* matrix rather than the published XYZ one
// is deliberate. The two are not quite consistent: composing Ottosson's
// XYZ -> LMS with a high-precision sRGB -> XYZ disagrees with his sRGB -> LMS
// in the fourth decimal (`spaces_compose_to_the_same_oklab` measures it).
// Anchoring here means the working-space matrix is exactly the product the
// shader used to compute at runtime, so composing it changes no pixel, and
// the output stage's inline copy of these numbers cannot drift from it.
const LMS_FROM_SRGB: [[f64; 3]; 3] = [
    [0.4122214708, 0.5363325363, 0.0514459929],
    [0.2119034982, 0.6806995451, 0.1073969566],
    [0.0883024619, 0.2817188376, 0.6299787005],
];

/// Oklab's XYZ(D65) -> LMS matrix, kept to measure the inconsistency above.
const LMS_FROM_XYZ: [[f64; 3]; 3] = [
    [0.8189330101, 0.3618667424, -0.1288597137],
    [0.0329845436, 0.9293118715, 0.0361456387],
    [0.0482003018, 0.2643662691, 0.6338517070],
];

fn from_rows(rows: &[[f64; 3]; 3]) -> DMat3 {
    DMat3::from_cols_array_2d(rows).transpose()
}

/// Working RGB straight to Oklab's cone space.
///
/// Oklab is defined on XYZ, so this is the same transform as converting to
/// sRGB first and then applying the published matrix — the matrices multiply
/// out. Composing it here says that plainly and drops two mat3 products per
/// pixel.
pub fn lms_from_rgb(primaries: Primaries) -> DMat3 {
    from_rows(&LMS_FROM_SRGB) * conversion(primaries, Primaries::Srgb)
}

pub fn rgb_from_lms(primaries: Primaries) -> DMat3 {
    lms_from_rgb(primaries).inverse()
}

/// The same cone space reached through XYZ. Only used by the contract that
/// records how far the published constants disagree.
pub fn lms_from_rgb_via_xyz(primaries: Primaries) -> DMat3 {
    from_rows(&LMS_FROM_XYZ) * rgb_to_xyz(primaries)
}

/// Oklab's cone space -> Lab. Independent of the RGB basis the cones came
/// from, so the shader carries this half as constants.
const LAB_FROM_LMS: [[f64; 3]; 3] = [
    [0.2104542553, 0.7936177850, -0.0040720468],
    [1.9779984951, -2.4285922050, 0.4505937099],
    [0.0259040371, 0.7827717662, -0.8086757660],
];

/// Reference Oklab math for tests; production pixels go through the shader.
pub fn oklab_from_rgb(primaries: Primaries, rgb: glam::DVec3) -> glam::DVec3 {
    let lms = lms_from_rgb(primaries) * rgb;
    from_rows(&LAB_FROM_LMS) * glam::DVec3::new(lms.x.cbrt(), lms.y.cbrt(), lms.z.cbrt())
}

pub fn rgb_from_oklab(primaries: Primaries, lab: glam::DVec3) -> glam::DVec3 {
    let lms = from_rows(&LAB_FROM_LMS).inverse() * lab;
    rgb_from_lms(primaries) * glam::DVec3::new(lms.x.powi(3), lms.y.powi(3), lms.z.powi(3))
}
