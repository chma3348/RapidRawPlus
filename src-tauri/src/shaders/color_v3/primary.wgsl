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
// functions (tone_v2.wgsl) in its order — EV shift, brightness, then
// contrast, shadows, whites and blacks, then highlights — on linear sRGB, the
// space they were written and tuned in. `rgb` arrives before the EV shift.
//
// Where they run depends on what the previous engine gave them. A RAW file's
// scene values: here too. A rendered picture's display values: here, the
// display values Resolve's rendering makes of the scene data, and back
// through Resolve's input transform afterwards. Those controls were never
// given values past white for such a picture — its brightness curve, handed
// one, breaks into bands — and at neutral the round trip is skipped, so an
// unedited picture is untouched.
// The four tone zones: Blacks, Shadows, Highlights, Whites, with Lightroom's
// measured strength (tone_zones_table.rs, fitted by tools/fit_lighting.py;
// docs/tone-zones.md). Each slider moves its part of the tonal range, judged
// on the tonal key: the working-space luminance in DaVinci Intermediate, a
// mix of the pixel's own and an edge-aware regional average of the unedited
// picture (so texture rides along and a dark subject against a bright sky
// lifts without a halo), per zone (ZONE_STYLE in plan.rs). The zones apply
// in order, each on the previous result; every step is monotone, so no
// combination of sliders can reverse tones. A zone moves a pixel by one gain
// on all three channels (hues stay put; where no gain can reach, pure black
// under a Blacks lift, the rest is filled in neutral), or, where Lightroom's
// colour calls for it, each channel along the zone's curve on its own.
fn shadow_key(working: vec3<f32>) -> f32 {
    let y = max(dot(working, vec3<f32>(0.27411851, 0.87363190, -0.14775041)), 0.0);
    if y <= 0.00262409 { return y * 10.44426855; }
    return (log2(y + 0.0075) + 7.0) * 0.07329248;
}
// One zone's offset at key `k`, for a slider in -1..1. Strength follows the
// tables at 50 and 100 (it doesn't grow evenly with the slider, as in
// Lightroom), straight between them and toward zero.
fn zone_offset(k: f32, zone: u32, amount: f32) -> f32 {
    if amount == 0.0 { return 0.0; }
    // Read where this photo's tones put it (PhotoTones in plan.rs).
    let x = clamp(k + parameters.zone_adapt[0][zone], 0.0, 1.0) * 64.0;
    // At the top knot i stays 63 and the fraction reaches one.
    let i = min(u32(floor(x)), 63u);
    let f = x - f32(i);
    var full: vec4<f32>;
    var half: vec4<f32>;
    if amount > 0.0 {
        full = mix(parameters.zone_lift[i], parameters.zone_lift[i + 1u], f);
        half = mix(parameters.zone_lift_half[i], parameters.zone_lift_half[i + 1u], f);
    } else {
        full = mix(parameters.zone_cut[i], parameters.zone_cut[i + 1u], f);
        half = mix(parameters.zone_cut_half[i], parameters.zone_cut_half[i + 1u], f);
    }
    let a = abs(amount);
    var strength = parameters.zone_adapt[2][zone];
    if amount > 0.0 { strength = parameters.zone_adapt[1][zone]; }
    if a <= 0.5 { return a * 2.0 * half[zone] * strength; }
    return mix(half[zone], full[zone], a * 2.0 - 1.0) * strength;
}
// The sliders in -1..1. The shared parser hands them over divided by 120
// (Shadows, Highlights), 30 (Whites) and 40 (Blacks).
fn zone_amounts() -> vec4<f32> {
    return clamp(vec4<f32>(parameters.basic[1].z * 0.4, parameters.basic[1].x * 1.2,
        parameters.basic[0].w * 1.2, parameters.basic[1].y * 0.3), vec4<f32>(-1.0), vec4<f32>(1.0));
}
// Contrast: a curve on each channel's DaVinci Intermediate value, the way
// Resolve applies contrast (one curve per channel, so colours gain or lose
// saturation with it, as there), with the strength and shape of
// Lightroom's, measured (tools/adobe_lighting.py). Tables at 50 and 100 on
// both sides; the pivot slides the curve along the tonal range.
fn contrast_offset(x: f32, amount: f32) -> f32 {
    let p = clamp(x - (parameters.basic[0].z - 0.5) * 0.5, 0.0, 1.0) * 64.0;
    let i = min(u32(floor(p)), 63u);
    let row = mix(parameters.contrast[i], parameters.contrast[i + 1u], p - f32(i));
    let a = abs(amount);
    var half = row.x;
    var full = row.y;
    if amount < 0.0 { half = row.z; full = row.w; }
    if a <= 0.5 { return a * 2.0 * half; }
    return mix(half, full, a * 2.0 - 1.0);
}
// Exposure: Lightroom's, measured. Its change for a tone is much the same
// curve on every photo, judged by the tone before exposure: shadows and
// midtones move more than a plain gain would move them, the brightest tones
// less (brightening doesn't wash out the top; darkening holds white near
// white). So the table takes each pixel's unexposed tonal key straight to
// its exposed one (tables at 1 and 2.5 stops; beyond 2.5 a plain gain
// carries on), reached by scaling the exposed pixel: the gain in basic_v2
// supplies only its colour, whatever domain it ran in. Not local, as
// Lightroom's isn't. Then Lightroom's colour: a little less saturation per
// stop brightening, a little more darkening (EXPOSURE_COLOUR in plan.rs).
fn apply_exposure_shape(rgb: vec3<f32>, unexposed: vec3<f32>) -> vec3<f32> {
    let stops = parameters.tone.x;
    if stops == 0.0 { return rgb; }
    // Brightness by weights that are all positive: the working space's own
    // luminance weighs blue negatively, so a saturated blue reads near zero
    // and the scaling below would jump there. Greys read the same either way.
    let weights = vec3<f32>(0.2126, 0.7152, 0.0722);
    let y = dot(max(rgb, vec3<f32>(0.0)), weights);
    if y <= 1e-6 { return rgb; }
    let k = encode_intermediate(dot(max(unexposed, vec3<f32>(0.0)), weights));
    let p = clamp(k, 0.0, 1.0) * 64.0;
    let i = min(u32(floor(p)), 63u);
    let row = mix(parameters.exposure_shape[i], parameters.exposure_shape[i + 1u], p - f32(i));
    let a = abs(stops);
    var one = row.x;
    var more = row.y;
    if stops < 0.0 { one = row.z; more = row.w; }
    var d: f32;
    if a <= 1.0 { d = a * one; } else { d = mix(one, more, min((a - 1.0) / 1.5, 1.0)); }
    var aim = max(decode_intermediate_soft(k + d), 0.0);
    if a > 2.5 { aim = aim * exp2(sign(stops) * (a - 2.5)); }
    let exposed = rgb * (aim / y);
    var per_stop = parameters.exposure_colour.x;
    if stops < 0.0 { per_stop = parameters.exposure_colour.y; }
    if per_stop == 0.0 { return exposed; }
    // Lightroom's colour change levels off: what one stop does, more does too.
    let s = max(1.0 + per_stop * min(a, 1.0), 0.0);
    let logged = vec3<f32>(channel_key(exposed.r), channel_key(exposed.g), channel_key(exposed.b));
    let l = dot(logged, vec3<f32>(0.2126, 0.7152, 0.0722));
    let mixed = vec3<f32>(l) + (logged - vec3<f32>(l)) * s;
    return max(vec3<f32>(decode_intermediate_soft(mixed.r), decode_intermediate_soft(mixed.g), decode_intermediate_soft(mixed.b)), vec3<f32>(0.0));
}
fn apply_contrast(rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(parameters.basic[0].y, -1.0, 1.0);
    if amount == 0.0 { return rgb; }
    var out = rgb;
    for (var c = 0u; c < 3u; c++) {
        let x = encode_intermediate(rgb[c]);
        out[c] = decode_intermediate_soft(x + contrast_offset(x, amount));
    }
    return out;
}

// Resolve's Saturation, measured: a mix toward Rec.709 luma of the DaVinci
// Intermediate log values, by exactly 1 + slider/100 (fitted 0.005, 0.503,
// 1.499, 1.995 at -100, -50, +50, +100 on the chart; 0.3 level residual).
// Greys are untouched; the log-domain mix is why saturated brights darken
// a little as they saturate, as they do in Resolve.
fn resolve_saturation(rgb: vec3<f32>) -> vec3<f32> {
    let s = parameters.color.x;
    if s == 1.0 { return rgb; }
    let logged = vec3<f32>(channel_key(rgb.r), channel_key(rgb.g), channel_key(rgb.b));
    let y = dot(logged, vec3<f32>(0.2126, 0.7152, 0.0722));
    let mixed = y + (logged - vec3<f32>(y)) * s;
    return max(vec3<f32>(decode_intermediate_soft(mixed.r), decode_intermediate_soft(mixed.g), decode_intermediate_soft(mixed.b)), vec3<f32>(0.0));
}
fn decode_intermediate_soft(v: f32) -> f32 {
    if v <= 0.02740668 { return v / 10.44426855; }
    return exp2(v / 0.07329248 - 7.0) - 0.0075;
}
// What lifting shadows does beyond the lift itself, measured on Resolve's
// exports (a smooth gain alone left photographs flat and grey): it brings
// out local contrast and colour, both in proportion to the lift, and so only
// where the Shadows zone lifts. In Intermediate, per channel:
//   out = in + lift + w * (DETAIL * (luma - blurred luma)
//                          + COLOUR * (1 + DARK * (0.4 - luma)) * (in - luma))
// with luma the Rec.709 luma of the Intermediate values and w the zone's
// lift in Intermediate units over 0.169. Darkening has neither (measured).
const SHADOW_DETAIL: f32 = 0.348;
const SHADOW_COLOUR: f32 = 0.48;
const SHADOW_COLOUR_DARK: f32 = 2.09;
fn log_luma709(rgb: vec3<f32>) -> f32 {
    return dot(vec3<f32>(channel_key(rgb.r), channel_key(rgb.g), channel_key(rgb.b)), vec3<f32>(0.2126, 0.7152, 0.0722));
}
fn shadows_finish(lifted: vec3<f32>, lift: f32, own_luma: f32, detail_base: f32) -> vec3<f32> {
    if lift <= 0.0 { return lifted; }
    let w = lift / 0.169;
    let logged = vec3<f32>(channel_key(lifted.r), channel_key(lifted.g), channel_key(lifted.b));
    let luma = dot(logged, vec3<f32>(0.2126, 0.7152, 0.0722));
    let colour = SHADOW_COLOUR * max(1.0 + SHADOW_COLOUR_DARK * (0.4 - own_luma), 0.0);
    let out = logged + vec3<f32>(w * SHADOW_DETAIL * (own_luma - detail_base)) + w * colour * (logged - vec3<f32>(luma));
    return max(vec3<f32>(decode_intermediate_soft(out.r), decode_intermediate_soft(out.g), decode_intermediate_soft(out.b)), vec3<f32>(0.0));
}
// Highlights has the same kind of finish with its own signs: lifting them
// softens local contrast and colour a little, pulling them brings out a
// little local contrast. w is the zone's offset in Intermediate over 0.2.
fn highlights_finish(rgb: vec3<f32>, offset: f32, own_luma: f32, detail_base: f32) -> vec3<f32> {
    if offset == 0.0 { return rgb; }
    let w = abs(offset) / 0.2;
    var detail = 0.215;
    var colour = -0.045;
    if offset > 0.0 {
        detail = -0.205;
        colour = -0.258;
    }
    let logged = vec3<f32>(channel_key(rgb.r), channel_key(rgb.g), channel_key(rgb.b));
    let luma = dot(logged, vec3<f32>(0.2126, 0.7152, 0.0722));
    let out = logged + vec3<f32>(w * detail * (own_luma - detail_base)) + w * colour * (logged - vec3<f32>(luma));
    return max(vec3<f32>(decode_intermediate_soft(out.r), decode_intermediate_soft(out.g), decode_intermediate_soft(out.b)), vec3<f32>(0.0));
}
fn channel_key(v: f32) -> f32 {
    let y = max(v, 0.0);
    if y <= 0.00262409 { return y * 10.44426855; }
    return (log2(y + 0.0075) + 7.0) * 0.07329248;
}
// The previous engine's controls key off their neighbourhood, and expect it
// to be the picture they are grading. That picture is now the lifted one,
// so its neighbourhood is lifted by the same gain: scene values directly,
// display values through their encoding. Without this, Highlights pulled
// down a lifted pixel by an unlifted key, and the tone response reversed.
fn lifted_neighbourhood(n: vec3<f32>, stops: f32) -> vec3<f32> {
    if stops == 0.0 { return n; }
    let g = exp2(stops);
    if parameters.basic_flags.y == 1u { return n * g; }
    return linear_to_srgb_extended(srgb_to_linear(n) * g);
}

fn basic_v2(rgb: vec3<f32>, tonal: vec3<f32>, structure: vec3<f32>) -> vec3<f32> {
    let gain = parameters.tone.z;
    let display = parameters.domain.x == 1u;
    if parameters.basic_flags.x == 0u && (!display || gain == 1.0) { return rgb * gain; }
    let a = parameters.basic[0];
    let b = parameters.basic[1];
    let raw = parameters.basic_flags.y;
    var c: vec3<f32>;
    if display { c = to_display_domain(rgb) * gain; } else { c = parameters.work_to_output * (rgb * gain); }
    c = apply_filmic_exposure(c, a.x);
    // Shadows, Whites and Blacks are the tone zones now (tone_zones); the
    // previous engine's only supplies Contrast here.
    // Contrast is v3's own now (apply_contrast); none of the previous
    // engine's tonal controls remain here.
    c = apply_tonal_adjustments_v2(c, structure, raw, 0.0, 0.0, 0.0, 0.0, a.z);
    // Highlights is a tone zone now too.
    c = apply_highlights_adjustment_v2(c, tonal, structure, raw, 0.0);
    if display { return from_display_domain(c); }
    // The previous engine's next stage, its HSL panel, runs on every pixel
    // whatever its settings and starts by clipping negative channels, so a
    // colour these controls push out of gamut arrives clipped. Do the same.
    // (In the display domain the return trip clips already.)
    return parameters.srgb_to_work * max(c, vec3<f32>(0.0));
}

fn grade(input:vec3<f32>, tonal:vec3<f32>, structure:vec3<f32>, key:f32, detail_base:f32) -> vec3<f32> {
    if parameters.flags.x == 0u {return input;}
    let balanced=parameters.white_balance*input;
    var toned=balanced;
    var seen=0.0;
    let amounts=zone_amounts();
    if any(amounts != vec4<f32>(0.0)) {
        // The regional key, carried along as each zone moves it.
        var regional=key;
        var parts=vec4<f32>(0.0);
        for (var z=0u; z<4u; z++) {
            let a=amounts[z];
            if a == 0.0 { continue; }
            let own=shadow_key(toned);
            let k=mix(regional, own, parameters.zone_style[0][z]);
            let d=zone_offset(k, z, a);
            parts[z]=d;
            regional=max(regional+d, 0.0);
            // One gain on all channels, taking k to k+d.
            let from_lin=max(decode_intermediate_soft(k),0.0);
            let to_lin=max(decode_intermediate_soft(max(k+d,0.0)),0.0);
            var gain=1.0;
            if from_lin > 1e-6 { gain=min(to_lin/from_lin, 64.0); }
            let together=toned*gain+vec3<f32>(max(to_lin-from_lin*gain,0.0));
            // Each channel on its own, read where the key reads it (shifted
            // by the region's difference from the pixel).
            var each=toned;
            var per_channel=parameters.zone_style[2][z];
            if a > 0.0 { per_channel=parameters.zone_style[1][z]; }
            if per_channel > 0.0 {
                for (var c=0u; c<3u; c++) {
                    let x=channel_key(toned[c]);
                    let dc=zone_offset(x+(k-own), z, a);
                    each[c]=max(decode_intermediate_soft(max(x+dc,0.0)),0.0);
                }
            }
            toned=mix(together, each, per_channel);
        }
        let own_luma=log_luma709(balanced);
        toned=shadows_finish(toned, parts.y*parameters.zone_style[3][1], own_luma, detail_base);
        toned=highlights_finish(toned, parts.z*parameters.zone_style[3][2], own_luma, detail_base);
        let y0=dot(balanced,vec3<f32>(0.27411851,0.87363190,-0.14775041));
        let y1=dot(toned,vec3<f32>(0.27411851,0.87363190,-0.14775041));
        if y0 > 1e-6 && y1 > 1e-6 { seen=log2(clamp(y1/y0, 1e-4, 64.0)); }
    }
    // The previous engine's controls see the picture as the zones left it.
    var rgb=basic_v2(toned, lifted_neighbourhood(tonal, seen), lifted_neighbourhood(structure, seen));
    rgb=apply_exposure_shape(rgb, toned);
    // After Exposure, as in Lightroom, so the pivot follows the exposed picture.
    rgb=apply_contrast(rgb);
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
    rgb=resolve_saturation(rgb);
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
    // Saturation itself is Resolve's now (resolve_saturation, above).
    let c=chroma*vibrance*exp2(delta.y);
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
