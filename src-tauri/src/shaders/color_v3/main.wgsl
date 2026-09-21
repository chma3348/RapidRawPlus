struct Parameters {
    source_to_work: mat3x3<f32>,
    work_to_output: mat3x3<f32>,
    modes: vec4<u32>,
    flags: vec4<u32>,
    work_to_lms: mat3x3<f32>,
    lms_to_work: mat3x3<f32>,
    white_balance: mat3x3<f32>,
    tone: vec4<f32>,
    zones: vec4<f32>,
    color: vec4<f32>,
    bands: array<vec4<f32>,8>,
    grading: array<vec4<f32>,4>,
    curve: array<vec4<f32>,5>,
    range_center: array<vec4<f32>,8>,
    range_width: array<vec4<f32>,8>,
    range_adjustment: array<vec4<f32>,8>,
    frame: vec4<u32>,
    effects: array<vec4<f32>,2>,
    channel_curves: array<vec4<f32>,15>,
    curve_flags: vec4<u32>,
    look: vec4<f32>,
    look_flags: vec4<u32>,
    work_to_look: mat3x3<f32>,
}
@group(0) @binding(0) var<storage, read> source: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> results: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> parameters: Parameters;
// One dummy entry when no captured transform is in use: the layout is fixed.
@group(0) @binding(3) var<storage, read> cube: array<vec4<f32>>;
// A creative LUT, likewise one dummy entry when there is none.
@group(0) @binding(4) var<storage, read> look_table: array<vec4<f32>>;

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
    let graded = vignette(grade(working), position, dims);
    // A captured transform replaces the whole rendering step, encode included,
    // and reads working values directly: its domain is the working space.
    let scene = look_scene(graded);
    var encoded: vec3<f32>;
    if parameters.modes.y == 6u {
        encoded = render_captured(scene);
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
