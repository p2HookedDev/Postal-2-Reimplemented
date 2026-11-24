const START_COMPRESSION: f32 = 0.76;
const DESATURATION: f32 = 0.05;

// Khronos PBR Neutral's shoulder on linear colour, without its toe offset: below the shoulder a colour stays as
// it is.
fn pbr_neutral(linear: vec3<f32>) -> vec3<f32> {
    let peak = max(linear.r, max(linear.g, linear.b));
    if peak < START_COMPRESSION {
        return linear;
    }
    let d = 1.0 - START_COMPRESSION;
    let new_peak = 1.0 - d * d / (peak + d - START_COMPRESSION);
    let g = 1.0 - 1.0 / (DESATURATION * (peak - new_peak) + 1.0);
    return mix(linear * (new_peak / peak), vec3(new_peak), g);
}
