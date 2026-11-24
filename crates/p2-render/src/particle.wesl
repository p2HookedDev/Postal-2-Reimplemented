// Particle sprites: D3D colour stages in display (gamma) space, then the output the pipeline's fixed
// blend turns into the engine's result. Sprite grids expand each quad from this frame's records.

import package::p2_render::color::to_display;
import package::p2_render::sun_shadow::{has_sun, sun_index, sun_map_visibility, sun_seen};
import package::p2_render::modern::{SKY_VIEW_MARK, ambient_occlusion, modern_surface, surface_of};
import package::p2_render::modern_base::LampLight;
import bevy_pbr::render::{
    mesh_functions::{get_world_from_local, get_tag},
    view_transformations::{depth_ndc_to_view_z, position_world_to_clip},
    mesh_view_bindings::{view, lights},
};

@if(DEPTH_PREPASS)
import bevy_pbr::render::mesh_view_bindings::depth_prepass_texture;

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
    // Modern graphics: the depth in world units over which a sprite fades into the geometry behind it (0 off), and
    // how it takes the light where it is: 0 not, 1 as a puff in the air, `LIT_LYING` as the surface it lies on.
    soft: f32,
    lit: u32,
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
    // The light at the emitter from its header record (display light, its zone's ambient and lamps); below 0 unlit.
    @location(6) light: vec3<f32>,
    // A modern puddle: the place across its quad (-1 to 1 each way) and its edge seed; all 0 for any other sprite.
    @location(7) puddle: vec3<f32>,
    // For a sprite lying on a surface: its emitter's lamps alone (display light) and the sprite's normal.
    @location(8) lamps: vec3<f32>,
    @location(9) normal: vec3<f32>,
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
    out.light = vec3(-1.0);
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
    out.light = sprites[first].center;
    out.lamps = sprites[first].up;
    out.normal = normalize((get_world_from_local(vertex.instance_index) * vec4(cross(sprite.up, sprite.right), 0.0)).xyz);
    if sprite.unused != 0u {
        out.puddle = vec3(select(-1.0, 1.0, along_right), select(-1.0, 1.0, along_up), f32(sprite.unused));
    }
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

// A puff of smoke faces the sun with about half of itself.
const PUFF_SUN: f32 = 0.5;
// Matches `particle_material.rs` `LIT_LYING`.
const LIT_LYING: u32 = 2u;
const LIGHT_LIMIT: f32 = 2.0;

// In display light, as the modern surfaces add their diffuse light: the emitter's light, which does not depend on the
// view, and the sun where it reaches. The sky view and an emitter marked unlit draw it as retail does, unlit.
fn particle_light(position: vec3<f32>, frag_coord: vec4<f32>, emitter: vec3<f32>) -> vec3<f32> {
    if emitter.x < 0.0 || lights.ambient_color.a < SKY_VIEW_MARK {
        return vec3(1.0);
    }
    var light = emitter;
    let sun = sun_index();
    if sun < lights.n_directional_lights {
        let to_sun = lights.directional_lights[sun].direction_to_light;
        let seen = sun_map_visibility(sun, vec4(position, 1.0), to_sun, frag_coord.xy);
        light += to_display(lights.directional_lights[sun].color.rgb) * (seen * PUFF_SUN);
    }
    return min(light, vec3(LIGHT_LIMIT));
}

// A sprite lying on a surface takes the light the modern surfaces give a white texel there, as a decal on the same
// surface does; the emitter's lamps stand in for the surface's own. Without a sun the surfaces keep their lightmaps.
fn lying_light(position: vec3<f32>, frag_coord: vec4<f32>, emitter: vec3<f32>, lamps: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    if emitter.x < 0.0 || lights.ambient_color.a < SKY_VIEW_MARK || !has_sun() {
        return particle_light(position, frag_coord, emitter);
    }
    let facing = select(-normal, normal, dot(normal, view.world_position.xyz - position) >= 0.0);
    let seen = sun_seen(vec4(position, 1.0), facing, frag_coord.xy);
    let lamp_light = LampLight(lamps, vec3(0.0), vec3(0.0), vec3(0.0), vec4(-1.0, 0.0, 0.0, 0.0), 1.0);
    let white = modern_surface(vec3(1.0), position, facing, seen, ambient_occlusion(frag_coord.xy), false, lamp_light, surface_of(0.0));
    return to_display(white);
}

// 1 where the sprite stands `params.soft` units or more before the geometry behind it, falling to 0 where it meets it.
fn soft_fade(frag_coord: vec4<f32>) -> f32 {
@if(DEPTH_PREPASS) {
    if params.soft <= 0.0 {
        return 1.0;
    }
    let scene = textureLoad(depth_prepass_texture, vec2<i32>(frag_coord.xy), 0);
    if scene <= 0.0 || frag_coord.z <= 0.0 {
        return 1.0;
    }
    return saturate((depth_ndc_to_view_z(frag_coord.z) - depth_ndc_to_view_z(scene)) / params.soft);
}
@else {
    return 1.0;
}
}

// Linear output F for a fixed blend `F + D * keep` (D the framebuffer, linear) that gives the engine's display-space
// `source + d * keep` exactly over a background of display brightness BACKGROUND.
fn over_background(source: vec3<f32>, keep: f32) -> vec3<f32> {
    return max(srgb_to_linear(source + vec3(BACKGROUND * keep)) - vec3(BACKGROUND_LINEAR * keep), vec3(0.0));
}

// 1 inside a puddle's edge, whose radius wanders with a noise fixed by the seed, falling to 0 outside it.
fn puddle_edge(puddle: vec3<f32>) -> f32 {
    if puddle.z <= 0.0 {
        return 1.0;
    }
    let angle = atan2(puddle.y, puddle.x);
    let seed = puddle.z * 1.618;
    let wander = 0.5 * sin(3.0 * angle + seed) + 0.3 * sin(5.0 * angle + seed * 2.3) + 0.2 * sin(8.0 * angle + seed * 3.7);
    let edge = 0.88 + 0.08 * wander;
    return 1.0 - smoothstep(edge - 0.18, edge, length(puddle.xy));
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
    if params.lit == LIT_LYING {
        color *= lying_light(in.world_position.xyz, in.position, in.light, in.lamps, in.normal);
    } else if params.lit != 0u {
        color *= particle_light(in.world_position.xyz, in.position, in.light);
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
    // Faded towards what each blend leaves the background as: no cover, no added light, neutral grey under modulation.
    let fade = soft_fade(in.position) * puddle_edge(in.puddle);
    if fade < 1.0 {
        if style == 2u {
            color = mix(vec3(127.0 / 255.0), color, fade);
        } else if style != 1u {
            color *= fade;
        }
        alpha *= fade;
    }
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
