import bevy_pbr::render::{
    forward_io::VertexOutput,
    pbr_types::PbrInput,
};
import package::p2_render::color::{to_display, to_linear};
import package::p2_render::layer_bindings::lit_color;
import package::p2_render::layer_types::ShaderLayers;
import package::p2_render::modern::{Lit, allows_gloss, ambient_occlusion, classed_surface, modern_glass, surface_light, view_around, with_light};
import package::p2_render::modern_base::LampLight;
import package::p2_render::sun_shadow::{ShadowLookup, sun_faced, sun_seen_from, sun_visibility};
import package::p2_render::sun_levels::LevelSource;
import package::p2_render::lamp_shadow::lamp_light;
@if(SCREEN_SHADOWS_ONLY)
import package::p2_render::lamp_shadow::screen_sun;
import package::p2_render::projector::{Pool, no_pool, wet_color, wet_normal};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};

fn material_levels(in: VertexOutput) -> LevelSource {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return LevelSource(material_indices[slot].depth_map_texture, material_indices[slot].depth_map_sampler);
}
@else {
    return LevelSource(0u, 0u);
}
}

fn material_sun_visibility(in: VertexOutput, sun: u32, position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    return sun_visibility(material_levels(in), sun, position, normal, frag_coord);
}

struct ModernLight {
    position: vec3<f32>,
    seen: vec3<f32>,
    occlusion: f32,
    gloss: bool,
    lamps: LampLight,
    surface: vec3<f32>,
    pool: Pool,
    glow: vec3<f32>,
}

const POOL_REFLECTANCE: f32 = 0.02;
// A pool is never smoother than this, and its highlight never brighter than this in display light squared:
// a mirror's highlight of a lamp or the sun spikes far past the frame's range and blooms over the screen.
const POOL_LEAST_ROUGHNESS: f32 = 0.06;
const POOL_SHINE_LIMIT: f32 = 16.0;

// A special-lit surface takes only special lights, wet or dry. The pool's reflectance is negated as a material
// class's is: it changes how the surface shines, while its colour comes from `wet_color`.
fn pooled_surface(surface: vec3<f32>, pool: Pool) -> vec3<f32> {
    let wet = pool.wet;
    if wet <= 0.0 || surface.z > 0.5 {
        return surface;
    }
    return vec3(mix(surface.x, max(pool.roughness, POOL_LEAST_ROUGHNESS), wet), surface.y * (1.0 - wet), -mix(abs(surface.z), POOL_REFLECTANCE, wet));
}

fn modern_light(in: VertexOutput, pbr_input: PbrInput, sun: u32, normal: vec3<f32>, bumped: vec3<f32>, flag: f32, blend: vec4<f32>, darken: bool, pool: Pool, lookup: ShadowLookup) -> ModernLight {
    let position = pbr_input.world_position.xyz;
@if(SCREEN_SHADOWS_ONLY)
    let seen = sun_faced(sun, normal, screen_sun(in, pbr_input.frag_coord.xy));
@else
    let seen = sun_seen_from(material_levels(in), sun, pbr_input.world_position, normal, pbr_input.frag_coord.xy, lookup);
    var standing = pool;
    if darken {
        standing = no_pool(normal);
    }
    let gloss = (allows_gloss(flag) || standing.wet > 0.0) && !darken;
    let surface = pooled_surface(classed_surface(flag, blend), standing);
    let lamps = lamp_light(in, position, wet_normal(bumped, standing), normal, gloss, surface, flag, lookup);
    return ModernLight(position, seen, ambient_occlusion(pbr_input.frag_coord.xy), gloss, lamps, surface, standing, vec3(0.0));
}

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
    let wet = wet_color(color.rgb, incoming.pool);
    let lit = pooled_surface_lit(select(wet, vec3(1.0), darken), incoming.position, wet_normal(normal, incoming.pool), incoming);
    return vec4(add_dynamic(layers, lit, wet, darken, dynamic), color.a);
}

fn pooled_surface_lit(color: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, incoming: ModernLight) -> vec3<f32> {
    let display = to_display(color);
    var around = view_around(normal, incoming.lamps, incoming.surface);
    around.glow = incoming.glow;
    let lit = surface_light(display * display, position, normal, incoming.seen, incoming.occlusion, incoming.gloss, incoming.lamps, incoming.surface, around);
    let shine = select(lit.shine, pool_shine(lit, incoming.pool), incoming.pool.wet > 0.0);
    return to_linear(sqrt(max(lit.matte + shine, vec3(0.0)))) + incoming.pool.glow;
}

fn pool_shine(lit: Lit, pool: Pool) -> vec3<f32> {
    return min((lit.shine + lit.reflection * (pool.tint - vec3(1.0)) * pool.wet) * pool.sheen, vec3(POOL_SHINE_LIMIT));
}

fn pooled_unlit(color: vec3<f32>, normal: vec3<f32>, incoming: ModernLight) -> vec3<f32> {
    let display = to_display(color);
    var around = view_around(normal, incoming.lamps, incoming.surface);
    around.glow = incoming.glow;
    let lit = surface_light(display * display, incoming.position, normal, incoming.seen, incoming.occlusion, incoming.gloss, incoming.lamps, incoming.surface, around);
    return to_linear(sqrt(display * display + pool_shine(lit, incoming.pool) * incoming.pool.wet)) + incoming.pool.glow;
}

fn retail_lit(texture: vec3<f32>, light: vec3<f32>) -> vec3<f32> {
    return to_linear(min(to_display(texture) * 2.0 * light, vec3(1.0)));
}
