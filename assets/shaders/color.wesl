fn to_display(linear: vec3<f32>) -> vec3<f32> {
    return pow(max(linear, vec3(0.0)), vec3(1.0 / 2.2));
}

fn to_linear(display: vec3<f32>) -> vec3<f32> {
    return pow(max(display, vec3(0.0)), vec3(2.2));
}

// The splat crimson sRGB (95, 0, 0) in display space, and absorption per Unreal unit per channel.
const BLOOD_BODY: vec3<f32> = vec3(0.114, 0.0006, 0.0006);
const BLOOD_SIGMA: vec3<f32> = vec3(30.0, 420.0, 400.0);
