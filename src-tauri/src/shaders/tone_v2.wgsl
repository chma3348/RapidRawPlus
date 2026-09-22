// Tone functions shared by the previous engine (shader.wgsl) and v3
// (color_v3/*.wgsl), so a control that exists in both does exactly the same
// thing in each. Pure functions only: nothing here may read a binding. The
// one exception to that rule, `resolve_shadow_correction`, samples a table
// each engine binds its own way, so each engine defines it.

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


// v2 tonal core: all tone moves ride Oklab lightness, so contrast and
// shadow/black/white recovery cannot skew hue — the property that makes
// graded footage look "clean" instead of "crunchy".
// ---------------------------------------------------------------------------
// DaVinci Resolve's shadow lift, MEASURED rather than modelled.
//
// A 33x33x33 identity LUT (35,937 colours) was rendered through Resolve with
// the shadow slider maxed, and read back. Three facts came out of that cube,
// and together they overturn how this used to work:
//
//  1. It is a single SCALAR GAIN on the DISPLAY-ENCODED (sRGB) signal - not
//     a lightness move in Oklab. Across the cube the three per-channel
//     ratios agree to a median of 0.005 relative, i.e. one shared gain.
//     Oklab was the root error: a gain in gamma-encoded space is NOT a pure
//     lightness change there, which is why our version drifted hue and kept
//     needing a bigger chroma patch to stop looking drained. With the space
//     right, no chroma compensation is needed at all - saturation comes out
//     correct on its own.
//
//  2. The gain is driven by Rec.709 luma OF THE ENCODED VALUES. Scatter
//     about a 1D fit: 0.0219 here, against 0.049 for Oklab L, 0.144 for
//     max(RGB), 0.259 for min(RGB).
//
//  3. It is GLOBAL. The grey ramp is spread across 33 tiles with wildly
//     different neighbours and returns perfectly monotonic, with curvature
//     (0.5-0.7 code values) BELOW the 8-bit quantisation floor. So there is
//     no neighbourhood term here: the blurred driver this code used to
//     consult was inventing the blotchiness, not curing it.
//
// SHADOW_STOPS is that gain curve, in stops, sampled at Y = i/64. It is read
// straight off the neutral axis of the cube - for a grey the driver EQUALS
// the grey level, so these are Resolve's measurements, not a fit to them.
//
// Error against the full cube, in 8-bit code values:
//
//                        RMS       neutral axis     worst pixel
//     this               0.78          0.30              -
//     previous          16.88           -               109
//
// 0.78 is below one code value: on photographic colour this reproduces
// Resolve's shadow slider to within what 8-bit can express. RMS is quoted
// over HSV saturation < 0.6, where photographic content lives. Near-pure
// primaries below code 16 sit further off (mean ~5), but Resolve's own gain
// stops being monotonic in luma down there - [8,0,0] gains x4.10 while the
// DARKER [0,0,8] gains x2.95 - so no luma-driven model can follow it, and
// no photograph contains those colours.
//
// An independent check: this curve and the earlier 22-step tone chart, two
// different images through two different renders, agree to 0.02 stops.
// ---------------------------------------------------------------------------
const SHADOW_STOPS: array<f32, 65> = array<f32, 65>(
    1.65857, 1.60555, 1.55254, 1.50150, 1.44652, 1.37773, 1.30333, 1.23020,
    1.15793, 1.08748, 1.02134, 0.96235, 0.90852, 0.85941, 0.81392, 0.77105,
    0.73101, 0.69389, 0.65909, 0.62625, 0.59511, 0.56543, 0.53703, 0.50941,
    0.48340, 0.45999, 0.43771, 0.41527, 0.39366, 0.37347, 0.35437, 0.33690,
    0.31910, 0.29756, 0.27683, 0.26100, 0.24670, 0.23242, 0.21869, 0.20560,
    0.19300, 0.18072, 0.16888, 0.15752, 0.14659, 0.13604, 0.12594, 0.11637,
    0.10718, 0.09830, 0.08971, 0.08139, 0.07339, 0.06578, 0.05852, 0.05127,
    0.04496, 0.04043, 0.03641, 0.03194, 0.02801, 0.02575, 0.02299, 0.01446,
    0.00000
);

// Resolve maps pure black to 2 code values. Sharp enough to leave everything
// above the very bottom alone (0.3 code at Y = 1/32).
const SHADOW_BLACK_LIFT: f32 = 0.00784;

fn resolve_shadow_stops(y: f32) -> f32 {
    var table = SHADOW_STOPS;
    let x = clamp(y, 0.0, 1.0) * 64.0;
    let lo = floor(x);
    let i = min(u32(lo), 63u);
    return mix(table[i], table[i + 1u], x - lo);
}


// t = 1.0 is Resolve's shadow slider at maximum.
fn apply_resolve_shadow_lift_encoded(enc: vec3<f32>, t: f32) -> vec3<f32> {
    let y = clamp(get_luma(enc), 0.0, 1.0);
    let amt = clamp(t, 0.0, 1.0);
    // Both the curve and its correction fade out together with the slider, so
    // amt = 0 is exactly the identity. RAW values above 1.0 clamp to the top
    // of the table, which is already 1.0, leaving the analytic curve alone.
    let corr = mix(1.0, resolve_shadow_correction(enc), amt);
    let lifted = enc * (pow(2.0, amt * resolve_shadow_stops(y)) * corr)
        + amt * SHADOW_BLACK_LIFT * pow(max(1.0 - y, 0.0), 60.0);
    return max(lifted, vec3<f32>(0.0));
}

fn apply_resolve_shadow_lift(color: vec3<f32>, t: f32) -> vec3<f32> {
    let lifted = apply_resolve_shadow_lift_encoded(linear_to_srgb_extended(color), t);
    // srgb_to_linear does not clamp, so RAW headroom above 1.0 survives.
    return srgb_to_linear(lifted);
}

// The fitted curve: how far a display-encoded luma is pulled down at strength
// `amt`. Matches Resolve's highlight slider on the grey ramp to a few
// thousandths (1.0 -> 0.725 at amt = 1.0).
fn highlight_compressed_luma(y: f32, amt: f32) -> f32 {
    return max(0.0, y - amt * 0.275 * pow(clamp(y, 0.0, 1.0), 1.88));
}

// A cheap, one-sample approximation of the tone equalizer's edge-aware guide.
// The Gaussian base is useful inside a textured area, but must not cross a
// strong brightness edge: doing so makes a bright object darken its darker
// surround. At a hard edge the guide falls back to the pixel; across fine
// texture it keeps the blurred base, so the detail split below still works.
fn highlight_edge_aware_base(y: f32, blurred_y: f32) -> f32 {
    let delta = abs(y - blurred_y);
    let edge_weight = exp(-(delta * delta) / (2.0 * 0.10 * 0.10));
    return mix(y, blurred_y, edge_weight);
}

fn relative_chroma(c: vec3<f32>) -> f32 {
    let hi = max(c.r, max(c.g, c.b));
    let lo = min(c.r, min(c.g, c.b));
    return (hi - lo) / max(hi, 1.0e-4);
}

// Blacks are an exposure band, not a colour operation. darktable's tone
// equalizer applies one correction factor to all three RGB channels; that is
// the important property to keep here because it preserves hue and does not
// turn small chroma noise into coloured blotches.
const BLACKS_INPUT_RANGE: f32 = 2.5;
const BLACKS_MAX_STOPS: f32 = 2.0;
// Linear-light equivalent of display-encoded 0.035. Keeping this linear is
// what makes the floor colour-safe; decoding a tinted encoded offset bends
// the three channels by different amounts.
const BLACKS_FLOOR_LINEAR: f32 = 0.002709;

fn apply_blacks_adjustment(
    color: vec3<f32>,
    bl: f32,
) -> vec3<f32> {
    let source = max(color, vec3<f32>(0.0));
    let encoded = max(linear_to_srgb_extended(source), vec3<f32>(0.0));
    let y = max(get_luma(encoded), 0.0);
    let source_linear_y = max(get_luma(source), 0.0);

    // The reference places "blacks" around -5 EV. The soft shoulders make
    // the adjustment continuous while keeping ordinary midtones pinned. Use
    // the pixel's own luminance: a broad Gaussian guide created visible bands
    // around bright/dark boundaries because it was not a true guided filter.
    let zone = 1.0 - smoothstep(0.015, 0.27, y);
    let strength = clamp(abs(bl) / BLACKS_INPUT_RANGE, 0.0, 1.0);
    let signed_stops = select(-BLACKS_MAX_STOPS, BLACKS_MAX_STOPS, bl > 0.0);
    // Apply the correction in scene-linear RGB, exactly as the reference tone
    // equalizer does. A shared gain here preserves actual chromaticity. Doing
    // this in encoded RGB appeared ratio-stable numerically but decoded each
    // channel nonlinearly, producing the orange/magenta cast in deep shadows.
    var adjusted = source * exp2(signed_stops * strength * zone);

    if (bl > 0.0) {
        // Multiplication alone cannot recover an exact zero. Compute the same
        // lifted floor brightness as before, but reach it by scaling along the
        // pixel's original LINEAR colour direction. That keeps the pulling
        // power without adding a grey veil over valid near-black colour.
        let crushed = 1.0 - smoothstep(0.0, 0.045, y);
        let floor_luma = BLACKS_FLOOR_LINEAR * strength * crushed * crushed * zone;
        let target_linear_y = source_linear_y
            * exp2(signed_stops * strength * zone)
            + floor_luma;

        // Only values indistinguishable from numerical zero fade to neutral;
        // there is no chromaticity to preserve there. This transition is far
        // below the former 0.8-4% encoded fallback that caused the white cast.
        let color_confidence = smoothstep(1.0e-7, 2.0e-5, source_linear_y);
        let source_direction = source / max(source_linear_y, 1.0e-8);
        let direction = mix(vec3<f32>(1.0), source_direction, color_confidence);
        adjusted = direction * target_linear_y;
    }

    return max(adjusted, vec3<f32>(0.0));
}

// Detail-preserving compression. The plain curve above has slope ~0.55 across
// the bright plateau, so it multiplies texture there by 0.55 -- a lit curtain
// or cloud loses ~40% of its detail and turns to mush. Resolve keeps the
// texture because it compresses the low-frequency TONE while leaving local
// detail alone.
//
// So split the signal: `base` is a small-radius blur (the tonal blur already
// bound for the tone tools), `detail` is the pixel's departure from it.
// Compress only the base with the curve, then add the detail back at full
// strength. On a flat plateau detail is ~0 and this equals the plain curve
// (same tone match to Resolve); on textured highlights the detail rides
// through undimmed, which is the 95%+ retention the plain curve throws away.
fn apply_highlight_compression_detail_encoded(
    enc: vec3<f32>,
    base_enc: vec3<f32>,
    t: f32,
) -> vec3<f32> {
    let source = clamp(enc, vec3<f32>(0.0), vec3<f32>(1.0));
    let amt = clamp(t, 0.0, 1.0);
    let y = clamp(get_luma(source), 0.0, 1.0);
    let y_base = clamp(get_luma(clamp(base_enc, vec3<f32>(0.0), vec3<f32>(1.0))), 0.0, 1.0);
    let guided_base = highlight_edge_aware_base(y, y_base);
    let detail = y - guided_base;
    // Compress the base tone, carry the detail through undimmed. Clamp keeps a
    // bright textured peak from running away past white.
    let target_y = clamp(highlight_compressed_luma(guided_base, amt) + detail, 0.0, 1.0);
    let gain = target_y / max(y, 1.0e-4);
    return source * gain;
}

// Scene-linear counterpart of the display-bounded function above. Crucially,
// neither the source nor the result is clipped at display white. RAW values of
// 1.2 and 2.0 must remain distinct so the later filmic/AgX transform can use
// that headroom. For a truly clipped, near-neutral RAW core, cautiously borrow
// chromaticity from the broad neighborhood; this is the only place where the
// input no longer contains trustworthy colour information.
fn apply_highlight_compression_scene(
    color: vec3<f32>,
    base_linear: vec3<f32>,
    neighborhood_linear: vec3<f32>,
    t: f32,
) -> vec3<f32> {
    let source = max(linear_to_srgb_extended(color), vec3<f32>(0.0));
    let base = max(linear_to_srgb_extended(base_linear), vec3<f32>(0.0));
    let neighborhood = max(linear_to_srgb_extended(neighborhood_linear), vec3<f32>(0.0));
    let amt = clamp(t, 0.0, 1.0);
    let y = max(get_luma(source), 0.0);
    let y_base = max(get_luma(base), 0.0);
    let guided_base = highlight_edge_aware_base(y, y_base);
    let detail = y - guided_base;
    let target_y = max(highlight_compressed_luma(guided_base, amt) + detail, 0.0);
    let compressed = source * (target_y / max(y, 1.0e-4));

    let neighborhood_y = get_luma(neighborhood);
    let neighborhood_chroma = relative_chroma(neighborhood);
    let source_neutral = 1.0 - smoothstep(0.025, 0.14, relative_chroma(source));
    let clipped_core = smoothstep(1.0, 1.18, y);
    let useful_surround = smoothstep(0.035, 0.20, neighborhood_chroma);
    let recovery = amt * clipped_core * source_neutral * useful_surround * 0.72;
    let neighborhood_tint = clamp(
        neighborhood / max(neighborhood_y, 1.0e-4),
        vec3<f32>(0.35),
        vec3<f32>(2.5),
    );
    let recovered = neighborhood_tint * target_y;
    return srgb_to_linear(mix(compressed, recovered, recovery));
}

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

    // The shadow LIFT runs first, on display-encoded RGB - the space the
    // measurement says Resolve works in - before any Oklab work below.
    // The slider divides by SCALES.shadows = 120, so slider 100 arrives as
    // sh = 0.8333; x1.2 puts our maximum exactly on Resolve's.
    var color_in = color;
    if (sh > 0.0) {
        color_in = apply_resolve_shadow_lift(color_in, sh * 1.2);
    }

    var neighborhood_linear: vec3<f32>;
    if (is_raw == 1u) {
        neighborhood_linear = neighborhood_input_space;
    } else {
        neighborhood_linear = srgb_to_linear(neighborhood_input_space);
    }

    // Keep Blacks out of the generic Oklab lightness/chroma compensation
    // below. It now works as an RGB gain, like the reference tone equalizer,
    // with a narrowly gated floor only for values too crushed for gain alone.
    if (bl != 0.0) {
        color_in = apply_blacks_adjustment(color_in, bl);
    }

    var lab = linear_to_oklab(max(color_in, vec3<f32>(0.0)));
    var l_ok = lab.x;
    let l_clamped = clamp(l_ok, 0.0, 1.0);

    // Local tone mapping driver: the NEIGHBORHOOD's lightness decides how
    // much a zone moves, so texture inside a lifted region rides along
    // and detail survives (the Lightroom/Resolve trick). A dash of the
    // pixel's own lightness guards halos at strong zone boundaries.
    let l_base = clamp(linear_to_oklab(max(neighborhood_linear, vec3<f32>(0.0))).x, 0.0, 1.0);
    // Mostly the pixel's OWN lightness. At 0.75 the neighbourhood decided
    // almost everything, and identical tones got wildly different
    // treatment: measured at full shadow lift, an L=0.10 pixel was lifted
    // x1.34 in a dark area, x1.55 in a mid one, and x1.00 — untouched —
    // next to something bright. A 1.6x spread across the same tone reads
    // as patches rather than a tone curve, which is the blotchy,
    // inconsistent look next to Resolve's global one.
    //
    // Some neighbourhood weighting is still worth keeping: it is what lets
    // texture inside a lifted region ride along instead of flattening. At
    // 0.25 the spread falls to 1.2x and nothing is left unlifted, which
    // keeps that benefit without the patchwork.
    let driver = mix(l_clamped, l_base, 0.25);

    // Whites: true white-point control — the gain fades in above the
    // midtones so the lower half of the image stays pinned (the old
    // uniform expansion was just a second exposure slider).
    var l_new = l_ok;
    if (wh != 0.0) {
        // REVERTED to the original uniform headroom expansion. Two zone/
        // curve redesigns (2026-08-16/24) both damaged faces: lit skin
        // occupies Oklab L 0.80-0.88, inside any "whites" luminance zone
        // — a knee or mask that spares skin excludes real whites, and one
        // that reaches whites grabs faces. Weak-but-safe wins until a
        // portrait-fixture-tested design exists. Chroma follows a
        // darkening pull (kept from the redesign - prevents bloom).
        let w_gain = 1.0 / max(1.0 - wh * 0.22, 0.01);
        l_new = l_new * w_gain;
    }

    // Shadows: only the DARKENING direction is left here. The lift is
    // applied at the top of this function instead, against a measured
    // Resolve curve - see apply_resolve_shadow_lift. Darkening has not been
    // measured against Resolve, so it keeps its original behaviour.
    if (sh < 0.0) {
        // RAW scene-linear and JPEG display-linear place the same perceptual
        // zone at different L - calibrate the fade per domain (one wide fade
        // for both was how shadows grabbed JPEG midtones).
        let sh_fade = select(0.46, 0.62, is_raw == 1u);
        let zone = 1.0 - smoothstep(0.04, sh_fade, driver);
        l_new = l_new * pow(2.0, sh * 1.1 * zone);
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

    // Chroma follows lightness. In Oklab saturation is chroma/L, so every
    // tool above that moved L changed saturation as a side effect — and
    // each had its own partial, inconsistent patch or none at all. Lifting
    // shadows kept 80-92% of saturation (that is the drained, chalky look);
    // whites cost 11% on skin; contrast pushed skin shadows to 111% while
    // pulling skin highlights to 92%, because it compensated nowhere.
    //
    // One correction covers all of them. The exponent is a hair under 1.0:
    // full preservation can read neon on strong lifts, and 0.9 measured
    // 97-99% retention across hues with no hue drift and no change in gamut
    // behaviour.
    //
    // Clamped because the ratio can explode on near-black pixels under the
    // remaining Oklab tools and amplify chroma noise. Blacks bypasses this
    // path entirely and therefore needs no such compensation.
    let l_ratio = l_new / max(l_ok, 1e-4);
    // 1.15, measured — not the 0.9 I picked by reasoning.
    //
    // Fitted across 72 colour patches (12 hues x 3 shadow levels x 2
    // saturations) put through both applications:
    //
    //   chroma_ratio = lightness_ratio ^ k
    //   Resolve   k = 1.15   (quartiles 1.00-1.29)
    //   ours      k = 0.90   (quartiles 0.88-0.92)
    //
    // Above 1.0 chroma GAINS on lightness; below it, colour drains as the
    // tone rises. Resolve gains, we drained — worst in the deepest patches,
    // where our chroma came out 36% short of theirs. That is the "dull and
    // lifeless" complaint, and it is separate from the tone curve.
    //
    // The same fit recovered our own 0.90 to within a hundredth, which is
    // the check that the method is measuring what it claims to.
    // Set 1.29 to MEASURE 1.15. The knob is not 1:1 — measured across the
    // 72 patches, setting 0.90 gave 0.90 but setting 1.15 gave only 1.06,
    // so roughly a third of what is asked for is eaten downstream, where
    // gamut and tonemapping pull saturated colour back. 1.29 lands the
    // measured value on Resolve's 1.15.
    // NOTE: shadows and blacks no longer reach this. Their lifts now run in display-
    // encoded space at the top of the function, where — as the LUT
    // measurement showed — saturation comes out right on its own and needs
    // no compensation. What is left here serves whites and contrast, which
    // have NOT been measured against Resolve, so 1.29 stays
    // as it was rather than being retuned on a guess.
    let chroma_follow = clamp(pow(max(l_ratio, 1e-4), 1.29), 0.25, 2.5);
    lab.y *= chroma_follow;
    lab.z *= chroma_follow;

    lab.x = l_new;
    var out = oklab_to_linear(lab);
    // Oklab round trips can produce tiny negatives on saturated colors.
    out = compress_gamut_soft(out);
    return out;
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

// The Refined (process version 2) highlights control. Shared with v3.
fn apply_highlights_adjustment_v2(
    color_in: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    neighborhood_input_space: vec3<f32>,
    is_raw: u32,
    highlights_adj: f32
) -> vec3<f32> {
    if (highlights_adj == 0.0) { return color_in; }
    if (highlights_adj < 0.0) {
        // The compression constants were fitted against Resolve's
        // highlight slider at MINIMUM, i.e. amt = 1.0 -- measured on the
        // grey ramps of the reference chart, the fit lands within a few
        // thousandths of their curve (1.0 -> 0.725 exactly). But the UI
        // slider divides by SCALES.highlights = 120, so -100 arrives
        // here as -0.8333 and the curve under-compressed by up to 12
        // code values near white. x1.2 puts slider minimum exactly on
        // Resolve's, the same convention the shadow lift uses.
        //
        // Detail-preserving: compress the blurred base tone and add the
        // pixel's local detail back undimmed, so bright texture (curtains,
        // clouds) keeps ~95% of its contrast instead of the ~65% the plain
        // per-pixel curve leaves. blurred_color_input_space is the r=3.5
        // tonal blur.
        if (is_raw == 1u) {
            return apply_highlight_compression_scene(
                color_in,
                blurred_color_input_space,
                neighborhood_input_space,
                -highlights_adj * 1.2,
            );
        }
        return srgb_to_linear(apply_highlight_compression_detail_encoded(
            linear_to_srgb_extended(color_in),
            blurred_color_input_space,
            -highlights_adj * 1.2,
        ));
    }

    var lab = linear_to_oklab(max(color_in, vec3<f32>(0.0)));
    let t = clamp(lab.x, 0.0, 1.0);

    // Positive highlights still use the local lightness driver so bright
    // regions glow together instead of clipping pixel-by-pixel.
    var nb_linear: vec3<f32>;
    if (is_raw == 1u) {
        nb_linear = neighborhood_input_space;
    } else {
        nb_linear = srgb_to_linear(neighborhood_input_space);
    }
    let l_base_h = clamp(linear_to_oklab(max(nb_linear, vec3<f32>(0.0))).x, 0.0, 1.2);
    let edge_weight_h = exp(-pow(abs(t - l_base_h), 2.0) / (2.0 * 0.12 * 0.12));
    let driver_h = mix(t, l_base_h, 0.75 * edge_weight_h);
    let mask = smoothstep(0.45, 1.0, driver_h);

    // Brighten with a clip-resistant approach: the push fades as
    // L nears white, so +100 glows instead of blowing out.
    lab.x = lab.x + highlights_adj * 0.7 * mask * max(1.02 - t, 0.0);
    return compress_gamut_soft(oklab_to_linear(lab));
}

// ---------- Display renderings (the Basic panel's tone mappers) ----------

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

// AgX with its matrices passed in: each engine computes them its own way.
fn agx_full_transform_with(
    color_in: vec3<f32>,
    pipe_to_rendering: mat3x3<f32>,
    rendering_to_pipe: mat3x3<f32>,
) -> vec3<f32> {
    let compressed_color = agx_compress_gamut(color_in);
    let color_in_agx_space = pipe_to_rendering * compressed_color;
    let tonemapped_agx = agx_tonemap(color_in_agx_space);
    return rendering_to_pipe * tonemapped_agx;
}

fn apply_raw_resolve_display_match(c: vec3<f32>) -> vec3<f32> {
    // Fitted from a no-edit RAW pair exported through DaVinci Resolve and
    // RapidRAW. The main error was display-space chroma, not scene-linear
    // exposure: Resolve's render was about 23% more saturated overall, with
    // much warmer yellow/orange highlights.
    let source = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    var matched = vec3<f32>(
        dot(source, vec3<f32>(1.23379687, -0.08049921, -0.04131304)) - 0.02269294,
        dot(source, vec3<f32>(-0.03429803, 1.08902969, 0.05548705)) - 0.01958838,
        dot(source, vec3<f32>(0.38266230, -1.08934827, 1.72959885)) - 0.01445909,
    );
    matched = clamp(matched, vec3<f32>(0.0), vec3<f32>(1.0));

    let source_luma = get_luma(source);
    let highlight = smoothstep(0.70, 0.88, source_luma);
    let clipped_core = smoothstep(0.94, 0.995, source_luma);
    var max_c = max(matched.r, max(matched.g, matched.b));
    var min_c = min(matched.r, min(matched.g, matched.b));
    var chroma = (max_c - min_c) / max(max_c, 1.0e-5);

    // Near-white clipped highlights in RapidRAW were landing pink/gray
    // while Resolve keeps a luminous white-yellow core.
    let neutral_core = clipped_core * (1.0 - smoothstep(0.08, 0.18, chroma)) * 0.65;
    matched = mix(matched, vec3<f32>(1.0), neutral_core);

    max_c = max(matched.r, max(matched.g, matched.b));
    min_c = min(matched.r, min(matched.g, matched.b));
    chroma = (max_c - min_c) / max(max_c, 1.0e-5);
    let warm_hue = clamp((matched.r - matched.b) * 2.0 + (matched.g - matched.b) * 0.8, 0.0, 1.0);
    let warm_highlight = warm_hue * clipped_core * smoothstep(0.08, 0.42, chroma);
    matched.r = mix(matched.r, 1.0, warm_highlight * 0.30);
    matched.g = mix(matched.g, 1.0, warm_highlight * 0.72);
    matched.b *= 1.0 - warm_highlight * 0.28;

    return clamp(matched, vec3<f32>(0.0), vec3<f32>(1.0));
}

// The previous engine's "Basic" rendering of a RAW file, for process
// version 2: a gentle gamma and S-curve on the encoded signal, then its fitted
// match to Resolve's display rendering.
fn basic_raw_rendering(linear: vec3<f32>) -> vec3<f32> {
    var srgb_emulated = linear_to_srgb(linear);
    const BRIGHTNESS_GAMMA: f32 = 1.1;
    srgb_emulated = pow(srgb_emulated, vec3<f32>(1.0 / BRIGHTNESS_GAMMA));
    const CONTRAST_MIX: f32 = 0.75;
    let contrast_curve = srgb_emulated * srgb_emulated * (3.0 - 2.0 * srgb_emulated);
    return apply_raw_resolve_display_match(mix(srgb_emulated, contrast_curve, CONTRAST_MIX));
}
