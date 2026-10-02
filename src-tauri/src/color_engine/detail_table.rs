//! RapidRAW's Clarity and Texture bands, fitted to Lightroom's by
//! tools/fit_detail.py from tools/adobe_detail.py's measurements; do not
//! edit.
//!
//! Each control is local contrast at several sizes (`*_SIGMAS`, Gaussian
//! sigma in full-resolution pixels), every band with its own amount at
//! slider +50, +100, -50 and -100 (`CLARITY[band]`, `TEXTURE[band]`).

pub const CLARITY_SIGMAS: [f32; 4] = [4.0, 16.0, 64.0, 200.0];
pub const CLARITY: [[f32; 4]; 4] = [
    [0.2457, 0.4904, 0.1085, 0.1814],
    [0.1476, 0.2694, -0.2554, -0.4354],
    [0.0115, 0.0029, -0.1740, -0.2968],
    [0.3211, 0.5961, -0.1758, -0.3063],
];
pub const TEXTURE_SIGMAS: [f32; 3] = [2.0, 6.0, 16.0];
pub const TEXTURE: [[f32; 4]; 3] = [
    [0.1537, 0.2626, -0.1537, -0.2626],
    [0.1182, 0.1665, -0.1182, -0.1665],
    [0.1817, 0.3104, -0.1817, -0.3104],
];
pub const DEHAZE_SIGMAS: [f32; 2] = [16.0, 200.0];
pub const DEHAZE: [[f32; 4]; 2] = [
    [-0.0638, -0.2413, 0.0000, 0.0000],
    [0.2121, 0.5275, 0.0000, 0.0000],
];
