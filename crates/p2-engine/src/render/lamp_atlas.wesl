// One face of a lamp's shadow tile: the distance to the nearest static surface, over the lamp's radius. Blending takes
// the minimum, so the pass needs no depth buffer.

struct Face {
    x: vec4<f32>,
    y: vec4<f32>,
    z: vec4<f32>,
    // xyz the lamp, w 1 / its radius.
    lamp: vec4<f32>,
    // x the guard band's scale; y, z the depth terms that clip at the near plane.
    terms: vec4<f32>,
}

@group(0) @binding(0) var<uniform> face: Face;

struct Output {
    @builtin(position) clip: vec4<f32>,
    @location(0) from_lamp: vec3<f32>,
}

fn placed(position: vec3<f32>) -> Output {
    let from_lamp = position - face.lamp.xyz;
    let along = dot(from_lamp, face.z.xyz);
    var out: Output;
    out.clip = vec4(dot(from_lamp, face.x.xyz) * face.terms.x, dot(from_lamp, face.y.xyz) * face.terms.x, face.terms.y * along + face.terms.z, along);
    out.from_lamp = from_lamp;
    return out;
}

@vertex
fn vertex(@location(0) position: vec3<f32>) -> Output {
    return placed(position);
}

@fragment
fn fragment(in: Output) -> @location(0) vec4<f32> {
    return vec4(length(in.from_lamp) * face.lamp.w, 0.0, 0.0, 1.0);
}

@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct MaskedOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) from_lamp: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) cutoff: f32,
}

@vertex
fn masked_vertex(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) cutoff: f32) -> MaskedOutput {
    let solid = placed(position);
    return MaskedOutput(solid.clip, solid.from_lamp, uv, cutoff);
}

@fragment
fn masked_fragment(in: MaskedOutput) -> @location(0) vec4<f32> {
    if textureSample(image, image_sampler, in.uv).a < in.cutoff {
        discard;
    }
    return vec4(length(in.from_lamp) * face.lamp.w, 0.0, 0.0, 1.0);
}
