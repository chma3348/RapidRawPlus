// Inputs and working values may exceed 1 or be negative. Clamp only at output.
fn decode_component(v: f32, transfer: u32) -> f32 {
    if transfer == 0u { return v; }
    if transfer == 1u {
        if v <= 0.04045 { return v / 12.92; }
        return pow((v + 0.055) / 1.055, 2.4);
    }
    if v <= 0.02740668 { return v / 10.44426855; }
    return exp2(v / 0.07329248 - 7.0) - 0.0075;
}

fn encode_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { return 12.92 * v; }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}
