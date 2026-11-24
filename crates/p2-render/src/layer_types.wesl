// The lit materials' data layouts, with no bindings, so the main and the prepass shaders read one declaration.
// Matches `shader_layers.rs` and `actor_light.rs`.

struct ShaderLayers {
    detail: vec4<f32>,
    specular: vec4<f32>,
    specular_u: vec4<f32>,
    specular_v: vec4<f32>,
    blend: vec4<f32>,
}

// A bindless material's index table entry; a terrain layer's table holds its alpha map too.
struct LayerIndices {
    data: u32,
    detail: u32,
    detail_sampler: u32,
    specular: u32,
    specular_sampler: u32,
    flat_specular: u32,
    flat_specular_sampler: u32,
    @if(TERRAIN_LAYER)
    unused_107: u32,
    @if(TERRAIN_LAYER)
    unused_108: u32,
    @if(TERRAIN_LAYER)
    alpha_map: u32,
    @if(TERRAIN_LAYER)
    alpha_map_sampler: u32,
}

struct HardwareLight {
    position: vec4<f32>,
    direction: vec3<f32>,
    kind: u32,
    color: vec4<f32>,
    attenuation: vec4<f32>,
}

struct Lighting {
    ambient: vec3<f32>,
    count: u32,
    lights: array<HardwareLight, 8>,
}

struct ActorShading {
    lighting: Lighting,
    layers: ShaderLayers,
    overlay: u32,
    reach: f32,
    free: f32,
    glow: f32,
}
