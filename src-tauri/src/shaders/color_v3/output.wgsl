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

fn cube_at(r: u32, g: u32, b: u32) -> vec3<f32> {
    let size = parameters.flags.w;
    return cube[r + (g + b * size) * size].rgb;
}

// Tetrahedral rather than trilinear: it is what LUT engines use, it keeps the
// neutral axis exact, and on a transform this smooth it costs four lookups
// instead of eight.
fn cube_lookup(coordinate: vec3<f32>) -> vec3<f32> {
    let last = f32(parameters.flags.w - 1u);
    let scaled = clamp(coordinate, vec3<f32>(0.0), vec3<f32>(1.0)) * last;
    let base = min(floor(scaled), vec3<f32>(last - 1.0));
    let f = scaled - base;
    let i = vec3<u32>(base);
    let c000 = cube_at(i.x, i.y, i.z);
    let c111 = cube_at(i.x + 1u, i.y + 1u, i.z + 1u);
    if f.x >= f.y {
        if f.y >= f.z {
            let c100 = cube_at(i.x + 1u, i.y, i.z);
            let c110 = cube_at(i.x + 1u, i.y + 1u, i.z);
            return c000 + (c100 - c000) * f.x + (c110 - c100) * f.y + (c111 - c110) * f.z;
        }
        if f.x >= f.z {
            let c100 = cube_at(i.x + 1u, i.y, i.z);
            let c101 = cube_at(i.x + 1u, i.y, i.z + 1u);
            return c000 + (c100 - c000) * f.x + (c111 - c101) * f.y + (c101 - c100) * f.z;
        }
        let c001 = cube_at(i.x, i.y, i.z + 1u);
        let c101 = cube_at(i.x + 1u, i.y, i.z + 1u);
        return c000 + (c101 - c001) * f.x + (c111 - c101) * f.y + (c001 - c000) * f.z;
    }
    if f.z > f.y {
        let c001 = cube_at(i.x, i.y, i.z + 1u);
        let c011 = cube_at(i.x, i.y + 1u, i.z + 1u);
        return c000 + (c111 - c011) * f.x + (c011 - c001) * f.y + (c001 - c000) * f.z;
    }
    if f.z > f.x {
        let c010 = cube_at(i.x, i.y + 1u, i.z);
        let c011 = cube_at(i.x, i.y + 1u, i.z + 1u);
        return c000 + (c111 - c011) * f.x + (c010 - c000) * f.y + (c011 - c010) * f.z;
    }
    let c010 = cube_at(i.x, i.y + 1u, i.z);
    let c110 = cube_at(i.x + 1u, i.y + 1u, i.z);
    return c000 + (c110 - c010) * f.x + (c010 - c000) * f.y + (c111 - c110) * f.z;
}

/// Working linear DWG straight to display-encoded output.
fn render_captured(working: vec3<f32>) -> vec3<f32> {
    let logged = vec3<f32>(
        encode_intermediate(working.r),
        encode_intermediate(working.g),
        encode_intermediate(working.b));
    return clamp(cube_lookup(logged), vec3<f32>(0.0), vec3<f32>(1.0));
}
