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
}
@group(0) @binding(0) var<storage, read> source: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> results: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> parameters: Parameters;
// One dummy entry when no captured transform is in use: the layout is fixed.
@group(0) @binding(3) var<storage, read> cube: array<vec4<f32>>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= parameters.modes.z { return; }
    let pixel = source[id.x];
    let linear = vec3<f32>(decode_component(pixel.r, parameters.modes.x),
        decode_component(pixel.g, parameters.modes.x), decode_component(pixel.b, parameters.modes.x));
    let working = parameters.source_to_work * linear;
    let graded = grade(working);
    // A captured transform replaces the whole rendering step, encode included,
    // and reads working values directly: its domain is the working space.
    var encoded: vec3<f32>;
    if parameters.modes.y == 6u {
        encoded = render_captured(graded);
    } else {
        let display = render_output(parameters.work_to_output * graded, parameters.modes.y);
        encoded = vec3<f32>(encode_srgb(display.r), encode_srgb(display.g), encode_srgb(display.b));
    }
    if parameters.modes.w == 1u {
        results[id.x*3u] = vec4<f32>(working, pixel.a);
        results[id.x*3u+1u] = vec4<f32>(graded, pixel.a);
        results[id.x*3u+2u] = vec4<f32>(encoded, pixel.a);
    } else {
        results[id.x] = vec4<f32>(encoded, pixel.a);
    }
}
