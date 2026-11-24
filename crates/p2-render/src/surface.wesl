// What a lit surface does with its light. Each lit shader says only where its light comes from.

import bevy_pbr::render::{
    forward_io::VertexOutput,
    pbr_types::PbrInput,
    clustered_forward::ClusterableObjectIndexRanges,
};
import package::p2_render::color::{to_display, to_linear};
import package::p2_render::layer_bindings::{ShaderLayers, lit_color};
import package::p2_render::modern::{allows_gloss, ambient_occlusion, classed_surface, modern_glass, modern_surface, with_light};
import package::p2_render::modern_base::LampLight;
import package::p2_render::sun_shadow::sun_seen_from;
import package::p2_render::lamp_shadow::lamp_light;

struct ModernLight {
    position: vec3<f32>,
    seen: vec3<f32>,
    occlusion: f32,
    gloss: bool,
    lamps: LampLight,
    surface: vec3<f32>,
}

// The sun sees `normal`, the lamps light `bumped`.
fn modern_light(in: VertexOutput, pbr_input: PbrInput, sun: u32, normal: vec3<f32>, bumped: vec3<f32>, ranges: ClusterableObjectIndexRanges, flag: f32, blend: vec4<f32>, darken: bool) -> ModernLight {
    let position = pbr_input.world_position.xyz;
    let seen = sun_seen_from(sun, pbr_input.world_position, normal, pbr_input.frag_coord.xy);
    let gloss = allows_gloss(flag) && !darken;
    let surface = classed_surface(flag, blend);
    let lamps = lamp_light(in, position, bumped, pbr_input.frag_coord.xy, ranges, gloss, surface, flag);
    return ModernLight(position, seen, ambient_occlusion(pbr_input.frag_coord.xy), gloss, lamps, surface);
}

// A darken surface takes the light alone, as it took the light map: `lit` is then the surface lit white.
fn add_dynamic(layers: ShaderLayers, lit: vec3<f32>, color: vec3<f32>, darken: bool, dynamic: vec3<f32>) -> vec3<f32> {
    if darken {
        return lit_color(layers, color, with_light(lit, vec3(1.0), dynamic));
    }
    return with_light(lit, color, dynamic);
}

fn shade_surface(incoming: ModernLight, layers: ShaderLayers, color: vec4<f32>, normal: vec3<f32>, darken: bool, see_through: bool, dynamic: vec3<f32>) -> vec4<f32> {
    if see_through && !darken {
        let glass = modern_glass(color, incoming.position, normal, incoming.seen, incoming.occlusion, incoming.gloss, incoming.lamps, incoming.surface);
        return vec4(with_light(glass.rgb, color.rgb, dynamic), glass.a);
    }
    let lit = modern_surface(select(color.rgb, vec3(1.0), darken), incoming.position, normal, incoming.seen, incoming.occlusion, incoming.gloss, incoming.lamps, incoming.surface);
    return vec4(add_dynamic(layers, lit, color.rgb, darken, dynamic), color.a);
}

// The texture under twice the light in display space, clamped to 1 as the frame buffer clamps it.
fn retail_lit(texture: vec3<f32>, light: vec3<f32>) -> vec3<f32> {
    return to_linear(min(to_display(texture) * 2.0 * light, vec3(1.0)));
}
