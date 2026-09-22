fn cube_root(v: vec3<f32>) -> vec3<f32> { return sign(v)*pow(abs(v),vec3<f32>(1.0/3.0)); }
// Oklab's cone-space -> Lab matrix and its inverse. These describe Oklab
// itself, so they hold whatever RGB basis the cone values came from; only the
// RGB -> LMS half depends on the primaries.
fn lms_to_lab(lms: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(lms,vec3<f32>(0.2104542553,0.7936177850,-0.0040720468)),dot(lms,vec3<f32>(1.9779984951,-2.4285922050,0.4505937099)),dot(lms,vec3<f32>(0.0259040371,0.7827717662,-0.8086757660)));
}
fn lab_to_lms(lab:vec3<f32>) -> vec3<f32> {
    let v=vec3<f32>(lab.x+0.3963377774*lab.y+0.2158037573*lab.z,lab.x-0.1055613458*lab.y-0.0638541728*lab.z,lab.x-0.0894841775*lab.y-1.2914855480*lab.z);
    return v*v*v;
}
// Working RGB straight to Oklab. Routing through sRGB first gives the same
// numbers — the matrices compose — so this is the same transform written
// without the detour, and two mat3 products per pixel cheaper.
fn to_lab_work(rgb: vec3<f32>) -> vec3<f32> {
    return lms_to_lab(cube_root(parameters.work_to_lms*rgb));
}
fn from_lab_work(lab: vec3<f32>) -> vec3<f32> {
    return parameters.lms_to_work*lab_to_lms(lab);
}
// The output stage works in the destination's own primaries, so it keeps the
// fixed linear-sRGB cone matrix rather than the working-space one.
fn to_lab_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return lms_to_lab(cube_root(vec3<f32>(dot(rgb,vec3<f32>(0.4122214708,0.5363325363,0.0514459929)),dot(rgb,vec3<f32>(0.2119034982,0.6806995451,0.1073969566)),dot(rgb,vec3<f32>(0.0883024619,0.2817188376,0.6299787005)))));
}
fn from_lab_srgb(lab:vec3<f32>) -> vec3<f32> {
    let t=lab_to_lms(lab);
    return vec3<f32>(dot(t,vec3<f32>(4.0767416621,-3.3077115913,0.2309699292)),dot(t,vec3<f32>(-1.2684380046,2.6097574011,-0.3413193965)),dot(t,vec3<f32>(-0.0041960863,-0.7034186147,1.7076147010)));
}
fn tone_curve(y:f32) -> f32 {
    let x=log2(1.0+16.0*y)/log2(17.0);
    var mapped=0.0;
    if x>=1.0 {
        // Continue the terminal tangent, never clamp scene highlights.
        mapped=1.0+(x-1.0)*parameters.curve[4].y;
    } else {
        let i=min(u32(x*4.0),3u);
        let t=x*4.0-f32(i);
        let a=parameters.curve[i]; let b=parameters.curve[i+1u];
        mapped=(2.0*t*t*t-3.0*t*t+1.0)*a.x+(t*t*t-2.0*t*t+t)*a.y*0.25+(-2.0*t*t*t+3.0*t*t)*b.x+(t*t*t-t*t)*b.y*0.25;
    }
    return (exp2(mapped*log2(17.0))-1.0)/16.0;
}
// One channel through its curve, in DaVinci Intermediate: the encoding
// Resolve's curves act in, so a channel curve here bends the same values a
// Resolve curve would. Monotone cubic between the knots; beyond the ends the
// end tangents continue, so values outside the encoding are not clamped.
fn channel_curve(v: f32, channel: u32) -> f32 {
    let base = channel * 5u;
    let x = encode_intermediate(v);
    var y = 0.0;
    if x >= 1.0 {
        y = 1.0 + (x - 1.0) * parameters.channel_curves[base + 4u].y;
    } else if x <= 0.0 {
        y = x * parameters.channel_curves[base].y;
    } else {
        let i = min(u32(x * 4.0), 3u);
        let t = x * 4.0 - f32(i);
        let a = parameters.channel_curves[base + i];
        let b = parameters.channel_curves[base + i + 1u];
        y = (2.0*t*t*t - 3.0*t*t + 1.0) * a.x + (t*t*t - 2.0*t*t + t) * a.y * 0.25
            + (-2.0*t*t*t + 3.0*t*t) * b.x + (t*t*t - t*t) * b.y * 0.25;
    }
    return decode_component(y, 2u);
}

// The Basic panel, shared with the previous engine and run through its own
// functions (tone_v2.wgsl) in its order — brightness, then contrast, shadows,
// whites and blacks, then highlights — on linear sRGB, the space they were
// written and tuned in.
fn basic_v2(rgb: vec3<f32>, tonal: vec3<f32>, structure: vec3<f32>) -> vec3<f32> {
    if parameters.basic_flags.x == 0u { return rgb; }
    let a = parameters.basic[0];
    let b = parameters.basic[1];
    let raw = parameters.basic_flags.y;
    var c = parameters.work_to_output * rgb;
    c = apply_filmic_exposure(c, a.x);
    c = apply_tonal_adjustments_v2(c, structure, raw, a.y, b.x, b.y, b.z, a.z);
    c = apply_highlights_adjustment_v2(c, tonal, structure, raw, a.w);
    return parameters.srgb_to_work * c;
}

fn grade(input:vec3<f32>, tonal:vec3<f32>, structure:vec3<f32>) -> vec3<f32> {
    if parameters.flags.x == 0u {return input;}
    var rgb=basic_v2(parameters.white_balance*input*parameters.tone.z, tonal, structure);
    let y=dot(rgb,vec3<f32>(0.27411851,0.87363190,-0.14775041));
    // DWG's blue coefficient is negative, so a non-physical pixel can land at
    // or below zero luminance. Fade the curve out across the bottom of the
    // floor instead of switching it off at a threshold, which used to put a
    // hard edge between two neighbouring near-black pixels.
    let floor=0.0001;
    let lit=max(y,0.0);
    var scale=1.0;
    // Skipped outright when the curve is neutral: the divide below is not
    // required to return exactly one, and exposure has to stay an exact
    // scaling.
    if parameters.flags.y == 1u {
        var t=max(lit,floor);
        t=tone_curve(t);
        scale=mix(1.0,t/max(lit,floor),smoothstep(0.0,floor,lit));
    }
    rgb*=scale;
    var graded_y=y*scale;
    if parameters.curve_flags.x == 1u {
        rgb = vec3<f32>(channel_curve(rgb.r, 0u), channel_curve(rgb.g, 1u), channel_curve(rgb.b, 2u));
        // The wheels read the same image the ranges select from: after the
        // curves, which move luminance too.
        graded_y = dot(rgb, vec3<f32>(0.27411851, 0.87363190, -0.14775041));
    }
    if parameters.flags.z == 0u { return rgb; }
    var lab=to_lab_work(rgb);
    let chroma=length(lab.yz);
    let angle=atan2(lab.z,lab.y);
    let protect=smoothstep(0.005,0.05,chroma/max(abs(lab.x),0.01));
    let centers=array<f32,8>(29.0,53.0,110.0,142.0,195.0,264.0,300.0,328.0);
    var delta=vec3<f32>(0.0);
    var total=0.0;
    for(var i=0u;i<8u;i++) {
        let dist=abs(atan2(sin(angle-radians(centers[i])),cos(angle-radians(centers[i]))));
        let w=1.0-smoothstep(0.0,radians(65.0),dist);
        delta+=parameters.bands[i].xyz*w; total+=w;
    }
    delta=delta/max(total,0.00001)*protect;
    // All ranges sample the same pre-selective color: order-independent edits.
    // Normalize overlaps so adding ranges cannot multiply their strength.
    var custom=vec3<f32>(0.0); var coverage=0.0;
    for(var i=0u;i<8u;i++) {
        let center=parameters.range_center[i];
        let width=parameters.range_width[i];
        let hue_distance=abs(atan2(sin(angle-center.x),cos(angle-center.x)));
        let distance=vec3<f32>(hue_distance,abs(chroma-center.y),abs(lab.x-center.z))/width.xyz;
        let falloff=vec3<f32>(1.0)-smoothstep(vec3<f32>(0.0),vec3<f32>(1.0),distance);
        let w=falloff.x*falloff.y*falloff.z*center.w;
        custom+=parameters.range_adjustment[i].xyz*w; coverage+=w;
    }
    delta+=custom/max(1.0,coverage)*protect;
    let h=angle+parameters.color.z*protect+delta.x;
    let vibrance=exp2(parameters.color.y*(1.0-smoothstep(0.0,0.4,chroma/max(abs(lab.x),0.01))));
    let c=chroma*parameters.color.x*vibrance*exp2(delta.y);
    lab=vec3<f32>(lab.x*exp2(delta.z*0.5),cos(h)*c,sin(h)*c);
    // Wheels key off the tone-mapped luminance, the same image the custom
    // ranges select from and the one on screen: set exposure and contrast
    // first, then tint the shadows you can actually see.
    let pos=max(graded_y,0.0);
    let shadow=1.0-smoothstep(0.02,0.4,pos);
    let highlight=smoothstep(0.25,1.2,pos);
    let mid=max(0.0,1.0-shadow-highlight);
    let g=parameters.grading[0]+parameters.grading[1]*shadow+parameters.grading[2]*mid+parameters.grading[3]*highlight;
    // Tinting pure black would colour the parts of the frame that carry no
    // colour, so the chroma terms fade out there. Lightness must not: lifting
    // black off zero is the main thing a shadow wheel is for.
    let tint_gate=smoothstep(0.0,0.02,abs(lab.x));
    lab+=vec3<f32>(g.z,g.x*tint_gate,g.y*tint_gate);
    return from_lab_work(lab);
}
