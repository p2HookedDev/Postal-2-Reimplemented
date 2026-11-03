struct Params {
    ramp: vec4<f32>,
}

@group(2) @binding(0) var<uniform> params: Params;
@group(2) @binding(1) var scene: texture_2d<f32>;
@group(2) @binding(2) var scene_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vertex(@location(0) position: vec3<f32>, @location(2) uv: vec2<f32>) -> VertexOutput {
    return VertexOutput(vec4<f32>(position.xy, 0.5, 1.0), uv);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3(0.0), vec3(1.0));
    return select(1.055 * pow(x, vec3(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3(2.4)), c / 12.92, c <= vec3(0.04045));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let shown = linear_to_srgb(textureSample(scene, scene_sampler, in.uv).rgb);
    let brightness = params.ramp.x;
    let contrast = params.ramp.y;
    let ramped = pow(shown, vec3(1.0 / params.ramp.z)) * (contrast + 0.5) + (brightness - 0.5) * 0.5 - contrast * 0.5 + 0.25;
    return vec4<f32>(srgb_to_linear(clamp(ramped, vec3(0.0), vec3(1.0))), 1.0);
}
