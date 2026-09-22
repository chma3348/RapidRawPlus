struct Parameters {
    source_to_work: mat3x3<f32>,
    work_to_output: mat3x3<f32>,
    modes: vec4<u32>,
    flags: vec4<u32>,
    work_to_lms: mat3x3<f32>,
    lms_to_work: mat3x3<f32>,
    white_balance: mat3x3<f32>,
    tone: vec4<f32>,
    color: vec4<f32>,
    bands: array<vec4<f32>,8>,
    grading: array<vec4<f32>,4>,
    curve: array<vec4<f32>,5>,
    range_center: array<vec4<f32>,8>,
    range_width: array<vec4<f32>,8>,
    range_adjustment: array<vec4<f32>,8>,
    frame: vec4<u32>,
    effects: array<vec4<f32>,3>,
    channel_curves: array<vec4<f32>,15>,
    curve_flags: vec4<u32>,
    look: vec4<f32>,
    look_flags: vec4<u32>,
    work_to_look: mat3x3<f32>,
    calibration: array<vec4<f32>,2>,
    srgb_to_work: mat3x3<f32>,
    basic: array<vec4<f32>,2>,
    basic_flags: vec4<u32>,
    agx_to: mat3x3<f32>,
    agx_from: mat3x3<f32>,
}
@group(0) @binding(0) var<storage, read> source: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> results: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> parameters: Parameters;
// One dummy entry when no captured transform is in use: the layout is fixed.
@group(0) @binding(3) var<storage, read> cube: array<vec4<f32>>;
// A creative LUT, likewise one dummy entry when there is none.
@group(0) @binding(4) var<storage, read> look_table: array<vec4<f32>>;
// Per pixel of this chunk: tonal blur, structure blur. A dummy when unbound.
@group(0) @binding(5) var<storage, read> neighbourhood: array<vec4<f32>>;
// The previous engine's measured Resolve shadow correction, 33^3, red fastest.
@group(0) @binding(6) var<storage, read> shadow_correction_table: array<f32>;

// Same table and interpolation as the previous engine's, which samples it
// from a texture; tone_v2.wgsl calls this.
fn resolve_shadow_correction(enc: vec3<f32>) -> f32 {
    let p = clamp(enc, vec3<f32>(0.0), vec3<f32>(1.0)) * 32.0;
    let b = floor(p);
    let f = p - b;
    let i0 = vec3<u32>(b);
    let i1 = min(i0 + vec3<u32>(1u), vec3<u32>(32u));
    let c000 = shadow_correction_table[(i0.z * 33u + i0.y) * 33u + i0.x];
    let c100 = shadow_correction_table[(i0.z * 33u + i0.y) * 33u + i1.x];
    let c010 = shadow_correction_table[(i0.z * 33u + i1.y) * 33u + i0.x];
    let c110 = shadow_correction_table[(i0.z * 33u + i1.y) * 33u + i1.x];
    let c001 = shadow_correction_table[(i1.z * 33u + i0.y) * 33u + i0.x];
    let c101 = shadow_correction_table[(i1.z * 33u + i0.y) * 33u + i1.x];
    let c011 = shadow_correction_table[(i1.z * 33u + i1.y) * 33u + i0.x];
    let c111 = shadow_correction_table[(i1.z * 33u + i1.y) * 33u + i1.x];
    let c00 = mix(c000, c100, f.x);
    let c10 = mix(c010, c110, f.x);
    let c01 = mix(c001, c101, f.x);
    let c11 = mix(c011, c111, f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= parameters.modes.z { return; }
    let pixel = source[id.x];
    let linear = vec3<f32>(decode_component(pixel.r, parameters.modes.x),
        decode_component(pixel.g, parameters.modes.x), decode_component(pixel.b, parameters.modes.x));
    let working = parameters.source_to_work * linear;
    // Where this pixel is: the pass walks the image as a flat list.
    let index = parameters.frame.z + id.x;
    let width = max(parameters.frame.x, 1u);
    let position = vec2<f32>(f32(index % width), f32(index / width)) + vec2<f32>(0.5);
    let dims = vec2<f32>(f32(width), f32(max(parameters.frame.y, 1u)));
    // The unedited neighbourhood the Basic tone controls read; without one
    // bound they see the pixel itself, in the encoding they expect.
    var tonal: vec3<f32>;
    var structure: vec3<f32>;
    if parameters.basic_flags.z == 1u {
        tonal = neighbourhood[id.x * 2u].rgb;
        structure = neighbourhood[id.x * 2u + 1u].rgb;
    } else {
        let own = max(parameters.work_to_output * working, vec3<f32>(0.0));
        tonal = select(linear_to_srgb_extended(own), own, parameters.basic_flags.y == 1u);
        structure = tonal;
    }
    let graded = film_saturation(vignette(grade(centre(calibrate(working), position, dims), tonal, structure), position, dims));
    // A captured transform replaces the whole rendering step, encode included,
    // and reads working values directly: its domain is the working space.
    let scene = look_scene(graded);
    var encoded: vec3<f32>;
    if parameters.modes.y == 6u {
        encoded = render_captured(scene);
    } else if parameters.modes.y >= 7u {
        encoded = render_previous(parameters.work_to_output * scene, parameters.modes.y);
    } else {
        let display = render_output(parameters.work_to_output * scene, parameters.modes.y);
        encoded = vec3<f32>(encode_srgb(display.r), encode_srgb(display.g), encode_srgb(display.b));
    }
    encoded = look_display(encoded, graded);
    encoded = film_grain(encoded, position);
    if parameters.modes.w == 1u {
        results[id.x*3u] = vec4<f32>(working, pixel.a);
        results[id.x*3u+1u] = vec4<f32>(graded, pixel.a);
        results[id.x*3u+2u] = vec4<f32>(encoded, pixel.a);
    } else if parameters.modes.w == 2u {
        // Graded only: what a local adjustment consumes, at a third of the
        // readback of a full capture.
        results[id.x] = vec4<f32>(graded, pixel.a);
    } else {
        results[id.x] = vec4<f32>(encoded, pixel.a);
    }
}

// The previous engine's tone mappers, on linear sRGB, after its gamut
// safety step; each returns display-encoded sRGB. Its Basic mapper had a RAW
// path and a display path, chosen here as there by the kind of source.
fn render_previous(linear: vec3<f32>, mode: u32) -> vec3<f32> {
    let c = compress_gamut_soft(linear);
    var out: vec3<f32>;
    if mode == 9u {
        out = linear_to_srgb(tonemap_filmic(c));
    } else if mode == 8u {
        out = agx_full_transform_with(c, parameters.agx_to, parameters.agx_from);
    } else if parameters.basic_flags.y == 1u {
        out = basic_raw_rendering(c);
    } else {
        out = linear_to_srgb(c);
    }
    return clamp(out, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Vignette as exposure in the working space, so a lightened corner rolls off
// through the output transform like any other bright value. The shape is
// the previous engine's: a radial profile normalised so the extreme corner
// is exactly one, beginning at the midpoint and softened by the feather.
fn vignette(rgb: vec3<f32>, position: vec2<f32>, dims: vec2<f32>) -> vec3<f32> {
    let v = parameters.effects[0];
    if v.x == 0.0 { return rgb; }
    let uv = (position / dims - vec2<f32>(0.5)) * 2.0;
    let shaped = sign(uv) * pow(abs(uv), vec2<f32>(1.0 - v.z));
    let aspect = dims.y / dims.x;
    let reach = length(shaped * vec2<f32>(1.0, aspect)) / max(length(vec2<f32>(1.0, aspect)), 1e-4);
    let start = v.y * 0.6;
    let falloff = pow(clamp((reach - start) / max(1.0 - start, 1e-4), 0.0, 1.0), 1.0 + v.w * 2.0);
    let stops = v.x * (1.6 + 2.4 * abs(v.x));
    return rgb * exp2(stops * falloff);
}

// The previous engine's camera calibration, in the linear
// sRGB primaries it was defined in: a hue matrix that leans each primary
// toward its neighbours, saturation weighted by how much of each primary a
// colour holds, and a green-magenta tint that fades out above the shadows.
fn calibrate(working: vec3<f32>) -> vec3<f32> {
    let a = parameters.calibration[0];
    let b = parameters.calibration[1];
    if b.w == 0.0 { return working; }
    let h_r = a.y;
    let h_g = a.w;
    let h_b = b.y;
    let hue = mat3x3<f32>(
        vec3<f32>(1.0 - abs(h_r), max(0.0, h_r), max(0.0, -h_r)),
        vec3<f32>(max(0.0, -h_g), 1.0 - abs(h_g), max(0.0, h_g)),
        vec3<f32>(max(0.0, h_b), max(0.0, -h_b), 1.0 - abs(h_b)));
    // Normalised so each row sums to one: the previous engine's matrix
    // moved the primary but also tinted neutrals (a red hue shift turned
    // grey green); here white stays white and the primary still moves.
    let rows = hue * vec3<f32>(1.0);
    var c = (hue * (parameters.work_to_output * working)) / rows;
    let weights = vec3<f32>(0.2126, 0.7152, 0.0722);
    let luma = dot(max(c, vec3<f32>(0.0)), weights);
    let sum = c.r + c.g + c.b;
    var share = vec3<f32>(0.0);
    if sum > 0.001 { share = c / sum; }
    c += (c - vec3<f32>(luma)) * dot(share, vec3<f32>(a.z, b.x, b.z));
    if abs(a.x) > 0.001 {
        let shadow = 1.0 - smoothstep(0.0, 0.3, dot(max(c, vec3<f32>(0.0)), weights));
        let tint = vec3<f32>(1.0 + a.x * 0.25, 1.0 - a.x * 0.25, 1.0 + a.x * 0.25);
        c = mix(c, c * tint, shadow);
    }
    return parameters.srgb_to_work * c;
}

// The previous engine's Centre, pointwise half: a radial weight that is one
// in the middle and falls to zero past the frame's edge, as there. The
// middle gains up to a fifth of a stop and chroma; the edges lose chroma.
// (Its other half, local contrast, runs with detail on the CPU.)
fn centre_weight(position: vec2<f32>, dims: vec2<f32>) -> f32 {
    let aspect = dims.y / dims.x;
    let uv = (position / dims - vec2<f32>(0.5)) * 2.0;
    let d = length(uv * vec2<f32>(1.0, aspect)) * 0.5;
    return 1.0 - smoothstep(0.4 - 0.375, 0.4 + 0.375, d);
}

fn centre(rgb: vec3<f32>, position: vec2<f32>, dims: vec2<f32>) -> vec3<f32> {
    let c = parameters.effects[2].x;
    if c == 0.0 { return rgb; }
    let m = centre_weight(position, dims);
    let lifted = rgb * exp2(m * c * 0.5);
    let chroma = max(1.0 + m * c * 0.7 - (1.0 - m) * c * 0.8, 0.0);
    let lab = to_lab_work(lifted);
    return from_lab_work(vec3<f32>(lab.x, lab.yz * chroma));
}

// Film saturation, as the previous engine had it: chroma eased toward
// neutral in deep shadows and near white, in Oklab so hue never moves.
fn film_saturation(rgb: vec3<f32>) -> vec3<f32> {
    let amount = parameters.effects[2].y;
    if amount <= 0.001 { return rgb; }
    let lab = to_lab_work(rgb);
    let l = clamp(lab.x, 0.0, 1.2);
    let highlight = smoothstep(0.78, 1.05, l);
    let shadow = 1.0 - smoothstep(0.05, 0.28, l);
    let desat = clamp((highlight * 0.75 + shadow * 0.55) * amount, 0.0, 0.9);
    return from_lab_work(vec3<f32>(lab.x, lab.yz * (1.0 - desat)));
}

fn grain_hash(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn grain_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let g = array<vec2<f32>, 4>(
        vec2<f32>(grain_hash(i), grain_hash(i + vec2<f32>(11.0, 37.0))) * 2.0 - 1.0,
        vec2<f32>(grain_hash(i + vec2<f32>(1.0, 0.0)), grain_hash(i + vec2<f32>(12.0, 37.0))) * 2.0 - 1.0,
        vec2<f32>(grain_hash(i + vec2<f32>(0.0, 1.0)), grain_hash(i + vec2<f32>(11.0, 38.0))) * 2.0 - 1.0,
        vec2<f32>(grain_hash(i + vec2<f32>(1.0, 1.0)), grain_hash(i + vec2<f32>(12.0, 38.0))) * 2.0 - 1.0);
    let bottom = mix(dot(g[0], f), dot(g[1], f - vec2<f32>(1.0, 0.0)), u.x);
    let top = mix(dot(g[2], f - vec2<f32>(0.0, 1.0)), dot(g[3], f - vec2<f32>(1.0, 1.0)), u.x);
    return mix(bottom, top, u.y);
}

// Grain on the finished, display-encoded image, as film grain is seen: in
// full-resolution coordinates so it keeps its size relative to the
// photograph at any preview scale, strongest in the midtones.
fn film_grain(encoded: vec3<f32>, position: vec2<f32>) -> vec3<f32> {
    let g = parameters.effects[1];
    if g.x <= 0.0 { return encoded; }
    let coord = position / g.w;
    let frequency = 1.0 / max(g.y, 0.1);
    let luma = max(dot(encoded, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0);
    let midtones = smoothstep(0.0, 0.15, luma) * (1.0 - smoothstep(0.6, 1.0, luma));
    let fine = grain_noise(coord * frequency);
    let rough = grain_noise(coord * frequency * 0.6 + vec2<f32>(5.2, 1.3));
    return encoded + vec3<f32>(mix(fine, rough, g.z) * g.x * midtones);
}
