// Provisional SDR transform for architecture testing, not Resolve emulation.
// Preserve ratios through the shoulder; pull negative channels toward neutral.
fn output_linear(input: vec3<f32>, scene: bool) -> vec3<f32> {
    var rgb = input;
    if scene {
        let low = min(rgb.r, min(rgb.g, rgb.b));
        if low < 0.0 {
            let y = max(dot(rgb, vec3<f32>(0.212639, 0.715169, 0.072192)), 0.0);
            if y <= 0.0 { return vec3<f32>(0.0); }
            rgb = mix(rgb, vec3<f32>(y), -low / (y - low));
        }
        let peak = max(rgb.r, max(rgb.g, rgb.b));
        if peak > 0.75 {
            let mapped = 0.75 + 0.25 * (1.0 - exp(-(peak - 0.75) / 0.25));
            rgb *= mapped / peak;
        }
    }
    return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn in_gamut(rgb:vec3<f32>) -> bool {return all(rgb>=vec3<f32>(0.0)) && all(rgb<=vec3<f32>(1.0));}
fn gamut_project(rgb:vec3<f32>) -> vec3<f32> {
    if in_gamut(rgb) {return rgb;}
    // Matrix roundoff at the cube boundary is not genuine out-of-gamut
    // chroma. Projecting it introduces a visible discontinuity at primaries.
    if all(rgb>=vec3<f32>(-0.000002)) && all(rgb<=vec3<f32>(1.000002)) {
        return clamp(rgb,vec3<f32>(0.0),vec3<f32>(1.0));
    }
    let lab=to_lab_srgb(rgb);
    if lab.x<=0.0 {return vec3<f32>(0.0);}
    if lab.x>=1.0 {return vec3<f32>(1.0);}
    var low=0.0; var high=1.0;
    for(var i=0u;i<14u;i++) {
        let mid=(low+high)*0.5;
        if in_gamut(from_lab_srgb(vec3<f32>(lab.x,lab.yz*mid))) {low=mid;} else {high=mid;}
    }
    return clamp(from_lab_srgb(vec3<f32>(lab.x,lab.yz*low)),vec3<f32>(0.0),vec3<f32>(1.0));
}
// Projecting every out-of-gamut colour onto the gamut shell makes colours that
// were far apart arrive identical, which is what flattens saturated areas. This
// compresses instead: chroma below `threshold` of the boundary is untouched,
// the rest is squeezed into the band above it and approaches the boundary
// without ever reaching it, so order and separation survive. The cost is that
// colours between the threshold and the boundary — in gamut, but only just —
// lose a little chroma. That is the trade, and it is why this is a separate
// rendering revision rather than a change to `gamut_project`.
fn gamut_compress(rgb:vec3<f32>) -> vec3<f32> {
    let threshold=0.85;
    let lab=to_lab_srgb(rgb);
    if lab.x<=0.0 {return vec3<f32>(0.0);}
    // Above the cube's lightness there is no colour but white to map to.
    if lab.x>=1.0 {return vec3<f32>(1.0);}
    // Comfortably inside: left exactly alone, and no roundoff epsilon needed
    // because the compression below meets this branch continuously.
    if in_gamut(from_lab_srgb(vec3<f32>(lab.x,lab.yz/threshold))) {
        return clamp(rgb,vec3<f32>(0.0),vec3<f32>(1.0));
    }
    // The cube is convex and the neutral axis is inside it, so the in-gamut
    // chroma scales along this ray form one interval and bisection is sound.
    var low=0.0; var high=1.0/threshold;
    for(var i=0u;i<14u;i++) {
        let mid=(low+high)*0.5;
        if in_gamut(from_lab_srgb(vec3<f32>(lab.x,lab.yz*mid))) {low=mid;} else {high=mid;}
    }
    let boundary=max(low,0.000001);
    let over=1.0/boundary;
    // u/(1+u) rather than 1-exp(-u): both meet the threshold with slope one,
    // but the rational tail falls off as 1/u^2 instead of exponentially, so
    // colours far outside the gamut keep a usable amount of separation.
    let u=(over-threshold)/(1.0-threshold);
    let mapped=threshold+(1.0-threshold)*u/(1.0+u);
    return clamp(from_lab_srgb(vec3<f32>(lab.x,lab.yz*mapped*boundary)),vec3<f32>(0.0),vec3<f32>(1.0));
}
fn render_output(input:vec3<f32>,mode:u32) -> vec3<f32> {
    if mode<=1u {return output_linear(input,mode==1u);}
    var rgb=input;
    if mode==2u||mode==4u {
        let y=dot(rgb,vec3<f32>(0.2126390059,0.7151686788,0.0721923154));
        if y<=0.0 {return vec3<f32>(0.0);}
        if y>0.6 {
            let delta=y-0.6;
            let mapped=0.6+delta/(1.0+delta/0.4);
            rgb*=mapped/y;
        }
    }
    if mode>=4u {return gamut_compress(rgb);}
    return gamut_project(rgb);
}

// --- Captured rendering transform -------------------------------------------
// The cube's domain is DaVinci Intermediate and its output is already
// display-encoded, so nothing encodes after it.
fn encode_intermediate(v: f32) -> f32 {
    if v <= 0.00262409 { return v * 10.44426855; }
    return (log2(max(v + 0.0075, 1e-10)) + 7.0) * 0.07329248;
}

// Tetrahedral rather than trilinear: it is what LUT engines use, it keeps the
// neutral axis exact, and it reads four lattice points instead of eight. The
// weights are computed once and shared by both lattices — the captured
// transform and a creative LUT — since WGSL cannot pass a storage buffer to a
// function.
struct Tetra {
    corners: array<vec3<u32>, 4>,
    weights: vec4<f32>,
}

fn tetra(coordinate: vec3<f32>, size: u32) -> Tetra {
    let last = f32(size - 1u);
    let scaled = clamp(coordinate, vec3<f32>(0.0), vec3<f32>(1.0)) * last;
    let base = min(floor(scaled), vec3<f32>(last - 1.0));
    let f = scaled - base;
    let i = vec3<u32>(base);
    let x = vec3<u32>(1u, 0u, 0u);
    let y = vec3<u32>(0u, 1u, 0u);
    let z = vec3<u32>(0u, 0u, 1u);
    // The first and second axes to step along, and the fractions sorted.
    var e1: vec3<u32>; var e2: vec3<u32>; var s: vec3<f32>;
    if f.x >= f.y {
        if f.y >= f.z { e1 = x; e2 = x + y; s = f.xyz; }
        else if f.x >= f.z { e1 = x; e2 = x + z; s = f.xzy; }
        else { e1 = z; e2 = z + x; s = f.zxy; }
    } else {
        if f.z > f.y { e1 = z; e2 = z + y; s = f.zyx; }
        else if f.z > f.x { e1 = y; e2 = y + z; s = f.yzx; }
        else { e1 = y; e2 = y + x; s = f.yxz; }
    }
    var t: Tetra;
    t.corners = array<vec3<u32>, 4>(i, i + e1, i + e2, i + vec3<u32>(1u));
    t.weights = vec4<f32>(1.0 - s.x, s.x - s.y, s.y - s.z, s.z);
    return t;
}

fn cube_at(c: vec3<u32>) -> vec3<f32> {
    let size = parameters.flags.w;
    return cube[c.x + (c.y + c.z * size) * size].rgb;
}

fn cube_lookup(coordinate: vec3<f32>) -> vec3<f32> {
    let t = tetra(coordinate, parameters.flags.w);
    return cube_at(t.corners[0]) * t.weights.x + cube_at(t.corners[1]) * t.weights.y
        + cube_at(t.corners[2]) * t.weights.z + cube_at(t.corners[3]) * t.weights.w;
}

fn look_at(c: vec3<u32>) -> vec3<f32> {
    let size = parameters.look_flags.x;
    return look_table[c.x + (c.y + c.z * size) * size].rgb;
}

fn look_lookup(coordinate: vec3<f32>) -> vec3<f32> {
    let t = tetra(coordinate, parameters.look_flags.x);
    return look_at(t.corners[0]) * t.weights.x + look_at(t.corners[1]) * t.weights.y
        + look_at(t.corners[2]) * t.weights.z + look_at(t.corners[3]) * t.weights.w;
}

fn decode_intermediate(v: f32) -> f32 { return decode_component(v, 2u); }

// Fujifilm F-Log2 (the curve is shared by F-Log2 C); see flog2c.rs.
fn flog2_encode(x: f32) -> f32 {
    let t = max(x, 0.0);
    if t >= 0.000889 { return 0.245281 * log2(5.555556 * t + 0.064829) * 0.30102999566 + 0.384316; }
    return 8.799461 * t + 0.092864;
}

/// A look made for DaVinci Intermediate: on scene data, before rendering.
fn look_scene(graded: vec3<f32>) -> vec3<f32> {
    if parameters.look_flags.x == 0u || parameters.look_flags.y != 3u { return graded; }
    let logged = vec3<f32>(encode_intermediate(graded.r), encode_intermediate(graded.g),
        encode_intermediate(graded.b));
    let looked = look_lookup(logged);
    let linear = vec3<f32>(decode_intermediate(looked.r), decode_intermediate(looked.g),
        decode_intermediate(looked.b));
    return mix(graded, linear, parameters.look.x);
}

/// A look on display code values, or a film simulation in place of the
/// rendering; either way the result is display-encoded.
fn look_display(encoded: vec3<f32>, graded: vec3<f32>) -> vec3<f32> {
    let space = parameters.look_flags.y;
    if parameters.look_flags.x == 0u || space == 3u { return encoded; }
    var looked: vec3<f32>;
    if space == 2u {
        let camera = parameters.work_to_look * (max(graded, vec3<f32>(0.0)) * parameters.look.y);
        looked = look_lookup(vec3<f32>(flog2_encode(camera.r), flog2_encode(camera.g),
            flog2_encode(camera.b)));
    } else {
        looked = look_lookup(encoded);
    }
    return mix(encoded, clamp(looked, vec3<f32>(0.0), vec3<f32>(1.0)), parameters.look.x);
}

/// Working linear DWG straight to display-encoded output.
fn render_captured(working: vec3<f32>) -> vec3<f32> {
    let logged = vec3<f32>(
        encode_intermediate(working.r),
        encode_intermediate(working.g),
        encode_intermediate(working.b));
    return clamp(cube_lookup(logged), vec3<f32>(0.0), vec3<f32>(1.0));
}
