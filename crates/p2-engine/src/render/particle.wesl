// Particle sprites: D3D colour stages in display (gamma) space, then the output the pipeline's fixed
// blend turns into the engine's result. Sprite grids expand each quad from this frame's records.

import bevy_pbr::render::{
    mesh_functions::{get_world_from_local, get_tag},
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
};

@if(DISTANCE_FOG)
import bevy_pbr::render::{
    mesh_view_bindings::fog,
    mesh_view_types::FOG_MODE_LINEAR,
};

struct Params {
    style: u32,
    alpha_ref: f32,
    alpha_test: u32,
    blend_frames: u32,
    fogged: u32,
}

@group(3) @binding(0) var particle_texture: texture_2d<f32>;
@group(3) @binding(1) var particle_sampler: sampler;
@group(3) @binding(2) var<uniform> params: Params;

// One sprite (`particles.rs` `sprite_record`, Bevy coordinates, the mesh's own space). Per emitter a header record (only
// `count` set) comes first, at the entity's `MeshTag`.
struct SpriteRecord {
    center: vec3<f32>,
    color: u32,
    up: vec3<f32>,
    count: u32,
    right: vec3<f32>,
    unused: u32,
    uv: vec4<f32>,
    next_uv: vec4<f32>,
}

@group(3) @binding(4) var<storage, read> sprites: array<SpriteRecord>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv_b: vec2<f32>,
    @location(5) color: vec4<f32>,
}

@if(!PARTICLE_INSTANCES)
struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv_b: vec2<f32>,
    @location(5) color: vec4<f32>,
}

@if(!PARTICLE_INSTANCES)
@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    out.world_position = get_world_from_local(vertex.instance_index) * vec4(vertex.position, 1.0);
    out.position = position_world_to_clip(out.world_position.xyz);
    out.uv = vertex.uv;
    out.uv_b = vertex.uv_b;
    out.color = vertex.color;
    return out;
}

@if(PARTICLE_INSTANCES)
struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

@if(PARTICLE_INSTANCES)
@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let first = get_tag(vertex.instance_index);
    let quad = u32(vertex.position.x + 0.5);
    let corner = u32(vertex.position.y + 0.5);
    if quad >= sprites[first].count {
        out.position = vec4(2.0, 2.0, 2.0, 1.0);
        return out;
    }
    let sprite = sprites[first + 1u + quad];
    let along_right = corner == 1u || corner == 2u;
    let along_up = corner >= 2u;
    let local = sprite.center + select(-sprite.up, sprite.up, along_up) + select(-sprite.right, sprite.right, along_right);
    out.world_position = get_world_from_local(vertex.instance_index) * vec4(local, 1.0);
    out.position = position_world_to_clip(out.world_position.xyz);
    out.uv = vec2(select(sprite.uv.x, sprite.uv.z, along_right), select(sprite.uv.y, sprite.uv.w, along_up));
    out.uv_b = vec2(select(sprite.next_uv.x, sprite.next_uv.z, along_right), select(sprite.next_uv.y, sprite.next_uv.w, along_up));
    out.color = unpack4x8unorm(sprite.color);
    return out;
}

// Display brightness of the background the gamma-space sums are fitted for (the framebuffer blends in linear light).
const BACKGROUND: f32 = 0.25;
const BACKGROUND_LINEAR: f32 = 0.05087609;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3(0.0), vec3(1.0));
    return select(pow((x + 0.055) / 1.055, vec3(2.4)), x / 12.92, x <= vec3(0.04045));
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3(0.0), vec3(1.0));
    return select(1.055 * pow(x, vec3(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3(0.0031308));
}

@if(DISTANCE_FOG)
fn fog_amount(world_position: vec3<f32>) -> f32 {
    if params.fogged == 0u || fog.mode != FOG_MODE_LINEAR {
        return 0.0;
    }
    let distance = length(world_position - view.world_position.xyz);
    let start = fog.be.x;
    let end = fog.be.y;
    return fog.base_color.a * (1.0 - clamp((end - distance) / (end - start), 0.0, 1.0));
}

@if(!DISTANCE_FOG)
fn fog_amount(world_position: vec3<f32>) -> f32 {
    return 0.0;
}

@if(DISTANCE_FOG)
fn scene_fog_color() -> vec3<f32> {
    return linear_to_srgb(fog.base_color.rgb);
}

@if(!DISTANCE_FOG)
fn scene_fog_color() -> vec3<f32> {
    return vec3(0.0);
}

// Linear output F for a fixed blend `F + D * keep` (D the framebuffer, linear) that gives the engine's display-space
// `source + d * keep` exactly over a background of display brightness BACKGROUND.
fn over_background(source: vec3<f32>, keep: f32) -> vec3<f32> {
    return max(srgb_to_linear(source + vec3(BACKGROUND * keep)) - vec3(BACKGROUND_LINEAR * keep), vec3(0.0));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let style = params.style;
    let first = textureSampleBias(particle_texture, particle_sampler, in.uv, view.mip_bias);
    var texel = vec4(linear_to_srgb(first.rgb), first.a);
    var alpha = texel.a * in.color.a;
    var color = texel.rgb * in.color.rgb;
    if params.blend_frames != 0u {
        let second = textureSampleBias(particle_texture, particle_sampler, in.uv_b, view.mip_bias);
        texel = mix(texel, vec4(linear_to_srgb(second.rgb), second.a), in.color.a);
        alpha = texel.a;
        color = texel.rgb * in.color.rgb;
    } else if style == 2u {
        color = mix(in.color.rgb, texel.rgb, in.color.a);
        alpha = mix(in.color.a, texel.a, in.color.a);
    }
    if params.alpha_test != 0u && alpha <= params.alpha_ref {
        discard;
    }
    // D3D fog mixes the colour towards a per-style fog colour: the scene's for regular and alpha blend,
    // grey 127 for modulated (neutral under 2x), black for the others (they add or darken nothing).
    var fog_color = vec3(0.0);
    if style <= 1u {
        fog_color = scene_fog_color();
    } else if style == 2u {
        fog_color = vec3(127.0 / 255.0);
    }
    color = mix(color, fog_color, fog_amount(in.world_position.xyz));
    switch style {
        case 0u: {
            return vec4(srgb_to_linear(color), 1.0);
        }
        case 1u: {
            return vec4(over_background(color * alpha, 1.0 - alpha), alpha);
        }
        case 2u: {
            return vec4(min(pow(2.0 * color, vec3(2.2)) * 0.5, vec3(1.0)), 0.0);
        }
        case 3u: {
            return vec4(over_background(color, 1.0), 0.0);
        }
        case 4u: {
            return vec4(over_background(color, 1.0 - alpha), alpha);
        }
        // Darken (blend Zero, OneMinusSrc): d * (1 - colour), a product: right but for the sRGB curve near black.
        case 5u: {
            return vec4(vec3(1.0) - srgb_to_linear(vec3(1.0) - color), 0.0);
        }
        default: {
            let target_color = srgb_to_linear(color + vec3(BACKGROUND) * (vec3(1.0) - color));
            return vec4(max((target_color - vec3(BACKGROUND_LINEAR)) / (1.0 - BACKGROUND_LINEAR), vec3(0.0)), 0.0);
        }
    }
}
