struct Point {
    x: f32,
    y: f32,
    _pad1: f32,
    _pad2: f32,
}

struct HslColor {
    hue: f32,
    saturation: f32,
    luminance: f32,
    _pad: f32,
}

struct ColorGradeSettings {
    hue: f32,
    saturation: f32,
    luminance: f32,
    _pad: f32,
}

struct ColorCalibrationSettings {
    shadows_tint: f32,
    red_hue: f32,
    red_saturation: f32,
    green_hue: f32,
    green_saturation: f32,
    blue_hue: f32,
    blue_saturation: f32,
    _pad1: f32,
}

struct GlobalAdjustments {
    exposure: f32,
    brightness: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    vibrance: f32,
    hue: f32,
    _pad_color1: f32,
    _pad_color2: f32,
    _pad_color3: f32,

    sharpness: f32,
    luma_noise_reduction: f32,
    color_noise_reduction: f32,
    clarity: f32,
    dehaze: f32,
    structure: f32,
    centre: f32,
    vignette_amount: f32,
    vignette_midpoint: f32,
    vignette_roundness: f32,
    vignette_feather: f32,
    grain_amount: f32,
    grain_size: f32,
    grain_roughness: f32,

    chromatic_aberration_red_cyan: f32,
    chromatic_aberration_blue_yellow: f32,
    show_clipping: u32,
    is_raw_image: u32,
    _pad_ca1: f32,

    has_lut: u32,
    lut_intensity: f32,
    tonemapper_mode: u32,
    _pad_lut2: f32,
    _pad_lut3: f32,
    _pad_lut4: f32,
    _pad_lut5: f32,

    _pad_agx1: f32,
    _pad_agx2: f32,
    _pad_agx3: f32,
    agx_pipe_to_rendering_matrix: mat3x3<f32>,
    agx_rendering_to_pipe_matrix: mat3x3<f32>,

    _pad_cg1: f32,
    _pad_cg2: f32,
    _pad_cg3: f32,
    _pad_cg4: f32,
    color_grading_shadows: ColorGradeSettings,
    color_grading_midtones: ColorGradeSettings,
    color_grading_highlights: ColorGradeSettings,
    color_grading_global: ColorGradeSettings,
    color_grading_blending: f32,
    color_grading_balance: f32,
    _pad2: f32,
    _pad3: f32,

    color_calibration: ColorCalibrationSettings,

    hsl: array<HslColor, 8>,
    luma_curve: array<Point, 16>,
    red_curve: array<Point, 16>,
    green_curve: array<Point, 16>,
    blue_curve: array<Point, 16>,
    luma_curve_count: u32,
    red_curve_count: u32,
    green_curve_count: u32,
    blue_curve_count: u32,
    _pad_end1: f32,
    _pad_end2: f32,
    _pad_end3: f32,
    _pad_end4: f32,

    glow_amount: f32,
    halation_amount: f32,
    flare_amount: f32,
    sharpness_threshold: f32,

    // Resolve-style primary wheels: xyz = per-channel, w = master.
    cw_lift: vec4<f32>,
    cw_gamma: vec4<f32>,
    cw_gain: vec4<f32>,
    cw_offset: vec4<f32>,
    contrast_pivot: f32,
    texture_amount: f32,
    _pad_pro1: f32,
    _pad_pro2: f32,
    // Hue/luma curves (x normalized 0..1, y = signed delta -1..1).
    hue_hue_curve: array<Point, 16>,
    hue_sat_curve: array<Point, 16>,
    hue_lum_curve: array<Point, 16>,
    lum_sat_curve: array<Point, 16>,
    hue_hue_count: u32,
    hue_sat_count: u32,
    hue_lum_count: u32,
    lum_sat_count: u32,

    // Rendering process version: 1 = classic per-channel RGB math,
    // 2 = Oklab hue-stable tonal core + gamut safety.
    process_version: u32,
    _pad_pv1: f32,
    _pad_pv2: f32,
    _pad_pv3: f32,

    // Film pack: density-style saturation response + Sat-vs-Sat curve.
    film_saturation: f32,
    _pad_film1: f32,
    _pad_film2: f32,
    _pad_film3: f32,
    sat_sat_curve: array<Point, 16>,
    sat_sat_count: u32,
    _pad_ss1: u32,
    _pad_ss2: u32,
    _pad_ss3: u32,

    // Point Color: [hue_deg, dh_deg, ds, dl] + [range_deg, active, sat, val].
    point_colors: array<vec4<f32>, 4>,
    point_color_meta: array<vec4<f32>, 4>,

    // 0 = display-referred LUT, 1 = F-Log2C film-sim LUT.
    lut_input_space: u32,
    lut_sim_exposure: f32,
    _pad_lspace2: u32,
    _pad_lspace3: u32,
}

struct MaskAdjustments {
    exposure: f32,
    brightness: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    vibrance: f32,

    sharpness: f32,
    luma_noise_reduction: f32,
    color_noise_reduction: f32,
    clarity: f32,
    dehaze: f32,
    structure: f32,

    glow_amount: f32,
    halation_amount: f32,
    flare_amount: f32,
    sharpness_threshold: f32,

    hue: f32,
    _pad_cg1: f32,
    _pad_cg2: f32,
    color_grading_shadows: ColorGradeSettings,
    color_grading_midtones: ColorGradeSettings,
    color_grading_highlights: ColorGradeSettings,
    color_grading_global: ColorGradeSettings,
    color_grading_blending: f32,
    color_grading_balance: f32,
    _pad5: f32,
    _pad6: f32,

    hsl: array<HslColor, 8>,
    luma_curve: array<Point, 16>,
    red_curve: array<Point, 16>,
    green_curve: array<Point, 16>,
    blue_curve: array<Point, 16>,
    luma_curve_count: u32,
    red_curve_count: u32,
    green_curve_count: u32,
    blue_curve_count: u32,
    _pad_end4: f32,
    _pad_end5: f32,
    _pad_end6: f32,
    _pad_end7: f32,

    cw_lift: vec4<f32>,
    cw_gamma: vec4<f32>,
    cw_gain: vec4<f32>,
    cw_offset: vec4<f32>,
    // Pivot is a signed delta for masks (global holds the absolute pivot).
    contrast_pivot: f32,
    texture_amount: f32,
    _pad_pro3: f32,
    _pad_pro4: f32,
}

struct AllAdjustments {
    global: GlobalAdjustments,
    mask_adjustments: array<MaskAdjustments, 32>,
    mask_count: u32,
    tile_offset_x: u32,
    tile_offset_y: u32,
    mask_atlas_cols: u32,
}

struct HslRange {
    center: f32,
    width: f32,
}

const HSL_RANGES: array<HslRange, 8> = array<HslRange, 8>(
    HslRange(358.0, 35.0),  // Red
    HslRange(25.0, 45.0),   // Orange
    HslRange(60.0, 40.0),   // Yellow
    HslRange(115.0, 90.0),  // Green
    HslRange(180.0, 60.0),  // Aqua
    HslRange(225.0, 60.0),  // Blue
    HslRange(280.0, 55.0),  // Purple
    HslRange(330.0, 50.0)   // Magenta
);

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<storage, read> adjustments: AllAdjustments;

@group(0) @binding(3) var mask_textures: texture_2d_array<f32>;

@group(0) @binding(4) var lut_texture: texture_3d<f32>;
@group(0) @binding(5) var lut_sampler: sampler;

@group(0) @binding(6) var sharpness_blur_texture: texture_2d<f32>;
@group(0) @binding(7) var tonal_blur_texture: texture_2d<f32>;
@group(0) @binding(8) var clarity_blur_texture: texture_2d<f32>;
@group(0) @binding(9) var structure_blur_texture: texture_2d<f32>;

@group(0) @binding(10) var flare_texture: texture_2d<f32>;
@group(0) @binding(11) var flare_sampler: sampler;

const LUMA_COEFF = vec3<f32>(0.2126, 0.7152, 0.0722);

fn get_luma(c: vec3<f32>) -> f32 {
    return dot(c, LUMA_COEFF);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = vec3<f32>(0.04045);
    let a = vec3<f32>(0.055);
    let higher = pow((c + a) / (1.0 + a), vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, c <= cutoff);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let c_clamped = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let cutoff = vec3<f32>(0.0031308);
    let a = vec3<f32>(0.055);
    let higher = (1.0 + a) * pow(c_clamped, vec3<f32>(1.0 / 2.4)) - a;
    let lower = c_clamped * 12.92;
    return select(higher, lower, c_clamped <= cutoff);
}

fn linear_to_srgb_extended(c: vec3<f32>) -> vec3<f32> {
    let safe_c = max(c, vec3<f32>(0.0));
    let cutoff = vec3<f32>(0.0031308);
    let a = vec3<f32>(0.055);
    let higher = (1.0 + a) * pow(safe_c, vec3<f32>(1.0 / 2.4)) - a;
    let lower = safe_c * 12.92;
    return select(higher, lower, safe_c <= cutoff);
}

fn rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
    let c_max = max(c.r, max(c.g, c.b));
    let c_min = min(c.r, min(c.g, c.b));
    let delta = c_max - c_min;
    var h: f32 = 0.0;
    if (delta > 0.0) {
        if (c_max == c.r) { h = 60.0 * (((c.g - c.b) / delta) % 6.0); }
        else if (c_max == c.g) { h = 60.0 * (((c.b - c.r) / delta) + 2.0); }
        else { h = 60.0 * (((c.r - c.g) / delta) + 4.0); }
    }
    if (h < 0.0) { h += 360.0; }
    let s = select(0.0, delta / c_max, c_max > 0.0);
    return vec3<f32>(h, s, c_max);
}

fn hsv_to_rgb(c: vec3<f32>) -> vec3<f32> {
    let h = c.x; let s = c.y; let v = c.z;
    let C = v * s;
    let X = C * (1.0 - abs((h / 60.0) % 2.0 - 1.0));
    let m = v - C;
    var rgb_prime: vec3<f32>;
    if (h < 60.0) { rgb_prime = vec3<f32>(C, X, 0.0); }
    else if (h < 120.0) { rgb_prime = vec3<f32>(X, C, 0.0); }
    else if (h < 180.0) { rgb_prime = vec3<f32>(0.0, C, X); }
    else if (h < 240.0) { rgb_prime = vec3<f32>(0.0, X, C); }
    else if (h < 300.0) { rgb_prime = vec3<f32>(X, 0.0, C); }
    else { rgb_prime = vec3<f32>(C, 0.0, X); }
    return rgb_prime + vec3<f32>(m, m, m);
}

fn apply_hue_shift(color: vec3<f32>, shift_degrees: f32) -> vec3<f32> {
    if (abs(shift_degrees) < 0.01) {
        return color;
    }
    let srgb_color = linear_to_srgb_extended(color);
    let hsv = rgb_to_hsv(srgb_color);
    var shifted_h = hsv.x + shift_degrees;
    shifted_h = (shifted_h + 360.0) % 360.0;
    let shifted_srgb = hsv_to_rgb(vec3<f32>(shifted_h, hsv.y, hsv.z));
    return srgb_to_linear(shifted_srgb);
}

fn get_raw_hsl_influence(hue: f32, center: f32, width: f32) -> f32 {
    let dist = min(abs(hue - center), 360.0 - abs(hue - center));
    const sharpness = 1.5;
    let falloff = dist / (width * 0.5);
    return exp(-sharpness * falloff * falloff);
}

fn hash(p: vec2<f32>) -> f32 {
    var p3  = fract(vec3<f32>(p.xyx) * .1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn gradient_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);

    let ga = vec2<f32>(hash(i + vec2(0.0, 0.0)), hash(i + vec2(0.0, 0.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;
    let gb = vec2<f32>(hash(i + vec2(1.0, 0.0)), hash(i + vec2(1.0, 0.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;
    let gc = vec2<f32>(hash(i + vec2(0.0, 1.0)), hash(i + vec2(0.0, 1.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;
    let gd = vec2<f32>(hash(i + vec2(1.0, 1.0)), hash(i + vec2(1.0, 1.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;

    let dot_00 = dot(ga, f - vec2(0.0, 0.0));
    let dot_10 = dot(gb, f - vec2(1.0, 0.0));
    let dot_01 = dot(gc, f - vec2(0.0, 1.0));
    let dot_11 = dot(gd, f - vec2(1.0, 1.0));

    let bottom_interp = mix(dot_00, dot_10, u.x);
    let top_interp = mix(dot_01, dot_11, u.x);

    return mix(bottom_interp, top_interp, u.y);
}

fn dither(coords: vec2<u32>) -> f32 {
    let p = vec2<f32>(coords);
    return fract(sin(dot(p, vec2<f32>(12.9898, 78.233))) * 43758.5453) - 0.5;
}

fn interpolate_cubic_hermite(x: f32, p1: Point, p2: Point, m1: f32, m2: f32) -> f32 {
    let dx = p2.x - p1.x;
    if (dx <= 0.0) { return p1.y; }
    let t = (x - p1.x) / dx;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    return h00 * p1.y + h10 * m1 * dx + h01 * p2.y + h11 * m2 * dx;
}

fn apply_curve(val: f32, points: array<Point, 16>, count: u32) -> f32 {
    if (count < 2u) { return val; }
    var local_points = points;
    let x = val * 255.0;
    if (x <= local_points[0].x) { return local_points[0].y / 255.0; }
    if (x >= local_points[count - 1u].x) { return local_points[count - 1u].y / 255.0; }
    for (var i = 0u; i < 15u; i = i + 1u) {
        if (i >= count - 1u) { break; }
        let p1 = local_points[i];
        let p2 = local_points[i + 1u];
        if (x <= p2.x) {
            let p0 = local_points[max(0u, i - 1u)];
            let p3 = local_points[min(count - 1u, i + 2u)];
            let delta_before = (p1.y - p0.y) / max(0.001, p1.x - p0.x);
            let delta_current = (p2.y - p1.y) / max(0.001, p2.x - p1.x);
            let delta_after = (p3.y - p2.y) / max(0.001, p3.x - p2.x);
            var tangent_at_p1: f32;
            var tangent_at_p2: f32;
            if (i == 0u) { tangent_at_p1 = delta_current; } else {
                if (delta_before * delta_current <= 0.0) { tangent_at_p1 = 0.0; } else { tangent_at_p1 = (delta_before + delta_current) / 2.0; }
            }
            if (i + 1u == count - 1u) { tangent_at_p2 = delta_current; } else {
                if (delta_current * delta_after <= 0.0) { tangent_at_p2 = 0.0; } else { tangent_at_p2 = (delta_current + delta_after) / 2.0; }
            }
            if (delta_current != 0.0) {
                let alpha = tangent_at_p1 / delta_current;
                let beta = tangent_at_p2 / delta_current;
                if (alpha * alpha + beta * beta > 9.0) {
                    let tau = 3.0 / sqrt(alpha * alpha + beta * beta);
                    tangent_at_p1 = tangent_at_p1 * tau;
                    tangent_at_p2 = tangent_at_p2 * tau;
                }
            }
            let result_y = interpolate_cubic_hermite(x, p1, p2, tangent_at_p1, tangent_at_p2);
            return clamp(result_y / 255.0, 0.0, 1.0);
        }
    }
    return local_points[count - 1u].y / 255.0;
}

// ---------- v2 rendering core: perceptual color math ----------

fn cbrt_safe(x: f32) -> f32 {
    return sign(x) * pow(abs(x), 1.0 / 3.0);
}

// Linear sRGB -> Oklab (Björn Ottosson's matrices). Hue-linear: moving L
// leaves (a, b) — and therefore hue — untouched.
fn linear_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let l = 0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b;
    let m = 0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b;
    let s = 0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b;
    let l_ = cbrt_safe(l);
    let m_ = cbrt_safe(m);
    let s_ = cbrt_safe(s);
    return vec3<f32>(
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    );
}

fn oklab_to_linear(c: vec3<f32>) -> vec3<f32> {
    let l_ = c.x + 0.3963377774 * c.y + 0.2158037573 * c.z;
    let m_ = c.x - 0.1055613458 * c.y - 0.0638541728 * c.z;
    let s_ = c.x - 0.0894841775 * c.y - 1.2914855480 * c.z;
    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;
    return vec3<f32>(
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    );
}

// Soft gamut safety: pulls negative (out-of-gamut) channels back toward
// neutral just enough, preserving hue direction — removes neon-edge and
// blocked-color artifacts after strong grades.
fn compress_gamut_soft(rgb: vec3<f32>) -> vec3<f32> {
    let l = max(get_luma(rgb), 0.0001);
    let min_c = min(rgb.r, min(rgb.g, rgb.b));
    if (min_c >= 0.0) { return rgb; }
    let t = min_c / (min_c - l);
    return mix(rgb, vec3<f32>(l), clamp(t, 0.0, 1.0));
}

fn hable(x: vec3<f32>) -> vec3<f32> {
    let a = 0.15; let b = 0.50; let c = 0.10; let d = 0.20; let e = 0.02; let f = 0.30;
    return ((x * (a * x + vec3<f32>(c * b)) + vec3<f32>(d * e))
        / (x * (a * x + vec3<f32>(b)) + vec3<f32>(d * f))) - vec3<f32>(e / f);
}

fn hable1(x: f32) -> f32 {
    let a = 0.15; let b = 0.50; let c = 0.10; let d = 0.20; let e = 0.02; let f = 0.30;
    return ((x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * f)) - e / f;
}

// Filmic display transform: luminance-driven shoulder keeps chroma through
// the mids, blending to per-channel compression near the top so highlights
// take the classic graceful path to white instead of clipping.
fn tonemap_filmic(rgb_in: vec3<f32>) -> vec3<f32> {
    let rgb = max(rgb_in, vec3<f32>(0.0));
    let white = hable1(4.0);
    let l = get_luma(rgb);
    let l_tm = hable1(l * 1.6) / white;
    let chroma_preserving = rgb * (l_tm / max(l, 0.0001));
    let per_channel = hable(rgb * 1.6) / vec3<f32>(white);
    let w = smoothstep(0.5, 1.0, l_tm);
    return clamp(mix(chroma_preserving, per_channel, w), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Resolve-style primary corrector. Neutral at all zeros. Operates in
// perceptual (gamma 2.2) space so the wheels feel like a grading panel:
// offset shifts everything, gain pivots at black, lift pivots at white,
// gamma bends midtones.
fn apply_color_wheels(
    color: vec3<f32>,
    lift: vec4<f32>,
    gamma: vec4<f32>,
    gain: vec4<f32>,
    offset: vec4<f32>
) -> vec3<f32> {
    let wheel_activity = dot(abs(lift), vec4<f32>(1.0)) + dot(abs(gamma), vec4<f32>(1.0))
        + dot(abs(gain), vec4<f32>(1.0)) + dot(abs(offset), vec4<f32>(1.0));
    if (wheel_activity < 0.0001) { return color; }

    let g_exp = 1.0 / 2.2;
    var v = pow(max(color, vec3<f32>(0.0)), vec3<f32>(g_exp));

    let o = (offset.xyz + vec3<f32>(offset.w)) * 0.3;
    let gn = vec3<f32>(1.0) + (gain.xyz + vec3<f32>(gain.w));
    let l = (lift.xyz + vec3<f32>(lift.w)) * 0.3;
    let gm = gamma.xyz + vec3<f32>(gamma.w);

    v = v + o;
    v = v * max(gn, vec3<f32>(0.0));
    v = v + l * (vec3<f32>(1.0) - v);
    let exponent = vec3<f32>(1.0) / max(vec3<f32>(1.0) + gm, vec3<f32>(0.05));
    v = pow(max(v, vec3<f32>(0.0)), exponent);

    return pow(v, vec3<f32>(2.2));
}

// Evaluates a delta curve (x 0..1 -> y -1..1) with smoothstep-blended
// segments. Outside the outermost points the edge value holds. Wrap for
// hue-domain curves is handled by phantom points baked on the CPU side.
fn eval_delta_curve(points: array<Point, 16>, count: u32, x: f32) -> f32 {
    if (count == 0u) { return 0.0; }
    if (count == 1u) { return points[0].y; }
    var pts = points;
    if (x <= pts[0].x) { return pts[0].y; }
    if (x >= pts[count - 1u].x) { return pts[count - 1u].y; }
    for (var i = 1u; i < count; i = i + 1u) {
        if (x <= pts[i].x) {
            let span = max(pts[i].x - pts[i - 1u].x, 0.0001);
            let t = (x - pts[i - 1u].x) / span;
            let ts = t * t * (3.0 - 2.0 * t);
            return mix(pts[i - 1u].y, pts[i].y, ts);
        }
    }
    return pts[count - 1u].y;
}

// Resolve-style custom color curves: Hue vs Hue / Sat / Luma, Luma vs Sat.
fn apply_hue_curves(color: vec3<f32>) -> vec3<f32> {
    let g = adjustments.global;
    if (g.hue_hue_count == 0u && g.hue_sat_count == 0u
        && g.hue_lum_count == 0u && g.lum_sat_count == 0u
        && g.sat_sat_count == 0u) {
        return color;
    }

    let perceptual = pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    var hsv = rgb_to_hsv(clamp(perceptual, vec3<f32>(0.0), vec3<f32>(1.0)));

    // rgb_to_hsv returns hue in DEGREES (0..360) — normalize for the
    // curve x-axis, and do hue arithmetic in degrees.
    let h_norm = hsv.x / 360.0;

    if (g.hue_hue_count > 0u) {
        // ±60° of shift at full deflection.
        let dh = eval_delta_curve(g.hue_hue_curve, g.hue_hue_count, h_norm) * 60.0;
        hsv.x = (hsv.x + dh + 360.0) % 360.0;
    }
    if (g.hue_sat_count > 0u) {
        let ds = eval_delta_curve(g.hue_sat_curve, g.hue_sat_count, h_norm);
        hsv.y = clamp(hsv.y * (1.0 + ds), 0.0, 1.0);
    }
    if (g.hue_lum_count > 0u) {
        let dl = eval_delta_curve(g.hue_lum_curve, g.hue_lum_count, h_norm) * 0.6;
        hsv.z = clamp(hsv.z * (1.0 + dl), 0.0, 1.0);
    }
    if (g.lum_sat_count > 0u) {
        let dls = eval_delta_curve(g.lum_sat_curve, g.lum_sat_count, hsv.z);
        hsv.y = clamp(hsv.y * (1.0 + dls), 0.0, 1.0);
    }
    if (g.sat_sat_count > 0u) {
        // Sat vs Sat: x = the pixel's own saturation — compress loud
        // colors and lift muted ones on one curve.
        let dss = eval_delta_curve(g.sat_sat_curve, g.sat_sat_count, hsv.y);
        hsv.y = clamp(hsv.y * (1.0 + dss), 0.0, 1.0);
    }

    let adjusted = hsv_to_rgb(hsv);
    // Weight by saturation so near-neutrals aren't destabilized by hue ops.
    let sat_guard = smoothstep(0.02, 0.12, hsv.y);
    var guard = sat_guard;
    if (g.lum_sat_count > 0u || g.hue_lum_count > 0u) {
        guard = max(guard, 0.6);
    }
    let result = mix(perceptual, adjusted, guard);
    return pow(max(result, vec3<f32>(0.0)), vec3<f32>(2.2));
}

// Point Color: precise H/S/L edits in a narrow window around each picked
// hue — the "edit exactly THIS color" tool, leaving all other colors and
// the preset mixer bands untouched.
fn apply_point_colors(
    color: vec3<f32>,
    blurred_input_space: vec3<f32>,
    is_raw: u32,
) -> vec3<f32> {
    let g = adjustments.global;
    var any_active = false;
    for (var i = 0; i < 4; i = i + 1) {
        if (g.point_color_meta[i].y > 0.5) { any_active = true; }
    }
    if (!any_active) {
        return color;
    }

    // HDR-safe: normalize by the max component so nothing is CLAMPED away
    // before tone mapping — a lossy round-trip here reads as a global
    // white-balance/exposure shift the moment any chip exists.
    let safe = max(color, vec3<f32>(0.0));
    let norm = max(max(safe.r, max(safe.g, safe.b)), 1.0);
    let perceptual = pow(safe / norm, vec3<f32>(1.0 / 2.2));
    var hsv = rgb_to_hsv(perceptual);

    // WINDOW MEMBERSHIP comes from the smoothed neighborhood (3.5px blur),
    // not the pixel itself: per-pixel noise — which dominates saturation on
    // pale washed areas — otherwise flickers pixels in and out of the
    // window, and a luminance shift then multiplies neighbors differently
    // (reads as instant pixelation). The EDIT still applies to the pixel's
    // own value; only the gate is smooth.
    var gate_linear: vec3<f32>;
    if (is_raw == 1u) {
        gate_linear = max(blurred_input_space, vec3<f32>(0.0));
    } else {
        gate_linear = srgb_to_linear(max(blurred_input_space, vec3<f32>(0.0)));
    }
    let gate_norm = max(max(gate_linear.r, max(gate_linear.g, gate_linear.b)), 1.0);
    let gate_hsv = rgb_to_hsv(pow(gate_linear / gate_norm, vec3<f32>(1.0 / 2.2)));

    var applied = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        if (g.point_color_meta[i].y <= 0.5) { continue; }
        let chip = g.point_colors[i];
        let range = max(g.point_color_meta[i].x, 4.0);
        var dh = abs(gate_hsv.x - chip.x);
        dh = min(dh, 360.0 - dh);
        // Full-color window: hue proximity AND sat/val proximity to the
        // SAMPLED color — a pick means "this color", not "every pixel with
        // this hue" (which is the entire frame on a wash-tinted photo).
        let w_hue = 1.0 - smoothstep(range * 0.4, range, dh);
        let w_sat = 1.0 - smoothstep(0.18, 0.42, abs(gate_hsv.y - g.point_color_meta[i].z));
        let w_val = 1.0 - smoothstep(0.22, 0.48, abs(gate_hsv.z - g.point_color_meta[i].w));
        // Near-neutral CHIPS must not gate on hue: hue is numerically
        // meaningless on pale pixels, so JPEG blocks swing wildly in hue
        // while looking identical — a hue gate then switches whole blocks
        // in and out of the window and luminance edits checkerboard them.
        // A pale pick selects by paleness + brightness alone; the neutral
        // guard (which exists to PROTECT neutrals from hue ops) also
        // yields, since a neutral chip is aimed at neutrals on purpose.
        let hue_conf = smoothstep(0.08, 0.25, g.point_color_meta[i].z);
        let w = mix(1.0, w_hue, hue_conf)
            * w_sat
            * w_val
            * mix(1.0, smoothstep(0.015, 0.06, gate_hsv.y), hue_conf);
        if (w <= 0.001) { continue; }
        applied = max(applied, w);
        hsv.x = (hsv.x + chip.y * w + 360.0) % 360.0;
        hsv.y = clamp(hsv.y * (1.0 + chip.z * w), 0.0, 1.0);
        hsv.z = clamp(hsv.z * (1.0 + chip.w * w), 0.0, 1.0);
    }
    if (applied <= 0.001) {
        return color;
    }

    let adjusted = pow(max(hsv_to_rgb(hsv), vec3<f32>(0.0)), vec3<f32>(2.2)) * norm;
    return mix(color, adjusted, smoothstep(0.0, 0.05, applied));
}

// v2 tonal core: all tone moves ride Oklab lightness, so contrast and
// shadow/black/white recovery cannot skew hue — the property that makes
// graded footage look "clean" instead of "crunchy".
fn apply_tonal_adjustments_v2(
    color: vec3<f32>,
    neighborhood_input_space: vec3<f32>,
    is_raw: u32,
    con: f32,
    sh: f32,
    wh: f32,
    bl: f32,
    pivot: f32
) -> vec3<f32> {
    if (con == 0.0 && sh == 0.0 && wh == 0.0 && bl == 0.0) { return color; }

    var lab = linear_to_oklab(max(color, vec3<f32>(0.0)));
    var l_ok = lab.x;
    let l_clamped = clamp(l_ok, 0.0, 1.0);

    // Local tone mapping driver: the NEIGHBORHOOD's lightness decides how
    // much a zone moves, so texture inside a lifted region rides along
    // and detail survives (the Lightroom/Resolve trick). A dash of the
    // pixel's own lightness guards halos at strong zone boundaries.
    var neighborhood_linear: vec3<f32>;
    if (is_raw == 1u) {
        neighborhood_linear = neighborhood_input_space;
    } else {
        neighborhood_linear = srgb_to_linear(neighborhood_input_space);
    }
    let l_base = clamp(linear_to_oklab(max(neighborhood_linear, vec3<f32>(0.0))).x, 0.0, 1.0);
    let driver = mix(l_clamped, l_base, 0.75);

    // Whites: true white-point control — the gain fades in above the
    // midtones so the lower half of the image stays pinned (the old
    // uniform expansion was just a second exposure slider).
    var l_new = l_ok;
    if (wh != 0.0) {
        let wzone = smoothstep(0.35, 0.95, driver);
        if (wh > 0.0) {
            l_new = l_new * (1.0 + wh * 0.38 * wzone);
        } else {
            l_new = l_new * (1.0 + wh * 0.24 * wzone);
        }
    }

    // Shadows: exponential lift, zone-weighted by the driver. The lift
    // peaks in the mid-shadows: deep blacks take a reduced share so the
    // frame keeps its footing (a floor launched upward reads as mist),
    // and the zone fades out before the midtones.
    if (sh != 0.0) {
        // Neighborhood-heavy driver: adjacent texture shares one lift
        // (detail survives). RAW scene-linear and JPEG display-linear
        // place the same perceptual zone at different L — calibrate the
        // fade per domain (one wide fade for both was how shadows
        // grabbed JPEG midtones).
        let sh_fade = select(0.46, 0.62, is_raw == 1u);
        let zone = 1.0 - smoothstep(0.04, sh_fade, driver);
        if (sh > 0.0) {
            let ground = 0.45 + 0.55 * smoothstep(0.0, 0.30, driver);
            let stops = sh * 1.35 * zone * ground;
            l_new = l_new * pow(2.0, stops);
            // Lifted shadows read slightly desaturated at constant (a,b);
            // nudge chroma with the lift like film does.
            let chroma_boost = 1.0 + stops * 0.14;
            lab.y *= chroma_boost;
            lab.z *= chroma_boost;
        } else {
            l_new = l_new * pow(2.0, sh * 1.1 * zone);
        }
    }

    // Blacks: floor control with real authority, tightly range-limited.
    if (bl != 0.0) {
        let bzone = 1.0 - smoothstep(0.0, 0.27, driver);
        if (bl > 0.0) {
            l_new = l_new + bl * 0.10 * bzone;
        } else {
            l_new = max(l_new + bl * 0.11 * bzone, 0.0);
        }
    }

    // Contrast: pivoted S-curve directly on perceptual lightness.
    if (con != 0.0) {
        let p = clamp(pivot, 0.05, 0.95);
        let strength = pow(2.0, con * 1.1);
        let x = clamp(l_new, 0.0, 1.0);
        var curved: f32;
        if (x < p) {
            curved = p * pow(x / p, strength);
        } else {
            curved = 1.0 - (1.0 - p) * pow((1.0 - x) / (1.0 - p), strength);
        }
        // Values above 1 (RAW headroom) keep their offset.
        l_new = curved + max(l_new - 1.0, 0.0);
    }

    lab.x = l_new;
    var out = oklab_to_linear(lab);
    // Oklab round trips can produce tiny negatives on saturated colors.
    out = compress_gamut_soft(out);
    return out;
}

fn apply_tonal_adjustments(
    color: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    neighborhood_input_space: vec3<f32>,
    is_raw: u32,
    con: f32,
    sh: f32,
    wh: f32,
    bl: f32,
    pivot: f32
) -> vec3<f32> {
    if (adjustments.global.process_version >= 2u) {
        return apply_tonal_adjustments_v2(color, neighborhood_input_space, is_raw, con, sh, wh, bl, pivot);
    }
    var rgb = color;

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    if (wh != 0.0) {
        let white_level = 1.0 - wh * 0.25;
        let w_mult = 1.0 / max(white_level, 0.01);
        rgb *= w_mult;
        blurred_linear *= w_mult;
    }

    let pixel_luma = get_luma(max(rgb, vec3<f32>(0.0)));
    let blurred_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));

    let safe_pixel_luma = max(pixel_luma, 0.0001);
    let safe_blurred_luma = max(blurred_luma, 0.0001);

    if (sh != 0.0 || bl != 0.0) {
        let t_pixel = pow(safe_pixel_luma, 0.4545);
        let t_blurred = pow(safe_blurred_luma, 0.4545);

        let shadow_lift = sh * t_pixel * pow(max(1.0 - t_pixel, 0.0), 4.5);
        let black_lift = bl * t_pixel * pow(max(1.0 - t_pixel, 0.0), 12.0);
        let lift_amount = max(shadow_lift + black_lift, 0.0);

        let t_pixel_curved = max(t_pixel + shadow_lift + black_lift, 0.0);

        let shadow_pivot = 0.2;
        let stretch_factor = 1.0 + (lift_amount * 1.3);
        let contrasted_t = shadow_pivot + (t_pixel_curved - shadow_pivot) * stretch_factor;

        let final_t = max(mix(t_pixel_curved, contrasted_t, 0.85), 0.0);
        let curved_luma = pow(final_t, 2.2);

        let luma_ratio = curved_luma / safe_pixel_luma;
        rgb *= luma_ratio;

        let detail = t_pixel / max(t_blurred, 0.0001);
        let safe_detail = clamp(detail, 0.8, 1.25);

        let noise_protection = smoothstep(0.0, 0.1, t_blurred);

        let detail_amp = 1.0 + (lift_amount * 1.2 * noise_protection);

        let enhanced_detail = pow(safe_detail, detail_amp);
        let detail_correction = enhanced_detail / safe_detail;

        let linear_correction = pow(detail_correction, 2.2);
        rgb *= linear_correction;

        if (luma_ratio > 1.0) {
            let recovered_luma = get_luma(rgb);
            let boost_amount = clamp((luma_ratio - 1.0) * 0.15, 0.0, 0.4);
            rgb = mix(rgb, vec3<f32>(recovered_luma), boost_amount);
        }
    }

    if (con != 0.0) {
        let safe_rgb = max(rgb, vec3<f32>(0.0));
        let g = 2.2;
        let perceptual = pow(safe_rgb, vec3<f32>(1.0 / g));
        let clamped_perceptual = clamp(perceptual, vec3<f32>(0.0), vec3<f32>(1.0));
        let strength = pow(2.0, con * 1.25);
        // S-curve around a movable pivot (Resolve-style): p=0.5 reproduces
        // the classic symmetric curve.
        let p = clamp(pivot, 0.05, 0.95);
        let condition = clamped_perceptual < vec3<f32>(p);
        let high_part = vec3<f32>(1.0) - (1.0 - p) * pow((vec3<f32>(1.0) - clamped_perceptual) / (1.0 - p), vec3<f32>(strength));
        let low_part = p * pow(clamped_perceptual / p, vec3<f32>(strength));
        let curved_perceptual = select(high_part, low_part, condition);
        let contrast_adjusted_rgb = pow(curved_perceptual, vec3<f32>(g));
        let mix_factor = smoothstep(vec3<f32>(1.0), vec3<f32>(1.01), safe_rgb);
        rgb = mix(contrast_adjusted_rgb, rgb, mix_factor);
    }
    return rgb;
}

fn apply_highlights_adjustment(
    color_in: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    neighborhood_input_space: vec3<f32>,
    is_raw: u32,
    highlights_adj: f32
) -> vec3<f32> {
    if (highlights_adj == 0.0) { return color_in; }
    if (adjustments.global.process_version >= 2u) {
        var lab = linear_to_oklab(max(color_in, vec3<f32>(0.0)));
        let t = clamp(lab.x, 0.0, 1.0);
        // Wide, smooth zone weight: the blotch-maker was a steep mask edge
        // meeting clipped, chroma-less pixels.
        // Zone weight from the NEIGHBORHOOD, so compression is locally
        // uniform and texture inside bright regions survives.
        var nb_linear: vec3<f32>;
        if (is_raw == 1u) {
            nb_linear = neighborhood_input_space;
        } else {
            nb_linear = srgb_to_linear(neighborhood_input_space);
        }
        let l_base_h = clamp(linear_to_oklab(max(nb_linear, vec3<f32>(0.0))).x, 0.0, 1.2);
        let driver_h = mix(t, l_base_h, 0.75);
        let mask = smoothstep(0.45, 1.0, driver_h);
        if (highlights_adj < 0.0) {
            // Monotone shoulder: L above the knee is remapped through a
            // rational compressor with unit slope at the knee. Tone order
            // can never invert (the old mask-scaled gain pulled brighter
            // pixels BELOW darker ones at strong settings) and midtones
            // sit below the knee, untouched.
            let amt = min(-highlights_adj, 1.0);
            let knee = select(0.56, 0.70, is_raw == 1u);
            let span = 0.30;
            if (lab.x > knee) {
                let x = (lab.x - knee) / span;
                let c = 0.60 * amt;
                let shoulder = knee + span * (x / (1.0 + c * x * (2.0 + x) / (1.0 + x)));
                // The compressor's top-end slope flattens texture inside
                // bright regions; restore part of the pixel-vs-neighborhood
                // detail so recovered skies keep their clouds.
                let restore_zone = smoothstep(0.68, 0.92, l_base_h);
                lab.x = shoulder + (t - min(l_base_h, 1.0)) * 0.5 * amt * restore_zone;
            }

            // Clipped pixels carry no color of their own — when pulled
            // down they'd surface as gray/white blotches. Reconstruct
            // their chroma from the local neighborhood instead (the
            // blurred image is already on hand), scaled by how clipped
            // the pixel actually is.
            var neighborhood_linear: vec3<f32>;
            if (is_raw == 1u) {
                neighborhood_linear = neighborhood_input_space;
            } else {
                neighborhood_linear = srgb_to_linear(neighborhood_input_space);
            }
            let min_ch = min(color_in.r, min(color_in.g, color_in.b));
            let clipped = smoothstep(0.82, 1.0, min_ch);
            if (clipped > 0.001) {
                let blab = linear_to_oklab(max(neighborhood_linear, vec3<f32>(0.0)));
                // The neighborhood blur includes the blown region itself,
                // diluting its chroma — amplify to compensate. Near truly
                // neutral surroundings the absolute chroma stays tiny, so
                // this cannot invent color that is not there.
                let inherit = clipped * min(-highlights_adj, 1.0);
                lab.y = mix(lab.y, blab.y * 3.2, inherit);
                lab.z = mix(lab.z, blab.z * 3.2, inherit);
            }
        } else {
            // Brighten with a clip-resistant approach: the push fades as
            // L nears white, so +100 glows instead of blowing out.
            lab.x = lab.x + highlights_adj * 0.7 * mask * max(1.02 - t, 0.0);
        }
        return compress_gamut_soft(oklab_to_linear(lab));
    }

    let pixel_luma = get_luma(max(color_in, vec3<f32>(0.0)));
    let safe_pixel_luma = max(pixel_luma, 0.0001);

    let pixel_mask_input = tanh(safe_pixel_luma * 1.5);
    let highlight_mask = smoothstep(0.3, 0.95, pixel_mask_input);

    if (highlight_mask < 0.001) {
        return color_in;
    }

    let luma = pixel_luma;
    var final_adjusted_color: vec3<f32>;

    if (highlights_adj < 0.0) {
        var new_luma: f32;
        if (luma <= 1.0) {
            let gamma = 1.0 - highlights_adj * 1.75;
            new_luma = pow(luma, gamma);
        } else {
            let luma_excess = luma - 1.0;
            let compression_strength = -highlights_adj * 6.0;
            let compressed_excess = luma_excess / (1.0 + luma_excess * compression_strength);
            new_luma = 1.0 + compressed_excess;
        }
        let tonally_adjusted_color = color_in * (new_luma / max(luma, 0.0001));
        let desaturation_amount = smoothstep(1.0, 10.0, luma);
        let white_point = vec3<f32>(new_luma);
        final_adjusted_color = mix(tonally_adjusted_color, white_point, desaturation_amount);
    } else {
        let adjustment = highlights_adj * 1.75;
        let factor = pow(2.0, adjustment);
        final_adjusted_color = color_in * factor;
    }

    return mix(color_in, final_adjusted_color, highlight_mask);
}

fn apply_linear_exposure(color_in: vec3<f32>, exposure_adj: f32) -> vec3<f32> {
    if (exposure_adj == 0.0) {
        return color_in;
    }
    return color_in * pow(2.0, exposure_adj);
}

fn apply_filmic_exposure(color_in: vec3<f32>, brightness_adj: f32) -> vec3<f32> {
    if (brightness_adj == 0.0) {
        return color_in;
    }
    const RATIONAL_CURVE_MIX: f32 = 0.95;
    const MIDTONE_STRENGTH: f32 = 1.2;
    const TOP_ANCHOR: f32 = 1.06;
    let original_luma = get_luma(color_in);
    if (abs(original_luma) < 0.00001) {
        return color_in;
    }
    let direct_adj = brightness_adj * (1.0 - RATIONAL_CURVE_MIX);
    let rational_adj = brightness_adj * RATIONAL_CURVE_MIX;
    let scale = pow(2.0, direct_adj);
    let k = pow(2.0, -rational_adj * MIDTONE_STRENGTH);
    let luma_abs = abs(original_luma);
    let luma_floor = floor(luma_abs / TOP_ANCHOR) * TOP_ANCHOR;
    let luma_norm = (luma_abs - luma_floor) / TOP_ANCHOR;
    let shaped_norm = luma_norm / (luma_norm + (1.0 - luma_norm) * k);
    let shaped_luma_abs = luma_floor + (shaped_norm * TOP_ANCHOR);
    let new_luma = sign(original_luma) * shaped_luma_abs * scale;
    let chroma = color_in - vec3<f32>(original_luma);
    let total_luma_scale = new_luma / original_luma;
    let luma_weight = clamp(new_luma, 0.0, 2.0) * 0.5;
    let dynamic_exp = mix(0.95, 0.65, luma_weight);
    let base_chroma_scale = pow(total_luma_scale, dynamic_exp);
    let highlight_rolloff = 1.0 / (1.0 + max(0.0, new_luma - 0.9) * 2.0);
    let chroma_scale = base_chroma_scale * highlight_rolloff;
    return vec3<f32>(new_luma) + chroma * chroma_scale;
}

fn apply_color_calibration(color: vec3<f32>, cal: ColorCalibrationSettings) -> vec3<f32> {
    let h_r = cal.red_hue;
    let h_g = cal.green_hue;
    let h_b = cal.blue_hue;
    let r_prime = vec3<f32>(1.0 - abs(h_r), max(0.0, h_r), max(0.0, -h_r));
    let g_prime = vec3<f32>(max(0.0, -h_g), 1.0 - abs(h_g), max(0.0, h_g));
    let b_prime = vec3<f32>(max(0.0, h_b), max(0.0, -h_b), 1.0 - abs(h_b));
    let hue_matrix = mat3x3<f32>(r_prime, g_prime, b_prime);
    var c = hue_matrix * color;

    let luma = get_luma(max(vec3(0.0), c));
    let desaturated_color = vec3<f32>(luma);
    let sat_vector = c - desaturated_color;

    let color_sum = c.r + c.g + c.b;
    var masks = vec3<f32>(0.0);
    if (color_sum > 0.001) {
        masks = c / color_sum;
    }

    let total_sat_adjustment =
        masks.r * cal.red_saturation +
        masks.g * cal.green_saturation +
        masks.b * cal.blue_saturation;

    c += sat_vector * total_sat_adjustment;

    let st = cal.shadows_tint;
    if (abs(st) > 0.001) {
        let shadow_luma = get_luma(max(vec3(0.0), c));
        let mask = 1.0 - smoothstep(0.0, 0.3, shadow_luma);
        let tint_mult = vec3<f32>(1.0 + st * 0.25, 1.0 - st * 0.25, 1.0 + st * 0.25);
        c = mix(c, c * tint_mult, mask);
    }

    return c;
}

fn apply_white_balance(color: vec3<f32>, temp: f32, tnt: f32) -> vec3<f32> {
    var rgb = color;
    let temp_kelvin_mult = vec3<f32>(1.0 + temp * 0.2, 1.0 + temp * 0.05, 1.0 - temp * 0.2);
    let tint_mult = vec3<f32>(1.0 + tnt * 0.25, 1.0 - tnt * 0.25, 1.0 + tnt * 0.25);
    rgb *= temp_kelvin_mult * tint_mult;
    return rgb;
}

fn apply_creative_color(color: vec3<f32>, sat: f32, vib: f32) -> vec3<f32> {
    if (adjustments.global.process_version >= 2u) {
        if (sat == 0.0 && vib == 0.0) { return color; }
        var lab = linear_to_oklab(max(color, vec3<f32>(0.0)));
        let chroma = sqrt(lab.y * lab.y + lab.z * lab.z);
        var scale_c = 1.0 + sat;
        if (vib != 0.0 && chroma > 0.0005) {
            // Vibrance: boosts muted colors more, with a skin guard
            // (Oklab skin hues sit around a≈+0.06..0.12, b≈+0.03..0.09).
            let sat_mask = 1.0 - smoothstep(0.08, 0.22, chroma);
            let hue_angle = atan2(lab.z, lab.y);
            let skin_dist = abs(hue_angle - 0.55);
            let skin_guard = mix(1.0, 0.5, smoothstep(0.5, 0.15, skin_dist));
            if (vib > 0.0) {
                scale_c *= 1.0 + vib * sat_mask * skin_guard * 2.0;
            } else {
                let desat_mask = 1.0 - smoothstep(0.05, 0.25, chroma);
                scale_c *= 1.0 + vib * desat_mask;
            }
        }
        lab.y *= max(scale_c, 0.0);
        lab.z *= max(scale_c, 0.0);
        return compress_gamut_soft(oklab_to_linear(lab));
    }
    var processed = color;
    let luma = get_luma(processed);

    if (sat != 0.0) {
        processed = mix(vec3<f32>(luma), processed, 1.0 + sat);
    }
    if (vib == 0.0) { return processed; }
    let c_max = max(processed.r, max(processed.g, processed.b));
    let c_min = min(processed.r, min(processed.g, processed.b));
    let delta = c_max - c_min;
    if (delta < 0.02) {
        return processed;
    }
    let current_sat = delta / max(c_max, 0.001);
    if (vib > 0.0) {
        let sat_mask = 1.0 - smoothstep(0.4, 0.9, current_sat);
        let hsv = rgb_to_hsv(processed);
        let hue = hsv.x;
        let skin_center = 25.0;
        let hue_dist = min(abs(hue - skin_center), 360.0 - abs(hue - skin_center));
        let is_skin = smoothstep(35.0, 10.0, hue_dist);
        let skin_dampener = mix(1.0, 0.6, is_skin);
        let amount = vib * sat_mask * skin_dampener * 3.0;
        processed = mix(vec3<f32>(luma), processed, 1.0 + amount);
    } else {
        let desat_mask = 1.0 - smoothstep(0.2, 0.8, current_sat);
        let amount = vib * desat_mask;
        processed = mix(vec3<f32>(luma), processed, 1.0 + amount);
    }
    return processed;
}

fn apply_hsl_panel(color: vec3<f32>, hsl_adjustments: array<HslColor, 8>, coords_i: vec2<i32>) -> vec3<f32> {
    let safe_color = max(color, vec3<f32>(0.0));
    if (distance(safe_color.r, safe_color.g) < 0.001 && distance(safe_color.g, safe_color.b) < 0.001) {
        return safe_color;
    }
    let original_hsv = rgb_to_hsv(safe_color);
    let original_luma = get_luma(safe_color);

    let saturation_mask = smoothstep(0.05, 0.20, original_hsv.y);
    let luminance_weight = smoothstep(0.0, 1.0, original_hsv.y);

    if (saturation_mask < 0.001 && luminance_weight < 0.001) {
        return safe_color;
    }

    let original_hue = original_hsv.x;

    var raw_influences: array<f32, 8>;
    var total_raw_influence: f32 = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let range = HSL_RANGES[i];
        let influence = get_raw_hsl_influence(original_hue, range.center, range.width);
        raw_influences[i] = influence;
        total_raw_influence += influence;
    }

    var total_hue_shift: f32 = 0.0;
    var total_sat_multiplier: f32 = 0.0;
    var total_lum_adjust: f32 = 0.0;

    for (var i = 0u; i < 8u; i = i + 1u) {
        let normalized_influence = raw_influences[i] / total_raw_influence;

        let hue_sat_influence = normalized_influence * saturation_mask;
        let luma_influence = normalized_influence * luminance_weight;

        total_hue_shift += hsl_adjustments[i].hue * 2.0 * hue_sat_influence;
        total_sat_multiplier += hsl_adjustments[i].saturation * hue_sat_influence;
        total_lum_adjust += hsl_adjustments[i].luminance * luma_influence;
    }

    if (original_hsv.y * (1.0 + total_sat_multiplier) < 0.0001) {
        let final_luma = original_luma * (1.0 + total_lum_adjust);
        return vec3<f32>(final_luma);
    }
    var hsv = original_hsv;
    hsv.x = (hsv.x + total_hue_shift + 360.0) % 360.0;
    hsv.y = clamp(hsv.y * (1.0 + total_sat_multiplier), 0.0, 1.0);
    let hs_shifted_rgb = hsv_to_rgb(vec3<f32>(hsv.x, hsv.y, original_hsv.z));
    let new_luma = get_luma(hs_shifted_rgb);
    let target_luma = original_luma * (1.0 + total_lum_adjust);
    if (new_luma < 0.0001) {
        return vec3<f32>(max(0.0, target_luma));
    }
    let final_color = hs_shifted_rgb * (target_luma / new_luma);
    return final_color;
}

fn apply_color_grading(color: vec3<f32>, shadows: ColorGradeSettings, midtones: ColorGradeSettings, highlights: ColorGradeSettings, global: ColorGradeSettings, blending: f32, balance: f32) -> vec3<f32> {
    let luma = get_luma(max(vec3(0.0), color));
    let base_shadow_crossover = 0.1;
    let base_highlight_crossover = 0.5;
    let balance_range = 0.5;
    let shadow_crossover = base_shadow_crossover + max(0.0, -balance) * balance_range;
    let highlight_crossover = base_highlight_crossover - max(0.0, balance) * balance_range;
    let feather = 0.2 * blending;
    let final_shadow_crossover = min(shadow_crossover, highlight_crossover - 0.01);
    let shadow_mask = 1.0 - smoothstep(final_shadow_crossover - feather, final_shadow_crossover + feather, luma);
    let highlight_mask = smoothstep(highlight_crossover - feather, highlight_crossover + feather, luma);
    let midtone_mask = max(0.0, 1.0 - shadow_mask - highlight_mask);
    let global_mask = 1.0;
    var graded_color = color;
    let shadow_sat_strength = 0.3;
    let shadow_lum_strength = 0.5;
    let midtone_sat_strength = 0.6;
    let midtone_lum_strength = 0.8;
    let highlight_sat_strength = 0.8;
    let highlight_lum_strength = 1.0;
    let global_sat_strength = 1.0;
    let global_lum_strength = 1.0;
    if (shadows.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(shadows.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * shadows.saturation * shadow_mask * shadow_sat_strength; }
    graded_color += shadows.luminance * shadow_mask * shadow_lum_strength;
    if (midtones.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(midtones.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * midtones.saturation * midtone_mask * midtone_sat_strength; }
    graded_color += midtones.luminance * midtone_mask * midtone_lum_strength;
    if (highlights.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(highlights.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * highlights.saturation * highlight_mask * highlight_sat_strength; }
    graded_color += highlights.luminance * highlight_mask * highlight_lum_strength;
    if (global.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(global.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * global.saturation * global_mask * global_sat_strength; }
    graded_color += global.luminance * global_mask * global_lum_strength;
    return graded_color;
}

fn apply_local_contrast(
    processed_color_linear: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    amount: f32,
    is_raw: u32,
    mode: u32,
    threshold: f32
) -> vec3<f32> {
    if (amount == 0.0) {
        return processed_color_linear;
    }

    var blurred_color_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_color_linear = blurred_color_input_space;
    } else {
        blurred_color_linear = srgb_to_linear(blurred_color_input_space);
    }

    if (amount < 0.0) {
        var blur_amount = -amount;
        if (mode == 0u) {
            blur_amount = blur_amount * 0.5;
        }
        return mix(processed_color_linear, blurred_color_linear, blur_amount);
    }

    let center_luma = get_luma(processed_color_linear);

    let shadow_threshold = select(0.03, 0.1, is_raw == 1u);
    let shadow_protection = smoothstep(0.0, shadow_threshold, center_luma);
    let highlight_protection = 1.0 - smoothstep(0.9, 1.0, center_luma);
    let midtone_mask = shadow_protection * highlight_protection;

    if (midtone_mask < 0.001) {
        return processed_color_linear;
    }

    let blurred_luma = get_luma(blurred_color_linear);
    let safe_center_luma = max(center_luma, 0.0001);
    let safe_blurred_luma = max(blurred_luma, 0.0001);

    let log_ratio = log2(safe_center_luma / safe_blurred_luma);
    var effective_amount = amount;

    if (mode == 0u) {
        let edge_magnitude = abs(log_ratio);
        let normalized_edge = clamp(edge_magnitude / 3.0, 0.0, 1.0);
        let edge_dampener = 1.0 - pow(normalized_edge, 0.5);
        let edge_mask = smoothstep(threshold * 0.5, threshold * 1.5, edge_magnitude);
        effective_amount = amount * edge_dampener * edge_mask * 0.8;
    } else {
        effective_amount = amount;
    }

    let contrast_factor = exp2(log_ratio * effective_amount);
    let final_color = processed_color_linear * contrast_factor;

    return mix(processed_color_linear, final_color, midtone_mask);
}

fn apply_centre_local_contrast(
    color_in: vec3<f32>,
    centre_amount: f32,
    coords_i: vec2<i32>,
    blurred_color_srgb: vec3<f32>,
    is_raw: u32
) -> vec3<f32> {
    if (centre_amount == 0.0) {
        return color_in;
    }
    let full_dims_f = vec2<f32>(textureDimensions(input_texture));
    let coord_f = vec2<f32>(coords_i);
    let midpoint = 0.4;
    let feather = 0.375;
    let aspect = full_dims_f.y / full_dims_f.x;
    let uv_centered = (coord_f / full_dims_f - 0.5) * 2.0;
    let d = length(uv_centered * vec2<f32>(1.0, aspect)) * 0.5;
    let vignette_mask = smoothstep(midpoint - feather, midpoint + feather, d);
    let centre_mask = 1.0 - vignette_mask;

    const CLARITY_SCALE: f32 = 0.9;
    var processed_color = color_in;
    let clarity_strength = centre_amount * (2.0 * centre_mask - 1.0) * CLARITY_SCALE;

    if (abs(clarity_strength) > 0.001) {
        processed_color = apply_local_contrast(processed_color, blurred_color_srgb, clarity_strength, is_raw, 1u, 0.0);
    }

    return processed_color;
}

fn apply_centre_tonal_and_color(
    color_in: vec3<f32>,
    centre_amount: f32,
    coords_i: vec2<i32>
) -> vec3<f32> {
    if (centre_amount == 0.0) {
        return color_in;
    }
    let full_dims_f = vec2<f32>(textureDimensions(input_texture));
    let coord_f = vec2<f32>(coords_i);
    let midpoint = 0.4;
    let feather = 0.375;
    let aspect = full_dims_f.y / full_dims_f.x;
    let uv_centered = (coord_f / full_dims_f - 0.5) * 2.0;
    let d = length(uv_centered * vec2<f32>(1.0, aspect)) * 0.5;
    let vignette_mask = smoothstep(midpoint - feather, midpoint + feather, d);
    let centre_mask = 1.0 - vignette_mask;

    const EXPOSURE_SCALE: f32 = 0.5;
    const VIBRANCE_SCALE: f32 = 0.4;
    const SATURATION_CENTER_SCALE: f32 = 0.3;
    const SATURATION_EDGE_SCALE: f32 = 0.8;

    var processed_color = color_in;

    let exposure_boost = centre_mask * centre_amount * EXPOSURE_SCALE;
    processed_color = apply_filmic_exposure(processed_color, exposure_boost);

    let vibrance_center_boost = centre_mask * centre_amount * VIBRANCE_SCALE;
    let saturation_center_boost = centre_mask * centre_amount * SATURATION_CENTER_SCALE;
    let saturation_edge_effect = -(1.0 - centre_mask) * centre_amount * SATURATION_EDGE_SCALE;
    let total_saturation_effect = saturation_center_boost + saturation_edge_effect;
    processed_color = apply_creative_color(processed_color, total_saturation_effect, vibrance_center_boost);

    return processed_color;
}

fn apply_dehaze(color: vec3<f32>, blurred_color_input_space: vec3<f32>, is_raw: u32, amount: f32) -> vec3<f32> {
    if (amount == 0.0) { return color; }

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    let atmospheric_light = vec3<f32>(0.95, 0.97, 1.0);

    if (amount > 0.0) {
        let pixel_dark = min(color.r, min(color.g, color.b));
        let regional_dark = min(blurred_linear.r, min(blurred_linear.g, blurred_linear.b));
        let pixel_luma = get_luma(max(color, vec3<f32>(0.0)));
        let blurred_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));
        let edge_diff = abs(pow(pixel_luma, 0.5) - pow(blurred_luma, 0.5));
        let halo_protection = smoothstep(0.02, 0.15, edge_diff);
        let spatial_dark = mix(regional_dark, pixel_dark, halo_protection);
        let safe_dark = max(spatial_dark - 0.02, 0.0);
        let mapped_haze = safe_dark / (safe_dark + 0.2);
        let t = max(1.0 - amount * mapped_haze * 0.85, 0.15);
        var recovered = (color - atmospheric_light) / t + atmospheric_light;
        let rec_luma = get_luma(max(recovered, vec3<f32>(0.0)));
        let shadow_lift = smoothstep(0.1, 0.0, rec_luma) * (1.0 - t) * 0.15;
        recovered += shadow_lift;
        let haze_removed = 1.0 - t;
        let sat_boost = haze_removed * 0.5;
        let final_luma = get_luma(max(recovered, vec3<f32>(0.0)));
        recovered = mix(vec3<f32>(final_luma), recovered, 1.0 + sat_boost);
        return max(recovered, vec3<f32>(0.0));
    } else {
        let regional_dark = min(blurred_linear.r, min(blurred_linear.g, blurred_linear.b));
        let safe_dark = max(regional_dark - 0.02, 0.0);
        let mapped_depth = safe_dark / (safe_dark + 0.2);
        let depth_factor = mix(0.4, 1.0, mapped_depth);
        return mix(color, atmospheric_light, abs(amount) * 0.7 * depth_factor);
    }
}

fn apply_noise_reduction(
    center_linear: vec3<f32>,
    coords_i: vec2<i32>,
    luma_amount: f32,
    color_amount: f32,
    scale: f32,
    is_raw: u32
) -> vec3<f32> {
    let luma_a  = clamp(luma_amount,  0.0, 1.0);
    let color_a = clamp(color_amount, 0.0, 1.0);
    if (luma_a < 0.001 && color_a < 0.001) {
        return center_linear;
    }

    let dims = vec2<i32>(textureDimensions(input_texture));
    let max_idx = dims - vec2<i32>(1);
    let center_safe   = max(center_linear, vec3<f32>(0.0));
    let center_luma   = get_luma(center_safe);
    let center_chroma = center_linear - vec3<f32>(center_luma);

    let res_factor = clamp(sqrt(scale), 0.5, 2.0);

    var new_luma   = center_luma;
    var new_chroma = center_chroma;

    // --- LUMA NOISE REDUCTION ---
    if (luma_a > 0.001) {
        let l_curve = sqrt(luma_a);

        let stride_f = mix(1.0, 2.0, smoothstep(0.45, 0.95, luma_a)) * res_factor;
        let extra    = clamp(stride_f - 1.0, 0.0, 1.0);

        let l_spatial = mix(1.0, 1.5, l_curve);
        let l_spat_n  = -1.0 / max(2.0 * l_spatial * l_spatial, 1e-6);

        let h1 = hash(vec2<f32>(coords_i));
        let h2 = hash(vec2<f32>(coords_i) + vec2<f32>(17.31, 71.13));

        var samp_luma: array<f32, 25>;
        var samp_spat: array<f32, 25>;
        var lmin: f32 = center_luma;
        var lmax: f32 = center_luma;

        samp_luma[0] = center_luma;
        samp_spat[0] = 1.0;

        var idx: u32 = 1u;
        for (var dy: i32 = -2; dy <= 2; dy = dy + 1) {
            for (var dx: i32 = -2; dx <= 2; dx = dx + 1) {
                if (dx == 0 && dy == 0) { continue; }

                let ring = max(abs(dx), abs(dy));
                let ring_factor = select(0.5, 1.0, ring == 2);
                let grow = 1.0 + extra * ring_factor;

                let jx = (h1 - 0.5) * 2.0 * extra;
                let jy = (h2 - 0.5) * 2.0 * extra;

                let off_f = vec2<f32>(f32(dx) * grow + jx, f32(dy) * grow + jy);
                let off   = vec2<i32>(i32(round(off_f.x)), i32(round(off_f.y)));
                let coord = clamp(coords_i + off, vec2<i32>(0), max_idx);

                var s = textureLoad(input_texture, vec2<u32>(coord), 0).rgb;
                if (is_raw == 0u) { s = srgb_to_linear(s); }
                let s_luma = get_luma(max(s, vec3<f32>(0.0)));
                samp_luma[idx] = s_luma;
                samp_spat[idx] = exp(f32(dx * dx + dy * dy) * l_spat_n);
                lmin = min(lmin, s_luma);
                lmax = max(lmax, s_luma);
                idx = idx + 1u;
            }
        }

        let luma_range    = lmax - lmin;
        let edge_strength = smoothstep(0.04, 0.20, luma_range);
        let edge_midpoint = (lmin + lmax) * 0.5;
        let center_side   = center_luma > edge_midpoint;

        let l_range_tol = mix(
            mix(0.025, 0.075, l_curve),
            mix(0.010, 0.025, l_curve),
            edge_strength
        );

        var samp_gate: array<f32, 25>;
        var sum_a: f32 = 0.0;
        var w_a:   f32 = 0.0;
        for (var k: u32 = 0u; k < 25u; k = k + 1u) {
            let diff = abs(samp_luma[k] - center_luma);
            let g_range = 1.0 - smoothstep(l_range_tol * 0.6, l_range_tol, diff);
            let s_side  = samp_luma[k] > edge_midpoint;
            let g_side  = select(0.0, 1.0, s_side == center_side);
            let g_edge  = mix(1.0, g_side, edge_strength);
            let w = samp_spat[k] * g_range * g_edge;
            samp_gate[k] = w;
            sum_a += samp_luma[k] * w;
            w_a   += w;
        }
        let initial_mean = sum_a / max(w_a, 1e-4);

        let outlier_tol = mix(0.07, 0.025, edge_strength);
        var sum_b: f32 = 0.0;
        var w_b:   f32 = 0.0;
        for (var k: u32 = 0u; k < 25u; k = k + 1u) {
            let init_w = samp_gate[k];
            if (init_w > 0.0001) {
                let d = samp_luma[k] - initial_mean;
                let r = abs(d) / outlier_tol;
                let bisq = max(0.0, 1.0 - r * r);
                let outlier_w = bisq * bisq;
                let w = init_w * outlier_w;
                sum_b += samp_luma[k] * w;
                w_b   += w;
            }
        }
        let robust_luma = select(initial_mean, sum_b / max(w_b, 1e-6), w_b > 0.01);

        let strength = luma_a * mix(1.0, 0.6, edge_strength);
        new_luma = mix(center_luma, robust_luma, strength);
    }

    if (color_a > 0.001) {
        let center_r_y = center_linear.r - center_luma;
        let center_b_y = center_linear.b - center_luma;
        let c_curve = sqrt(color_a);
        let stride_f = mix(2.0, 3.5, c_curve) * res_factor;

        let c_spatial = mix(2.0, 3.5, c_curve);
        let c_spat_n  = -1.0 / max(2.0 * c_spatial * c_spatial, 1e-6);

        let luma_tol = mix(0.12, 0.04, c_curve);
        let luma_n   = -1.0 / max(2.0 * luma_tol * luma_tol, 1e-6);

        let chroma_tol = mix(0.20, 0.08, c_curve);
        let chroma_n   = -1.0 / max(2.0 * chroma_tol * chroma_tol, 1e-6);

        let jh1 = hash(vec2<f32>(coords_i) + vec2<f32>(43.7, 91.1));
        let jh2 = hash(vec2<f32>(coords_i) + vec2<f32>(73.3, 17.9));
        let jx  = (jh1 - 0.5) * stride_f * 0.5;
        let jy  = (jh2 - 0.5) * stride_f * 0.5;

        var sum_r: f32 = center_r_y;
        var sum_b: f32 = center_b_y;
        var w_sum: f32 = 1.0;

        for (var dy: i32 = -2; dy <= 2; dy = dy + 1) {
            for (var dx: i32 = -2; dx <= 2; dx = dx + 1) {
                if (dx == 0 && dy == 0) { continue; }
                let off_f = vec2<f32>(f32(dx) * stride_f + jx, f32(dy) * stride_f + jy);
                let off   = vec2<i32>(i32(round(off_f.x)), i32(round(off_f.y)));
                let coord = clamp(coords_i + off, vec2<i32>(0), max_idx);
                var s = textureLoad(input_texture, vec2<u32>(coord), 0).rgb;

                if (is_raw == 0u) { s = srgb_to_linear(s); }

                let s_safe = max(s, vec3<f32>(0.0));
                let s_luma = get_luma(s_safe);
                let s_r_y  = s.r - s_luma;
                let s_b_y  = s.b - s_luma;

                let r2  = f32(dx * dx + dy * dy);
                let w_s = exp(r2 * c_spat_n);
                let dl  = s_luma - center_luma;
                let w_l = exp(dl * dl * luma_n);
                let dr  = s_r_y - center_r_y;
                let db  = s_b_y - center_b_y;
                let dc2 = dr * dr + db * db;
                let w_c = exp(dc2 * chroma_n);
                let w = w_s * w_l * w_c;

                sum_r += s_r_y * w;
                sum_b += s_b_y * w;
                w_sum += w;
            }
        }
        let filtered_r_y = sum_r / max(w_sum, 1e-6);
        let filtered_b_y = sum_b / max(w_sum, 1e-6);

        let new_r_y = mix(center_r_y, filtered_r_y, color_a);
        let new_b_y = mix(center_b_y, filtered_b_y, color_a);
        let new_g_y = -(LUMA_COEFF.r * new_r_y + LUMA_COEFF.b * new_b_y) / LUMA_COEFF.g;

        new_chroma = vec3<f32>(new_r_y, new_g_y, new_b_y);
    }

    return vec3<f32>(new_luma) + new_chroma;
}

fn apply_ca_correction(coords: vec2<u32>, ca_rc: f32, ca_by: f32) -> vec3<f32> {
    let dims = vec2<f32>(textureDimensions(input_texture));
    let center = dims / 2.0;
    let current_pos = vec2<f32>(coords);

    let to_center = current_pos - center;
    let dist = length(to_center);

    if (dist == 0.0) {
        return textureLoad(input_texture, coords, 0).rgb;
    }

    let dir = to_center / dist;

    let red_shift = dir * dist * ca_rc;
    let blue_shift = dir * dist * ca_by;

    let red_coords = vec2<i32>(round(current_pos - red_shift));
    let blue_coords = vec2<i32>(round(current_pos - blue_shift));
    let green_coords = vec2<i32>(current_pos);

    let max_coords = vec2<i32>(dims - 1.0);

    let r = textureLoad(input_texture, vec2<u32>(clamp(red_coords, vec2<i32>(0), max_coords)), 0).r;
    let g = textureLoad(input_texture, vec2<u32>(clamp(green_coords, vec2<i32>(0), max_coords)), 0).g;
    let b = textureLoad(input_texture, vec2<u32>(clamp(blue_coords, vec2<i32>(0), max_coords)), 0).b;

    return vec3<f32>(r, g, b);
}

const AGX_EPSILON: f32 = 1.0e-6;
const AGX_MIN_EV: f32 = -15.2;
const AGX_MAX_EV: f32 = 5.0;
const AGX_RANGE_EV: f32 = AGX_MAX_EV - AGX_MIN_EV;
const AGX_GAMMA: f32 = 2.4;
const AGX_SLOPE: f32 = 2.3843;
const AGX_TOE_POWER: f32 = 1.5;
const AGX_SHOULDER_POWER: f32 = 1.5;
const AGX_TOE_TRANSITION_X: f32 = 0.6060606;
const AGX_TOE_TRANSITION_Y: f32 = 0.43446;
const AGX_SHOULDER_TRANSITION_X: f32 = 0.6060606;
const AGX_SHOULDER_TRANSITION_Y: f32 = 0.43446;
const AGX_INTERCEPT: f32 = -1.0112;
const AGX_TOE_SCALE: f32 = -1.0359;
const AGX_SHOULDER_SCALE: f32 = 1.3475;
const AGX_TARGET_BLACK_PRE_GAMMA: f32 = 0.0;
const AGX_TARGET_WHITE_PRE_GAMMA: f32 = 1.0;

fn agx_sigmoid(x: f32, power: f32) -> f32 {
    return x / pow(1.0 + pow(x, power), 1.0 / power);
}

fn agx_scaled_sigmoid(x: f32, scale: f32, slope: f32, power: f32, transition_x: f32, transition_y: f32) -> f32 {
    return scale * agx_sigmoid(slope * (x - transition_x) / scale, power) + transition_y;
}

fn agx_apply_curve_channel(x: f32) -> f32 {
    var result: f32 = 0.0;
    if (x < AGX_TOE_TRANSITION_X) {
        result = agx_scaled_sigmoid(x, AGX_TOE_SCALE, AGX_SLOPE, AGX_TOE_POWER, AGX_TOE_TRANSITION_X, AGX_TOE_TRANSITION_Y);
    } else if (x <= AGX_SHOULDER_TRANSITION_X) {
        result = AGX_SLOPE * x + AGX_INTERCEPT;
    } else {
        result = agx_scaled_sigmoid(x, AGX_SHOULDER_SCALE, AGX_SLOPE, AGX_SHOULDER_POWER, AGX_SHOULDER_TRANSITION_X, AGX_SHOULDER_TRANSITION_Y);
    }
    return clamp(result, AGX_TARGET_BLACK_PRE_GAMMA, AGX_TARGET_WHITE_PRE_GAMMA);
}

fn agx_compress_gamut(c: vec3<f32>) -> vec3<f32> {
    let min_c = min(c.r, min(c.g, c.b));
    if (min_c < 0.0) {
        return c - min_c;
    }
    return c;
}

fn agx_tonemap(c: vec3<f32>) -> vec3<f32> {
    let x_relative = max(c / 0.18, vec3<f32>(AGX_EPSILON));
    let log_encoded = (log2(x_relative) - AGX_MIN_EV) / AGX_RANGE_EV;
    let mapped = clamp(log_encoded, vec3<f32>(0.0), vec3<f32>(1.0));

    var curved: vec3<f32>;
    curved.r = agx_apply_curve_channel(mapped.r);
    curved.g = agx_apply_curve_channel(mapped.g);
    curved.b = agx_apply_curve_channel(mapped.b);

    let final_color = pow(max(curved, vec3<f32>(0.0)), vec3<f32>(AGX_GAMMA));

    return final_color;
}

fn agx_full_transform(color_in: vec3<f32>) -> vec3<f32> {
    let compressed_color = agx_compress_gamut(color_in);
    let color_in_agx_space = adjustments.global.agx_pipe_to_rendering_matrix * compressed_color;
    let tonemapped_agx = agx_tonemap(color_in_agx_space);
    let final_color = adjustments.global.agx_rendering_to_pipe_matrix * tonemapped_agx;
    return final_color;
}

fn legacy_tonemap(c: vec3<f32>) -> vec3<f32> {
    const a: f32 = 2.51;
    const b: f32 = 0.03;
    const c_const: f32 = 2.43;
    const d: f32 = 0.59;
    const e: f32 = 0.14;

    let x = max(c, vec3<f32>(0.0));

    let numerator = x * (a * x + b);
    let denominator = x * (c_const * x + d) + e;

    let tonemapped = select(vec3<f32>(0.0), numerator / denominator, denominator > vec3<f32>(0.00001));

    return clamp(tonemapped, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn no_tonemap(c: vec3<f32>) -> vec3<f32> {
    return c;
}

fn is_default_curve(points: array<Point, 16>, count: u32) -> bool {
    if (count < 2u) {
        return false;
    }

    var is_identity = true;
    for (var i = 0u; i < count; i = i + 1u) {
        if (abs(points[i].x - points[i].y) > 0.5) {
            is_identity = false;
            break;
        }
    }

    let p0 = points[0];
    let p_last = points[count - 1u];
    let p0_is_origin = abs(p0.x - 0.0) < 0.1 && abs(p0.y - 0.0) < 0.1;
    let p_last_is_end = abs(p_last.x - 255.0) < 0.1 && abs(p_last.y - 255.0) < 0.1;

    return is_identity && p0_is_origin && p_last_is_end;
}

fn apply_all_curves(color: vec3<f32>, luma_curve: array<Point, 16>, luma_curve_count: u32, red_curve: array<Point, 16>, red_curve_count: u32, green_curve: array<Point, 16>, green_curve_count: u32, blue_curve: array<Point, 16>, blue_curve_count: u32) -> vec3<f32> {
    let red_is_default = is_default_curve(red_curve, red_curve_count);
    let green_is_default = is_default_curve(green_curve, green_curve_count);
    let blue_is_default = is_default_curve(blue_curve, blue_curve_count);
    let rgb_curves_are_active = !red_is_default || !green_is_default || !blue_is_default;

    if (rgb_curves_are_active) {
        let color_graded = vec3<f32>(apply_curve(color.r, red_curve, red_curve_count), apply_curve(color.g, green_curve, green_curve_count), apply_curve(color.b, blue_curve, blue_curve_count));
        let luma_initial = get_luma(color);
        let luma_target = apply_curve(luma_initial, luma_curve, luma_curve_count);
        let luma_graded = get_luma(color_graded);
        var final_color: vec3<f32>;
        if (luma_graded > 0.001) { final_color = color_graded * (luma_target / luma_graded); } else { final_color = vec3<f32>(luma_target); }
        let max_comp = max(final_color.r, max(final_color.g, final_color.b));
        if (max_comp > 1.0) { final_color = final_color / max_comp; }
        return final_color;
    } else {
        return vec3<f32>(apply_curve(color.r, luma_curve, luma_curve_count), apply_curve(color.g, luma_curve, luma_curve_count), apply_curve(color.b, luma_curve, luma_curve_count));
    }
}

fn get_mask_influence(mask_index: u32, coords: vec2<u32>) -> f32 {
    return textureLoad(mask_textures, vec2<i32>(coords), i32(mask_index), 0).r;
}

// Fujifilm F-Log2 C (constants from the official F-Log2C Data Sheet
// Ver.1.0; Rust mirror + spec-pinned tests live in flog2c.rs).
const SRGB_TO_FGAMUT_C = mat3x3<f32>(
    vec3<f32>(0.51706902, 0.08861716, 0.01775004),
    vec3<f32>(0.41293468, 0.80926315, 0.10944762),
    vec3<f32>(0.06999630, 0.10211969, 0.87280234),
);

fn flog2_encode(x: f32) -> f32 {
    let t = max(x, 0.0);
    if (t >= 0.000889) {
        // c * log10(a*t + b) + d
        return 0.245281 * log(5.555556 * t + 0.064829) * 0.4342944819 + 0.384316;
    }
    return 8.799461 * t + 0.092864;
}

fn sample_lut_tetrahedral(uv: vec3<f32>) -> vec3<f32> {
    let dims = vec3<f32>(textureDimensions(lut_texture));
    let size = dims - vec3<f32>(1.0);
    let scaled = clamp(uv, vec3<f32>(0.0), vec3<f32>(1.0)) * size;
    let i_base = floor(scaled);
    let f = scaled - i_base;
    let coord0 = vec3<i32>(i_base);
    let coord1 = min(coord0 + vec3<i32>(1), vec3<i32>(dims) - vec3<i32>(1));
    let c000 = textureLoad(lut_texture, coord0, 0).rgb;
    let c111 = textureLoad(lut_texture, coord1, 0).rgb;

    var res = vec3<f32>(0.0);

    if (f.r > f.g) {
        if (f.g > f.b) {
            let c100 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord0.z), 0).rgb;
            let c110 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord1.y, coord0.z), 0).rgb;

            res = c000 * (1.0 - f.r) +
                  c100 * (f.r - f.g) +
                  c110 * (f.g - f.b) +
                  c111 * (f.b);
        } else if (f.r > f.b) {
            let c100 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord0.z), 0).rgb;
            let c101 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.r) +
                  c100 * (f.r - f.b) +
                  c101 * (f.b - f.g) +
                  c111 * (f.g);
        } else {
            let c001 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord0.y, coord1.z), 0).rgb;
            let c101 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.b) +
                  c001 * (f.b - f.r) +
                  c101 * (f.r - f.g) +
                  c111 * (f.g);
        }
    } else {
        if (f.b > f.g) {
            let c001 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord0.y, coord1.z), 0).rgb;
            let c011 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.b) +
                  c001 * (f.b - f.g) +
                  c011 * (f.g - f.r) +
                  c111 * (f.r);
        } else if (f.b > f.r) {
            let c010 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord0.z), 0).rgb;
            let c011 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.g) +
                  c010 * (f.g - f.b) +
                  c011 * (f.b - f.r) +
                  c111 * (f.r);
        } else {
            let c010 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord0.z), 0).rgb;
            let c110 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord1.y, coord0.z), 0).rgb;

            res = c000 * (1.0 - f.g) +
                  c010 * (f.g - f.r) +
                  c110 * (f.r - f.b) +
                  c111 * (f.b);
        }
    }

    return res;
}

// Film-density saturation: print film eases saturation out of deep
// shadows and near-white instead of holding it constant — a large part of
// why film renders read as "expensive" while digital saturation reads as
// loud. Runs in Oklab so the ease never shifts hue.
fn apply_film_saturation(color: vec3<f32>, amount: f32) -> vec3<f32> {
    if (amount <= 0.001) {
        return color;
    }
    let lab = linear_to_oklab(max(color, vec3<f32>(0.0)));
    let l = clamp(lab.x, 0.0, 1.2);
    let highlight_ease = smoothstep(0.78, 1.05, l);
    let shadow_ease = 1.0 - smoothstep(0.05, 0.28, l);
    let desat = clamp((highlight_ease * 0.75 + shadow_ease * 0.55) * amount, 0.0, 0.9);
    return oklab_to_linear(vec3<f32>(lab.x, lab.yz * (1.0 - desat)));
}

fn apply_glow_bloom(
    color: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    amount: f32,
    is_raw: u32,
    exp: f32, bright: f32, con: f32, wh: f32
) -> vec3<f32> {
    if (amount <= 0.0) {
        return color;
    }

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    blurred_linear = apply_linear_exposure(blurred_linear, exp);
    blurred_linear = apply_filmic_exposure(blurred_linear, bright);
    blurred_linear = apply_tonal_adjustments(blurred_linear, blurred_color_input_space, blurred_color_input_space, is_raw, 0.0, 0.0, wh, 0.0, 0.5);

    let linear_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));

    var perceptual_luma: f32;
    if (linear_luma <= 1.0) {
        perceptual_luma = pow(max(linear_luma, 0.0), 1.0 / 2.2);
    } else {
        perceptual_luma = 1.0 + pow(linear_luma - 1.0, 1.0 / 2.2);
    }

    let luma_cutoff = mix(0.75, 0.08, clamp(amount, 0.0, 1.0));

    let cutoff_fade = smoothstep(
        luma_cutoff,
        luma_cutoff + 0.15,
        perceptual_luma
    );

    let excess = max(perceptual_luma - luma_cutoff, 0.0);

    let falloff_range = 5.5;
    let normalized = excess / falloff_range;

    let bloom_intensity =
        pow(smoothstep(0.0, 1.0, normalized), 0.45);

    var bloom_color: vec3<f32>;
    if (linear_luma > 0.01) {
        let color_ratio = blurred_linear / linear_luma;
        let warm_tint = vec3<f32>(1.03, 1.0, 0.97);
        bloom_color = color_ratio * warm_tint;
    } else {
        bloom_color = vec3<f32>(1.0, 0.99, 0.98);
    }

    let luma_factor = pow(linear_luma, 0.6);

    let black_gate_width = 0.5;
    let black_gate_raw = smoothstep(0.0, black_gate_width, linear_luma);
    let black_gate = pow(black_gate_raw, 0.5);

    bloom_color *= bloom_intensity * luma_factor * cutoff_fade * black_gate;

    let current_luma = get_luma(max(color, vec3<f32>(0.0)));
    let protection = 1.0 - smoothstep(1.0, 2.2, current_luma);

    return color + bloom_color * amount * 3.8 * protection;
}

fn apply_halation(
    color: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    amount: f32,
    is_raw: u32,
    exp: f32, bright: f32, con: f32, wh: f32
) -> vec3<f32> {
    if (amount <= 0.0) { return color; }

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    blurred_linear = apply_linear_exposure(blurred_linear, exp);
    blurred_linear = apply_filmic_exposure(blurred_linear, bright);
    blurred_linear = apply_tonal_adjustments(blurred_linear, blurred_color_input_space, blurred_color_input_space, is_raw, 0.0, 0.0, wh, 0.0, 0.5);

    let linear_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));

    var perceptual_luma: f32;
    if (linear_luma <= 1.0) {
        perceptual_luma = pow(max(linear_luma, 0.0), 1.0 / 2.2);
    } else {
        perceptual_luma = 1.0 + pow(linear_luma - 1.0, 1.0 / 2.2);
    }

    let luma_cutoff = mix(0.85, 0.1, clamp(amount, 0.0, 1.0));

    if (perceptual_luma <= luma_cutoff) { return color; }

    let excess = perceptual_luma - luma_cutoff;
    let range = max(1.5 - luma_cutoff, 0.1);
    let halation_mask = smoothstep(0.0, range * 0.6, excess);

    let halation_core = vec3<f32>(1.0, 0.15, 0.03);
    let halation_fringe = vec3<f32>(1.0, 0.32, 0.10);

    let intensity_blend = smoothstep(0.0, 0.7, halation_mask);
    let halation_tint = mix(halation_fringe, halation_core, intensity_blend);

    let glow_intensity = halation_mask * linear_luma;
    let halation_glow = halation_tint * glow_intensity;

    let color_luma = get_luma(max(color, vec3<f32>(0.0)));
    let desat_strength = halation_mask * 0.12;
    let affected_color = mix(color, vec3<f32>(color_luma), desat_strength);

    let contrast_reduced = mix(vec3<f32>(0.5), affected_color, 1.0 - halation_mask * 0.06);

    return contrast_reduced + halation_glow * amount * 2.5;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let out_dims = vec2<u32>(textureDimensions(output_texture));
    if (id.x >= out_dims.x || id.y >= out_dims.y) { return; }

    const REFERENCE_DIMENSION: f32 = 1080.0;
    let full_dims = vec2<f32>(textureDimensions(input_texture));
    let current_ref_dim = min(full_dims.x, full_dims.y);
    let scale = max(0.1, current_ref_dim / REFERENCE_DIMENSION);

    let absolute_coord = id.xy + vec2<u32>(adjustments.tile_offset_x, adjustments.tile_offset_y);
    let absolute_coord_i = vec2<i32>(absolute_coord);

    let ca_rc = adjustments.global.chromatic_aberration_red_cyan;
    let ca_by = adjustments.global.chromatic_aberration_blue_yellow;
    var color_from_texture = textureLoad(input_texture, absolute_coord, 0).rgb;
    if (abs(ca_rc) > 0.000001 || abs(ca_by) > 0.000001) {
        color_from_texture = apply_ca_correction(absolute_coord, ca_rc, ca_by);
    }
    let original_alpha = textureLoad(input_texture, absolute_coord, 0).a;

    var initial_linear_rgb: vec3<f32>;
    let is_raw = adjustments.global.is_raw_image;
    if (is_raw == 0u) {
        initial_linear_rgb = srgb_to_linear(color_from_texture);
    } else {
        initial_linear_rgb = color_from_texture;
    }

    var t_exposure = adjustments.global.exposure;
    var t_brightness = adjustments.global.brightness;
    var t_contrast = adjustments.global.contrast;
    var t_highlights = adjustments.global.highlights;
    var t_shadows = adjustments.global.shadows;
    var t_whites = adjustments.global.whites;
    var t_blacks = adjustments.global.blacks;
    var t_saturation = adjustments.global.saturation;
    var t_temperature = adjustments.global.temperature;
    var t_tint = adjustments.global.tint;
    var t_vibrance = adjustments.global.vibrance;
    var t_luma_nr = adjustments.global.luma_noise_reduction;
    var t_color_nr = adjustments.global.color_noise_reduction;
    var t_clarity = adjustments.global.clarity;
    var t_dehaze = adjustments.global.dehaze;
    var t_structure = adjustments.global.structure;
    var t_glow = adjustments.global.glow_amount;
    var t_halation = adjustments.global.halation_amount;
    var t_flare = adjustments.global.flare_amount;
    var t_sharpness = adjustments.global.sharpness;
    var t_hue = adjustments.global.hue;
    var t_texture = adjustments.global.texture_amount;
    var t_pivot = adjustments.global.contrast_pivot;
    var t_cw_lift = adjustments.global.cw_lift;
    var t_cw_gamma = adjustments.global.cw_gamma;
    var t_cw_gain = adjustments.global.cw_gain;
    var t_cw_offset = adjustments.global.cw_offset;

    var h0_h = adjustments.global.hsl[0].hue; var h0_s = adjustments.global.hsl[0].saturation; var h0_l = adjustments.global.hsl[0].luminance;
    var h1_h = adjustments.global.hsl[1].hue; var h1_s = adjustments.global.hsl[1].saturation; var h1_l = adjustments.global.hsl[1].luminance;
    var h2_h = adjustments.global.hsl[2].hue; var h2_s = adjustments.global.hsl[2].saturation; var h2_l = adjustments.global.hsl[2].luminance;
    var h3_h = adjustments.global.hsl[3].hue; var h3_s = adjustments.global.hsl[3].saturation; var h3_l = adjustments.global.hsl[3].luminance;
    var h4_h = adjustments.global.hsl[4].hue; var h4_s = adjustments.global.hsl[4].saturation; var h4_l = adjustments.global.hsl[4].luminance;
    var h5_h = adjustments.global.hsl[5].hue; var h5_s = adjustments.global.hsl[5].saturation; var h5_l = adjustments.global.hsl[5].luminance;
    var h6_h = adjustments.global.hsl[6].hue; var h6_s = adjustments.global.hsl[6].saturation; var h6_l = adjustments.global.hsl[6].luminance;
    var h7_h = adjustments.global.hsl[7].hue; var h7_s = adjustments.global.hsl[7].saturation; var h7_l = adjustments.global.hsl[7].luminance;

    for (var i = 0u; i < adjustments.mask_count; i = i + 1u) {
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            let m = adjustments.mask_adjustments[i];

            t_exposure += m.exposure * influence;
            t_brightness += m.brightness * influence;
            t_contrast += m.contrast * influence;
            t_highlights += m.highlights * influence;
            t_shadows += m.shadows * influence;
            t_whites += m.whites * influence;
            t_blacks += m.blacks * influence;

            t_saturation += m.saturation * influence;
            t_temperature += m.temperature * influence;
            t_tint += m.tint * influence;
            t_vibrance += m.vibrance * influence;

            t_luma_nr += m.luma_noise_reduction * influence;
            t_color_nr += m.color_noise_reduction * influence;
            t_clarity += m.clarity * influence;
            t_dehaze += m.dehaze * influence;
            t_structure += m.structure * influence;

            t_glow += m.glow_amount * influence;
            t_halation += m.halation_amount * influence;
            t_flare += m.flare_amount * influence;
            t_hue += m.hue * influence;
            t_texture += m.texture_amount * influence;
            t_pivot += m.contrast_pivot * influence;
            t_cw_lift += m.cw_lift * influence;
            t_cw_gamma += m.cw_gamma * influence;
            t_cw_gain += m.cw_gain * influence;
            t_cw_offset += m.cw_offset * influence;

            h0_h += m.hsl[0].hue * influence; h0_s += m.hsl[0].saturation * influence; h0_l += m.hsl[0].luminance * influence;
            h1_h += m.hsl[1].hue * influence; h1_s += m.hsl[1].saturation * influence; h1_l += m.hsl[1].luminance * influence;
            h2_h += m.hsl[2].hue * influence; h2_s += m.hsl[2].saturation * influence; h2_l += m.hsl[2].luminance * influence;
            h3_h += m.hsl[3].hue * influence; h3_s += m.hsl[3].saturation * influence; h3_l += m.hsl[3].luminance * influence;
            h4_h += m.hsl[4].hue * influence; h4_s += m.hsl[4].saturation * influence; h4_l += m.hsl[4].luminance * influence;
            h5_h += m.hsl[5].hue * influence; h5_s += m.hsl[5].saturation * influence; h5_l += m.hsl[5].luminance * influence;
            h6_h += m.hsl[6].hue * influence; h6_s += m.hsl[6].saturation * influence; h6_l += m.hsl[6].luminance * influence;
            h7_h += m.hsl[7].hue * influence; h7_s += m.hsl[7].saturation * influence; h7_l += m.hsl[7].luminance * influence;
        }
    }

    let final_hsl = array<HslColor, 8>(
        HslColor(h0_h, h0_s, h0_l, 0.0), HslColor(h1_h, h1_s, h1_l, 0.0),
        HslColor(h2_h, h2_s, h2_l, 0.0), HslColor(h3_h, h3_s, h3_l, 0.0),
        HslColor(h4_h, h4_s, h4_l, 0.0), HslColor(h5_h, h5_s, h5_l, 0.0),
        HslColor(h6_h, h6_s, h6_l, 0.0), HslColor(h7_h, h7_s, h7_l, 0.0)
    );

    initial_linear_rgb = apply_noise_reduction(
        initial_linear_rgb, absolute_coord_i,
        t_luma_nr, t_color_nr, scale, is_raw
    );

    let sharpness_blurred = textureLoad(sharpness_blur_texture, id.xy, 0).rgb;
    let tonal_blurred = textureLoad(tonal_blur_texture, id.xy, 0).rgb;
    let clarity_blurred = textureLoad(clarity_blur_texture, id.xy, 0).rgb;
    let structure_blurred = textureLoad(structure_blur_texture, id.xy, 0).rgb;

    var locally_contrasted_rgb = initial_linear_rgb;

    locally_contrasted_rgb = apply_local_contrast(
        locally_contrasted_rgb, sharpness_blurred,
        t_sharpness, is_raw, 0u, adjustments.global.sharpness_threshold
    );

    var sharpness_delta = vec3<f32>(0.0);
    for (var i = 0u; i < adjustments.mask_count; i = i + 1u) {
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            let m = adjustments.mask_adjustments[i];
            if (abs(m.sharpness) > 0.001) {
                let local_sharp_result = apply_local_contrast(
                    initial_linear_rgb, sharpness_blurred,
                    m.sharpness, is_raw, 0u, m.sharpness_threshold
                );
                sharpness_delta += (local_sharp_result - initial_linear_rgb) * influence;
            }
        }
    }
    locally_contrasted_rgb += sharpness_delta;

    // Texture: mid-fine detail band (Lightroom-style) — local contrast
    // against the small-radius blur, gentler than sharpening, no threshold.
    locally_contrasted_rgb = apply_local_contrast(locally_contrasted_rgb, sharpness_blurred, t_texture * 0.75, is_raw, 1u, 0.0);
    locally_contrasted_rgb = apply_local_contrast(locally_contrasted_rgb, clarity_blurred, t_clarity, is_raw, 1u, 0.0);
    locally_contrasted_rgb = apply_local_contrast(locally_contrasted_rgb, structure_blurred, t_structure, is_raw, 1u, 0.0);
    locally_contrasted_rgb = apply_centre_local_contrast(locally_contrasted_rgb, adjustments.global.centre, absolute_coord_i, clarity_blurred, is_raw);

    var processed_rgb = apply_linear_exposure(locally_contrasted_rgb, t_exposure);

    if (t_glow > 0.0) {
        processed_rgb = apply_glow_bloom(
            processed_rgb, structure_blurred, t_glow, is_raw,
            t_exposure, t_brightness, t_contrast, t_whites
        );
    }
    if (t_halation > 0.0) {
        // Layered falloff: the 8px blur keeps a hot core, the 40px blur
        // lets the red spill breathe wide like real emulsion halation.
        let halation_spread = mix(clarity_blurred, structure_blurred, 0.6);
        processed_rgb = apply_halation(
            processed_rgb, halation_spread, t_halation, is_raw,
            t_exposure, t_brightness, t_contrast, t_whites
        );
    }
    if (t_flare > 0.0) {
        let uv = vec2<f32>(absolute_coord) / full_dims;
        var flare_color = textureSampleLevel(flare_texture, flare_sampler, uv, 0.0).rgb;
        flare_color *= 1.4;
        flare_color = flare_color * flare_color;
        let linear_luma = get_luma(max(processed_rgb, vec3<f32>(0.0)));
        var perceptual_luma: f32;
        if (linear_luma <= 1.0) {
            perceptual_luma = pow(max(linear_luma, 0.0), 1.0 / 2.2);
        } else {
            perceptual_luma = 1.0 + pow(linear_luma - 1.0, 1.0 / 2.2);
        }
        let protection = 1.0 - smoothstep(0.7, 1.8, perceptual_luma);
        processed_rgb += flare_color * t_flare * protection;
    }

    var composite_rgb_linear = apply_dehaze(processed_rgb, structure_blurred, is_raw, t_dehaze);
    composite_rgb_linear = apply_centre_tonal_and_color(composite_rgb_linear, adjustments.global.centre, absolute_coord_i);
    composite_rgb_linear = apply_white_balance(composite_rgb_linear, t_temperature, t_tint);
    composite_rgb_linear = apply_color_wheels(composite_rgb_linear, t_cw_lift, t_cw_gamma, t_cw_gain, t_cw_offset);
    composite_rgb_linear = apply_filmic_exposure(composite_rgb_linear, t_brightness);
    composite_rgb_linear = apply_tonal_adjustments(composite_rgb_linear, tonal_blurred, structure_blurred, is_raw, t_contrast, t_shadows, t_whites, t_blacks, t_pivot);
    composite_rgb_linear = apply_highlights_adjustment(composite_rgb_linear, tonal_blurred, structure_blurred, is_raw, t_highlights);
    composite_rgb_linear = apply_color_calibration(composite_rgb_linear, adjustments.global.color_calibration);
    composite_rgb_linear = apply_hsl_panel(composite_rgb_linear, final_hsl, absolute_coord_i);
    composite_rgb_linear = apply_hue_shift(composite_rgb_linear, t_hue);
    composite_rgb_linear = apply_hue_curves(composite_rgb_linear);
    composite_rgb_linear = apply_point_colors(composite_rgb_linear, tonal_blurred, is_raw);
    composite_rgb_linear = apply_creative_color(composite_rgb_linear, t_saturation, t_vibrance);
    composite_rgb_linear = apply_film_saturation(composite_rgb_linear, adjustments.global.film_saturation);

    composite_rgb_linear = apply_color_grading(
        composite_rgb_linear,
        adjustments.global.color_grading_shadows,
        adjustments.global.color_grading_midtones,
        adjustments.global.color_grading_highlights,
        adjustments.global.color_grading_global,
        adjustments.global.color_grading_blending,
        adjustments.global.color_grading_balance
    );

    for (var i = 0u; i < adjustments.mask_count; i = i + 1u) {
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            let m = adjustments.mask_adjustments[i];
            let mask_graded = apply_color_grading(
                composite_rgb_linear,
                m.color_grading_shadows, m.color_grading_midtones, m.color_grading_highlights, m.color_grading_global, m.color_grading_blending, m.color_grading_balance
            );
            composite_rgb_linear = mix(composite_rgb_linear, mask_graded, influence);
        }
    }

    if (adjustments.global.vignette_amount != 0.0) {
        let full_dims_f = vec2<f32>(textureDimensions(input_texture));
        let coord_f = vec2<f32>(absolute_coord);
        let v_amount = adjustments.global.vignette_amount;
        let v_mid = adjustments.global.vignette_midpoint;
        let v_round = 1.0 - adjustments.global.vignette_roundness;
        let v_feather = adjustments.global.vignette_feather * 0.5;
        let aspect = full_dims_f.y / full_dims_f.x;
        let uv_centered = (coord_f / full_dims_f - 0.5) * 2.0;
        let uv_round = sign(uv_centered) * pow(abs(uv_centered), vec2<f32>(v_round, v_round));
        // Radius normalized so the extreme corner is exactly 1.0 — the old
        // ring mask peaked at ~0.78 in the corner tip and near zero at
        // half-radius, so even ±4 stops "barely did anything" against real
        // lens vignetting (which spreads smoothly from ~30% radius out).
        let corner_len = length(vec2<f32>(1.0, aspect));
        let dn = length(uv_round * vec2<f32>(1.0, aspect)) / max(corner_len, 1e-4);
        // Polynomial radial profile like actual lens falloff: midpoint sets
        // where it begins, feather sets curve softness, corners ALWAYS get
        // full strength.
        let start = v_mid * 0.6;
        let shape = 1.0 + v_feather * 4.0;
        let vignette_mask = pow(clamp((dn - start) / max(1.0 - start, 1e-4), 0.0, 1.0), shape);
        // Exposure-style gain in linear light, both directions. Brightening
        // used to BLEND TOWARD WHITE — devignetting painted white mist over
        // the corners instead of pulling the darks up; darkening crushed
        // linearly to black. Soft-knee response: gentle mid-range for
        // creative vignettes, ramping to ±4 stops at full deflection —
        // severe lens vignetting runs 3-4 stops in extreme corners and the
        // correction must reach that far.
        let stops = v_amount * (1.6 + 2.4 * abs(v_amount));
        let pre_luma = get_luma(max(composite_rgb_linear, vec3<f32>(0.0)));
        composite_rgb_linear *= exp2(stops * vignette_mask);
        // Crushed-black rescue: multiplication cannot lift corners the
        // camera clipped to ~0 (measured: display 1 -> 12 at full gain).
        // The floor lift is (a) gated to pixels that were actually crushed
        // — surviving detail gets pure gain, no haze — and (b) tinted by
        // the blurred neighborhood so recovered blacks inherit the scene's
        // cast instead of reading as gray mist. amount^2 keeps the
        // mid-range physically clean.
        if (v_amount > 0.0) {
            let crushed = 1.0 - smoothstep(0.008, 0.09, pre_luma);
            if (crushed > 0.001) {
                var nb = max(structure_blurred, vec3<f32>(0.0));
                if (is_raw != 1u) {
                    nb = srgb_to_linear(nb);
                }
                let tint = clamp(nb / max(get_luma(nb), 1e-4), vec3<f32>(0.4), vec3<f32>(2.2));
                let lift = v_amount * v_amount * 0.05 * vignette_mask * crushed;
                composite_rgb_linear += tint * lift;
            }
        }
    }

    if (adjustments.global.process_version >= 2u) {
        composite_rgb_linear = compress_gamut_soft(composite_rgb_linear);
    }

    var base_srgb: vec3<f32>;
    if (adjustments.global.tonemapper_mode == 2u) {
        // Filmic: soft shoulder + path-to-white instead of a hard clip.
        base_srgb = linear_to_srgb(tonemap_filmic(composite_rgb_linear));
    } else if (adjustments.global.tonemapper_mode == 1u) {
        base_srgb = agx_full_transform(composite_rgb_linear);
    } else if (is_raw == 1u) {
        var srgb_emulated = linear_to_srgb(composite_rgb_linear);
        const BRIGHTNESS_GAMMA: f32 = 1.1;
        srgb_emulated = pow(srgb_emulated, vec3<f32>(1.0 / BRIGHTNESS_GAMMA));
        const CONTRAST_MIX: f32 = 0.75;
        let contrast_curve = srgb_emulated * srgb_emulated * (3.0 - 2.0 * srgb_emulated);
        base_srgb = mix(srgb_emulated, contrast_curve, CONTRAST_MIX);
    } else {
        base_srgb = linear_to_srgb(composite_rgb_linear);
    }

    var final_rgb = apply_all_curves(base_srgb,
        adjustments.global.luma_curve, adjustments.global.luma_curve_count,
        adjustments.global.red_curve, adjustments.global.red_curve_count,
        adjustments.global.green_curve, adjustments.global.green_curve_count,
        adjustments.global.blue_curve, adjustments.global.blue_curve_count
    );

    for (var i = 0u; i < adjustments.mask_count; i = i + 1u) {
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            let m = adjustments.mask_adjustments[i];
            let mask_curved_srgb = apply_all_curves(final_rgb,
                m.luma_curve, m.luma_curve_count,
                m.red_curve, m.red_curve_count,
                m.green_curve, m.green_curve_count,
                m.blue_curve, m.blue_curve_count
            );
            final_rgb = mix(final_rgb, mask_curved_srgb, influence);
        }
    }

    if (adjustments.global.has_lut == 1u) {
        var lut_color: vec3<f32>;
        if (adjustments.global.lut_input_space == 1u) {
            // Film-sim LUT: feed the cube scene-linear pixels encoded
            // exactly as a Fujifilm body would (F-Gamut C + F-Log2 curve);
            // its output IS the finished BT.709 film-simulation image —
            // the LUT replaces our display rendering at full intensity.
            let sim_gain = exp2(adjustments.global.lut_sim_exposure);
            let fgc = SRGB_TO_FGAMUT_C * (max(composite_rgb_linear, vec3<f32>(0.0)) * sim_gain);
            let encoded = vec3<f32>(
                flog2_encode(fgc.r),
                flog2_encode(fgc.g),
                flog2_encode(fgc.b),
            );
            lut_color = sample_lut_tetrahedral(encoded);
        } else {
            lut_color = sample_lut_tetrahedral(final_rgb);
        }
        final_rgb = mix(final_rgb, lut_color, adjustments.global.lut_intensity);
    }

    if (adjustments.global.grain_amount > 0.0) {
        let coord = vec2<f32>(absolute_coord_i);
        let amount = adjustments.global.grain_amount * 0.5;
        let grain_frequency = (1.0 / max(adjustments.global.grain_size, 0.1)) / scale;
        let roughness = adjustments.global.grain_roughness;
        let luma = max(0.0, get_luma(final_rgb));
        let luma_mask = smoothstep(0.0, 0.15, luma) * (1.0 - smoothstep(0.6, 1.0, luma));
        let base_coord = coord * grain_frequency;
        let rough_coord = coord * grain_frequency * 0.6;
        let noise_base = gradient_noise(base_coord);
        let noise_rough = gradient_noise(rough_coord + vec2<f32>(5.2, 1.3));
        let noise_val = mix(noise_base, noise_rough, roughness);
        final_rgb += vec3<f32>(noise_val) * amount * luma_mask;
    }

    if (adjustments.global.show_clipping == 1u) {
        let HIGHLIGHT_WARNING_COLOR = vec3<f32>(1.0, 0.0, 0.0);
        let SHADOW_WARNING_COLOR = vec3<f32>(0.0, 0.0, 1.0);
        let HIGHLIGHT_CLIP_THRESHOLD = 0.998;
        let SHADOW_CLIP_THRESHOLD = 0.002;
        if (any(final_rgb > vec3<f32>(HIGHLIGHT_CLIP_THRESHOLD))) {
            final_rgb = HIGHLIGHT_WARNING_COLOR;
        } else if (any(final_rgb < vec3<f32>(SHADOW_CLIP_THRESHOLD))) {
            final_rgb = SHADOW_WARNING_COLOR;
        }
    }

    let dither_amount = 1.0 / 255.0;
    final_rgb += dither(id.xy) * dither_amount;

    textureStore(output_texture, id.xy, vec4<f32>(clamp(final_rgb, vec3<f32>(0.0), vec3<f32>(1.0)), original_alpha));
}
