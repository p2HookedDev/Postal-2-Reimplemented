import bevy_pbr::render::{
    forward_io::VertexOutput,
    pbr_types::PbrInput,
};
import package::p2_render::color::{to_display, to_linear};
import package::p2_render::layer_bindings::lit_color;
import package::p2_render::layer_types::ShaderLayers;
import package::p2_render::modern::{GLASS_CLASS, Lit, allows_gloss, ambient_occlusion, classed_surface, mirrors_screen, modern_glass, surface_light, view_around, with_light};
import package::p2_render::modern_base::{LUMINANCE, LampLight, modern_bits, to_eye};
@if(GLASS_MIRROR && DEPTH_PREPASS)
import bevy_pbr::{
    render::{
        mesh_view_bindings::{depth_prepass_texture, view, view_transmission_sampler, view_transmission_texture},
        utils::interleaved_gradient_noise,
        view_transformations::{ndc_to_uv, position_world_to_ndc},
    },
    ssr::raymarch,
};
import package::p2_render::sun_shadow::{ShadowLookup, sun_faced, sun_seen_from, sun_visibility};
import package::p2_render::sun_levels::LevelSource;
import package::p2_render::lamp_shadow::lamp_light;
@if(SCREEN_SUN)
import package::p2_render::lamp_shadow::screen_sun;
@if(CACHED_LAMPS)
import package::p2_render::lamp_shadow::{cached_sky, find_patch};
@if(CACHED_LAMPS)
import package::p2_render::sun_shadow::ROOFED_SKY;
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
    view: u32,
}

// debug_view.rs holds the same number.
const DEBUG_VIEW_SHIFT: u32 = 12u;
const DEBUG_GREY: f32 = 0.5;

const POOL_REFLECTANCE: f32 = 0.02;
const POOL_LEAST_ROUGHNESS: f32 = 0.06;
const POOL_SHINE_LIMIT: f32 = 16.0;

fn pooled_surface(surface: vec3<f32>, pool: Pool) -> vec3<f32> {
    let wet = pool.wet;
    if wet <= 0.0 || surface.z > 0.5 {
        return surface;
    }
    return vec3(mix(surface.x, max(pool.roughness, POOL_LEAST_ROUGHNESS), wet), surface.y * (1.0 - wet), -mix(abs(surface.z), POOL_REFLECTANCE, wet));
}

fn modern_light(in: VertexOutput, pbr_input: PbrInput, sun: u32, normal: vec3<f32>, bumped: vec3<f32>, cast_normal: vec3<f32>, flag: f32, blend: vec4<f32>, darken: bool, pool: Pool, lookup: ShadowLookup) -> ModernLight {
    let position = pbr_input.world_position.xyz;
@if(SCREEN_SUN)
    var seen = sun_faced(sun, normal, screen_sun(in, pbr_input.frag_coord.xy));
@else
    var seen = sun_seen_from(material_levels(in), sun, pbr_input.world_position, normal, pbr_input.frag_coord.xy, lookup);
@if(CACHED_LAMPS) {
    let sky = cached_sky(in, find_patch(in));
    if sky >= 0.0 {
        seen.z = mix(ROOFED_SKY, 1.0, sky);
    }
}
    var standing = pool;
    if darken {
        standing = no_pool(normal);
    }
    let gloss = (allows_gloss(flag) || standing.wet > 0.0) && !darken;
    let surface = pooled_surface(classed_surface(flag, blend), standing);
    let lamps = lamp_light(in, position, wet_normal(bumped, standing), cast_normal, gloss, surface, flag, lookup);
    let view = (modern_bits(flag) >> DEBUG_VIEW_SHIFT) & 15u;
    return ModernLight(position, seen, ambient_occlusion(pbr_input.frag_coord.xy), gloss, lamps, surface, standing, vec3(0.0), view);
}

fn add_dynamic(layers: ShaderLayers, lit: vec3<f32>, color: vec3<f32>, darken: bool, dynamic: vec3<f32>) -> vec3<f32> {
    if darken {
        return lit_color(layers, color, with_light(lit, vec3(1.0), dynamic));
    }
    return with_light(lit, color, dynamic);
}

fn shade_surface(incoming: ModernLight, layers: ShaderLayers, color: vec4<f32>, normal: vec3<f32>, darken: bool, see_through: bool, dynamic: vec3<f32>) -> vec4<f32> {
    if incoming.view == GLASS_VIEW {
        return glass_view(incoming, layers, color, normal, see_through && !darken);
    }
    if incoming.view != 0u {
        return debug_shade(incoming, color, normal, dynamic);
    }
    if see_through && !darken {
        let mirror = glass_mirror(incoming, layers, normal);
        let mirrors = mirrors_screen(see_through, layers.blend.y);
        let glass = modern_glass(color, incoming.position, normal, incoming.seen, incoming.occlusion, incoming.gloss, incoming.lamps, incoming.surface, mirror, mirrors);
        return vec4(with_light(glass.rgb, color.rgb, dynamic), glass.a);
    }
    let wet = wet_color(color.rgb, incoming.pool);
    let lit = pooled_surface_lit(select(wet, vec3(1.0), darken), incoming.position, wet_normal(normal, incoming.pool), incoming);
    return vec4(add_dynamic(layers, lit, wet, darken, dynamic), color.a);
}

const GLASS_STEPS: u32 = 24u;
const GLASS_REFINE_STEPS: u32 = 4u;
// In world units: how far a ray may pass behind the depth and still hit it; a room seen through the pane lies
// further back (unverified).
const GLASS_THICKNESS: f32 = 16.0;
const GLASS_EDGE_FADE: f32 = 0.08;

fn glass_mirror(incoming: ModernLight, layers: ShaderLayers, normal: vec3<f32>) -> vec4<f32> {
@if(GLASS_MIRROR && DEPTH_PREPASS) {
    if !incoming.gloss || layers.blend.y != GLASS_CLASS {
        return vec4(0.0);
    }
    let start = position_world_to_ndc(incoming.position);
    var march = raymarch::depth_ray_march_new_from_depth(vec2<f32>(textureDimensions(depth_prepass_texture)));
    raymarch::depth_ray_march_from_cs(&march, start);
    let ray = reflect(-to_eye(incoming.position), normal);
    raymarch::depth_ray_march_to_ws_dir(&march, ray);
    march.linear_steps = GLASS_STEPS;
    march.bisection_steps = GLASS_REFINE_STEPS;
    march.use_secant = true;
    march.depth_thickness_linear_z = GLASS_THICKNESS;
    march.jitter = interleaved_gradient_noise(ndc_to_uv(start.xy) * view.viewport.zw, 0u);
    let found = raymarch::depth_ray_march_march(&march);
    if !found.hit {
        let end = ndc_to_uv(march.ray_end_cs.xy);
        let size = vec2<i32>(textureDimensions(depth_prepass_texture));
        let away = dot(ray, view.world_from_view[2].xyz) < 0.0;
        if ray.y <= 0.0 || !away || textureLoad(depth_prepass_texture, clamp(vec2<i32>(end * vec2<f32>(size)), vec2(0), size - 1), 0) > 0.0 {
            return vec4(0.0);
        }
        let sky = to_display(textureSampleLevel(view_transmission_texture, view_transmission_sampler, end, 0.0).rgb);
        return vec4(sky * sky, 1.0);
    }
    let uv = found.hit_uv;
    let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
    let seen = to_display(textureSampleLevel(view_transmission_texture, view_transmission_sampler, uv, 0.0).rgb);
    return vec4(seen * seen, smoothstep(0.0, GLASS_EDGE_FADE, edge));
}
    return vec4(0.0);
}

const GLASS_VIEW: u32 = 10u;

fn glass_view(incoming: ModernLight, layers: ShaderLayers, color: vec4<f32>, normal: vec3<f32>, glass: bool) -> vec4<f32> {
    if !glass {
        return vec4(color.rgb * 0.2, color.a);
    }
    let marched = incoming.gloss && mirrors_screen(glass, layers.blend.y);
    let mirror = glass_mirror(incoming, layers, normal);
    let missed = select(vec3(0.0, 0.0, 0.3), vec3(0.15, 0.0, 0.0), marched);
    return vec4(mix(missed, mirror.rgb, mirror.a), 1.0);
}

fn debug_shade(incoming: ModernLight, color: vec4<f32>, normal: vec3<f32>, dynamic: vec3<f32>) -> vec4<f32> {
    let grey = vec3(DEBUG_GREY);
    if incoming.view == 1u {
        let lit = pooled_surface_lit(to_linear(grey), incoming.position, wet_normal(normal, incoming.pool), incoming);
        return vec4(with_light(lit, to_linear(grey), dynamic), color.a);
    }
    if incoming.view == 9u {
        return color;
    }
    let lamps = incoming.lamps;
    let around = view_around(normal, lamps, incoming.surface);
    var shown = vec3(0.0);
    switch incoming.view {
        case 2u: { shown = grey * sqrt(around.sun_color) * (saturate(dot(normal, around.to_sun)) * incoming.seen.x); }
        case 3u: { shown = vec3(incoming.seen.z); }
        case 4u: { shown = grey * lamps.diffuse; }
        case 5u: { shown = vec3(0.25 * DEBUG_GREY * dot(lamps.diffuse, LUMINANCE)) + vec3(DEBUG_GREY * dot(lamps.unshadowed, LUMINANCE), 0.0, 0.0); }
        case 6u: { shown = grey * sqrt(around.fills * around.fills + lamps.bounce); }
        case 7u: { shown = vec3(incoming.occlusion * lamps.grounded); }
        case 8u: { shown = normal * 0.5 + 0.5; }
        default: {}
    }
    return vec4(to_linear(shown), color.a);
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
