// Modern graphics shading, read only while the switch is on: real-time light from the map's suns and sky over the
// texture, with no baked light. Linear light is display light squared. Diffuse lights add in display light as retail's
// do, which keeps the map's colours; only highlights add in linear light. The result is linear light, past 1 where bright.

import package::p2_engine::color::{to_display, to_linear};
import bevy_pbr::render::{
    forward_io::VertexOutput,
    clustered_forward as clustering,
    mesh_view_bindings::{view, lights, directional_shadow_textures},
    mesh_view_types::{DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT, DIRECTIONAL_LIGHT_FLAGS_CONTACT_SHADOWS_ENABLED_BIT},
    pbr_bindings,
    pbr_types,
    shadows,
    shadow_sampling::sample_shadow_map_hardware,
};
import bevy_pbr::render::{
    mesh_view_bindings::clustered_lights,
    mesh_view_types::POINT_LIGHT_FLAGS_SHADOWS_ENABLED_BIT,
    shadow_sampling::sample_shadow_cubemap,
};
@if(SCREEN_SPACE_AMBIENT_OCCLUSION)
import bevy_pbr::render::mesh_view_bindings::screen_space_ambient_occlusion_texture;
@if(CONTACT_SHADOWS)
import bevy_pbr::render::mesh_view_bindings::contact_shadows_settings;
@if(CONTACT_SHADOWS && DEPTH_PREPASS)
import bevy_pbr::{render::{mesh_view_bindings::depth_prepass_texture, utils::interleaved_gradient_noise, view_transformations::position_world_to_ndc}, ssr::raymarch};
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};

const RELIEF_STRENGTH: f32 = 3.0;
const RELIEF_MAX_TILT: f32 = 0.45;
const RELIEF_NEAR: f32 = 1024.0;
const RELIEF_FAR: f32 = 4096.0;
const RELIEF_MINIFIED_FROM: f32 = 1.0;
const RELIEF_MINIFIED_TO: f32 = 3.0;
const RELIEF_SKEW_FROM: f32 = 0.05;
const RELIEF_SKEW_TO: f32 = 0.2;
const AO_STRENGTH: f32 = 0.8;
const LUMINANCE: vec3<f32> = vec3(0.2126, 0.7152, 0.0722);

// In shadow map texels: the depth margin and the move along the normal.
const SHADOW_BIAS: f32 = 1.0;
const SHADOW_NORMAL_OFFSET: f32 = 1.5;
// The radius of a shadow's soft edge in world units, the same in every level and cascade: counted in texels it was four
// times wider in each coarser level, crisp near the eye and soft far off. A level whose texels are wider than this
// cannot be that sharp and takes the least radius its texels allow.
const SOFT_EDGE_UNITS: f32 = 1.5;
const SOFT_EDGE_LEAST_TEXELS: f32 = 0.75;
const SOFT_EDGE_MOST_TEXELS: f32 = 8.0;
const SOFT_EDGE_TAPS: i32 = 12;
const GOLDEN_ANGLE: f32 = 2.3999632;
// A receiver plane steeper than this many texels of depth per texel is noise.
const SHADOW_SLOPE_CAP_TEXELS: f32 = 16.0;
// A level's shadows stand a little apart from the next coarser one's, by their texels' bias, so the change is spread
// over the outer part of the level, not made at its edge.
// In a level's half widths from the eye, not from the level's centre, which jumps a quarter of the half width at each
// redraw: a level is used out to where it surely holds the point whatever its centre, and blends into the next from
// `LEVEL_FADE_FROM`.
const LEVEL_FADE_FROM: f32 = 0.45;
const LEVEL_USABLE: f32 = 0.75;
// In world units: the most a shadow lookup moves along the normal, which a coarse level's wide texels would make a
// visible shift of the whole shadow.
const SHADOW_NORMAL_OFFSET_MOST: f32 = 8.0;
// In the outermost level's half widths from the eye: its shadows fade out over this range. The level stands up to a
// fifth of its half width off the eye and moves in steps, so an end at its own edge jumped as the eye walked.
const LAST_LEVEL_FADE_FROM: f32 = 0.55;
const LAST_LEVEL_FADE_TO: f32 = 0.74;
// The share of the movers' last cascade past which their shadows fade out.
const MOVER_FADE_FROM: f32 = 0.8;
const SUN_FACING_FADE: f32 = 0.05;
const SKY_FACING_FADE: f32 = 0.1;
const CONTACT_NEAR: f32 = 1024.0;
const CONTACT_FAR: f32 = 2048.0;
// Contact steps fall from the near count to the far one over these distances, where a march's result is hardly seen.
const CONTACT_STEPS_FULL_TO: f32 = 256.0;
const CONTACT_STEPS_FEW_FROM: f32 = 1024.0;
const CONTACT_FEWEST_STEPS: f32 = 4.0;
// In display light, as the sky's share left to fills in a sun shadow.
const SKY_SHADOWED: f32 = 0.8;
// In the sky level's texels: how far its four taps spread, which softens a roof's edge.
const SKY_KERNEL_TEXELS: f32 = 2.0;
// The sky left under a roof, standing for the bounce no light here carries (a guess, not seen in game).
const ROOFED_SKY: f32 = 0.1;
// The sky's reflection in a sun shadow: what cast the shadow also stands in the way of a reflection towards the sun's side.
const REFLECTION_SHADOWED: f32 = 0.2;

const DEFAULT_ROUGHNESS: f32 = 0.85;
const DIELECTRIC_F0: f32 = 0.04;
// A near-mirror's sub-pixel highlight is a firefly, so one light's peak stays under this.
const GGX_PEAK: f32 = 256.0;
// The sun is a disc of 0.53 degrees, which widens the highlight.
const SUN_DISC: f32 = 0.002325;
const GROUND_ALBEDO: vec3<f32> = vec3(0.17, 0.14, 0.11);
// Retail's lit surface stops at twice its texture.
const DIFFUSE_LIMIT: f32 = 2.0;
// The sun's cascade overlap, which also tells it from the fills when it draws no shadow map. Matches `modern.rs`.
const SUN_OVERLAP: f32 = 0.1;

const ACTOR_GLOSS: f32 = 0.1;
// Skinned actors' sun shadow fades in over N · L from 0 to this (the interpolated normal's): near the terminator a
// low-poly mesh's flat faces shadow themselves where its smooth normal still faces the sun, which showed its triangles.
const ACTOR_TERMINATOR_COS: f32 = 0.35;
const ACTOR_SHININESS: f32 = 20.0;

const MODERN_ON: u32 = 1u;
const NO_RELIEF: u32 = 2u;
const NO_GLOSS: u32 = 4u;

fn modern_bits(flag: f32) -> u32 {
    return u32(max(flag, 0.0) + 0.5);
}

fn is_modern(flag: f32) -> bool {
    return (modern_bits(flag) & MODERN_ON) != 0u;
}

fn allows_gloss(flag: f32) -> bool {
    let bits = modern_bits(flag);
    return (bits & MODERN_ON) != 0u && (bits & NO_GLOSS) == 0u;
}

// x roughness, y metal, z 1 on a special-lit surface. `packed` is the roughness, 0 for the default surface, 2 further from
// 0 when special-lit, negative on a metal.
fn surface_of(packed: f32) -> vec3<f32> {
    let size = abs(packed);
    let special = size >= 2.0;
    let rough = select(size, size - 2.0, special);
    return vec3(select(rough, DEFAULT_ROUGHNESS, rough == 0.0), select(0.0, 1.0, packed < 0.0), select(0.0, 1.0, special));
}

fn relief_from(
    normal: vec3<f32>,
    position: vec3<f32>,
    dp_dx: vec3<f32>,
    dp_dy: vec3<f32>,
    duv_dx: vec2<f32>,
    duv_dy: vec2<f32>,
    size: vec2<f32>,
    slope: vec2<f32>,
) -> vec3<f32> {
    let dp_dy_perp = cross(dp_dy, normal);
    let dp_dx_perp = cross(normal, dp_dx);
    let flip = select(-1.0, 1.0, dot(dp_dx, dp_dy_perp) >= 0.0);
    let tangent = dp_dy_perp * duv_dx.x + dp_dx_perp * duv_dy.x;
    let bitangent = dp_dy_perp * duv_dx.y + dp_dx_perp * duv_dy.y;
    let scale = flip * inverseSqrt(max(max(dot(tangent, tangent), dot(bitangent, bitangent)), 1e-20));
    let skew = abs(duv_dx.x * duv_dy.y - duv_dx.y * duv_dy.x) / max(length(duv_dx) * length(duv_dy), 1e-20);
    let texels = max(length(duv_dx * size), length(duv_dy * size));
    let fade = (1.0 - smoothstep(RELIEF_NEAR, RELIEF_FAR, distance(view.world_position.xyz, position)))
        * (1.0 - smoothstep(RELIEF_MINIFIED_FROM, RELIEF_MINIFIED_TO, texels))
        * smoothstep(RELIEF_SKEW_FROM, RELIEF_SKEW_TO, skew);
    var tilt = RELIEF_STRENGTH * fade * scale * (slope.x * tangent + slope.y * bitangent);
    let amount = length(tilt);
    if amount > RELIEF_MAX_TILT {
        tilt *= RELIEF_MAX_TILT / amount;
    }
    return normalize(normal - tilt);
}

// Past these a slope shows nothing, so its texture taps are saved.
fn relief_shows(position: vec3<f32>, duv_dx: vec2<f32>, duv_dy: vec2<f32>, size: vec2<f32>) -> bool {
    let texels = max(length(duv_dx * size), length(duv_dy * size));
    return texels < RELIEF_MINIFIED_TO && distance(view.world_position.xyz, position) < RELIEF_FAR;
}

fn base_relief(in: VertexOutput, pbr_input: pbr_types::PbrInput, normal: vec3<f32>, flag: f32) -> vec3<f32> {
@if(VERTEX_UVS_A) {
    if (modern_bits(flag) & NO_RELIEF) != 0u || (pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT) == 0u {
        return normal;
    }
@if(BINDLESS)
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
@if(BINDLESS)
    let texture = bindless_textures_2d[material_indices[slot].base_color_texture];
@else
    let texture = pbr_bindings::base_color_texture;
@if(BINDLESS)
    let sampler_ = bindless_samplers_filtering[material_indices[slot].base_color_sampler];
@else
    let sampler_ = pbr_bindings::base_color_sampler;
    let uv = (pbr_input.material.uv_transform * vec3(in.uv, 1.0)).xy;
    let gx = dpdx(uv);
    let gy = dpdy(uv);
    let size = vec2<f32>(textureDimensions(texture, 0));
    let position = pbr_input.world_position.xyz;
    if !relief_shows(position, gx, gy, size) {
        return normal;
    }
    let step = max(1.0 / size, max(abs(gx), abs(gy)));
    let b = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(0.0, -1.0), gx, gy).rgb, LUMINANCE);
    let d = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(-1.0, 0.0), gx, gy).rgb, LUMINANCE);
    let f = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(1.0, 0.0), gx, gy).rgb, LUMINANCE);
    let h = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(0.0, 1.0), gx, gy).rgb, LUMINANCE);
    let slope = vec2(f - d, h - b) / 2.0;
    return relief_from(normal, position, dpdx(position), dpdy(position), gx, gy, size, slope);
}
@else {
    return normal;
}
}

fn casts_shadow(i: u32) -> bool {
    return (lights.directional_lights[i].flags & DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u;
}

// A black sun with one cascade is a cached level of the static world's shadow, not a light.
fn is_level(i: u32) -> bool {
    return casts_shadow(i) && lights.directional_lights[i].num_cascades == 1u;
}

// The one level looking straight down is the only one with an overlap, which one cascade never reads.
fn is_sky_level(i: u32) -> bool {
    return is_level(i) && lights.directional_lights[i].cascades_overlap_proportion > 0.0;
}

fn is_cached_level(i: u32) -> bool {
    return is_level(i) && lights.directional_lights[i].cascades_overlap_proportion == 0.0;
}

fn is_sun(i: u32) -> bool {
    return !is_level(i) && abs(lights.directional_lights[i].cascades_overlap_proportion - SUN_OVERLAP) < 0.01;
}

fn sun_index() -> u32 {
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if is_sun(i) {
            return i;
        }
    }
    return lights.n_directional_lights;
}

// The sun is spawned whenever the switch is on. Views it does not reach, the sky's and the first-person weapon's, keep
// retail's light.
fn has_sun() -> bool {
    return sun_index() < lights.n_directional_lights;
}

// The lowest sun shadow setting takes one hardware tap everywhere.
fn sharp_filter() -> bool {
@if(SHADOW_FILTER_METHOD_HARDWARE_2X2) {
    return false;
}
@else {
    return true;
}
}

fn shadow_tap(uv: vec2<f32>, offset: vec2<f32>, depth: f32, slope: vec2<f32>, layer: i32) -> f32 {
    return sample_shadow_map_hardware(uv + offset, depth + dot(offset, slope), layer);
}

// Sharp on purpose: a rim that settles most pixels, then a 3 x 3 texel filter in four bilinear taps.
fn cascade_shadow(sun: u32, cascade_index: u32, position: vec3<f32>, normal: vec3<f32>, sharp: bool) -> f32 {
    let light = &lights.directional_lights[sun];
    let cascade = &(*light).cascades[cascade_index];
    let to_sun = (*light).direction_to_light;
    let facing = dot(normal, to_sun);
    let tilt = min(sqrt(saturate(1.0 - facing * facing)) / max(facing, 0.1), 4.0);
    let texel_world = (*cascade).texel_size;
    let margin = texel_world * SHADOW_BIAS * (1.0 + 0.5 * tilt);
    let moved = position + normal * min(texel_world * SHADOW_NORMAL_OFFSET, SHADOW_NORMAL_OFFSET_MOST) + to_sun * margin;
    let local = shadows::world_to_directional_light_local(sun, cascade_index, vec4(moved, 1.0));
    if local.w == 0.0 {
        return 1.0;
    }
    let matrix = (*cascade).clip_from_world;
    let along_x = vec3(matrix[0].x, matrix[1].x, matrix[2].x);
    let along_y = vec3(matrix[0].y, matrix[1].y, matrix[2].y);
    let along_z = vec3(matrix[0].z, matrix[1].z, matrix[2].z);
    let texel = 1.0 / f32(textureDimensions(directional_shadow_textures).x);
    let cap = SHADOW_SLOPE_CAP_TEXELS * texel_world * length(along_z) / texel;
    let depth_step = max(dot(normal, along_z) / dot(along_z, along_z), 1e-6);
    let slope = clamp(vec2(-2.0, 2.0) * vec2(dot(normal, along_x) / dot(along_x, along_x), dot(normal, along_y) / dot(along_y, along_y)) / depth_step, vec2(-cap), vec2(cap));
    let layer = i32((*light).depth_texture_base_index + cascade_index);
    if !sharp {
        return shadow_tap(local.xy, vec2(0.0), local.z, slope, layer);
    }
    let radius = clamp(SOFT_EDGE_UNITS / texel_world, SOFT_EDGE_LEAST_TEXELS, SOFT_EDGE_MOST_TEXELS) * texel;
    let rim = radius + texel;
    let settled = shadow_tap(local.xy, vec2(0.0), local.z, slope, layer)
        + shadow_tap(local.xy, vec2(rim, 0.0), local.z, slope, layer)
        + shadow_tap(local.xy, vec2(0.0, rim), local.z, slope, layer)
        + shadow_tap(local.xy, vec2(-rim, 0.0), local.z, slope, layer)
        + shadow_tap(local.xy, vec2(0.0, -rim), local.z, slope, layer);
    if settled < 1e-3 {
        return 0.0;
    }
    if settled > 5.0 - 1e-3 {
        return 1.0;
    }
    // A disk of taps turned by a pattern fixed to the screen, so the edge is grain that stands still, not bands.
    let clip = view.clip_from_world * vec4(position, 1.0);
    let pixel = (clip.xy / clip.w * 0.5 + 0.5) * view.viewport.zw;
    let turn = 6.2831853 * fract(52.9829189 * fract(dot(pixel, vec2(0.06711056, 0.00583715))));
    var lit = 0.0;
    for (var tap = 0; tap < SOFT_EDGE_TAPS; tap++) {
        let out = sqrt((f32(tap) + 0.5) / f32(SOFT_EDGE_TAPS)) * radius;
        let angle = f32(tap) * GOLDEN_ANGLE + turn;
        lit += shadow_tap(local.xy, vec2(cos(angle), sin(angle)) * out, local.z, slope, layer);
    }
    return lit / f32(SOFT_EDGE_TAPS);
}

// The finest level holding the point, fading into the next one near its edge. Past the last level there is no shadow.
fn static_sun_map(position: vec4<f32>, normal: vec3<f32>) -> f32 {
    let none = lights.n_directional_lights;
    var best = none;
    var next = none;
    var best_edge = 0.0;
    for (var i = 0u; i < none; i += 1u) {
        if !is_cached_level(i) {
            continue;
        }
        let local = lights.directional_lights[i].cascades[0].clip_from_world * position;
        let eye = lights.directional_lights[i].cascades[0].clip_from_world * vec4(view.world_position.xyz, 1.0);
        let edge = max(abs(local.x - eye.x), abs(local.y - eye.y));
        if edge >= LEVEL_USABLE || max(abs(local.x), abs(local.y)) >= 1.0 {
            continue;
        }
        let texel = lights.directional_lights[i].cascades[0].texel_size;
        if best == none || texel < lights.directional_lights[best].cascades[0].texel_size {
            next = best;
            best = i;
            best_edge = edge;
        } else if next == none || texel < lights.directional_lights[next].cascades[0].texel_size {
            next = i;
        }
    }
    if best == none {
        return 1.0;
    }
    if next == none {
        let half_width = 0.5 * lights.directional_lights[best].cascades[0].texel_size * f32(textureDimensions(directional_shadow_textures).x);
        let fade = smoothstep(LAST_LEVEL_FADE_FROM, LAST_LEVEL_FADE_TO, distance(view.world_position.xyz, position.xyz) / half_width);
        if fade >= 1.0 {
            return 1.0;
        }
        return mix(cascade_shadow(best, 0u, position.xyz, normal, true), 1.0, fade);
    }
    let shadow = cascade_shadow(best, 0u, position.xyz, normal, true);
    if best_edge <= LEVEL_FADE_FROM {
        return shadow;
    }
    return mix(shadow, cascade_shadow(next, 0u, position.xyz, normal, true), (best_edge - LEVEL_FADE_FROM) / (LEVEL_USABLE - LEVEL_FADE_FROM));
}

fn sun_map(sun: u32, position: vec4<f32>, normal: vec3<f32>) -> f32 {
    if !casts_shadow(sun) {
        return 1.0;
    }
    let world = static_sun_map(position, normal);
    if world <= 0.0 {
        return 0.0;
    }
    return min(world, mover_sun_map(sun, position, normal));
}

fn mover_sun_map(sun: u32, position: vec4<f32>, normal: vec3<f32>) -> f32 {
    let light = &lights.directional_lights[sun];
    let view_z = dot(vec4(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), position);
    let index = shadows::get_cascade_index(sun, view_z);
    if index >= (*light).num_cascades {
        return 1.0;
    }
    // Past the nearest cascade a shadow is small on screen: one filtered tap.
    var shadow = cascade_shadow(sun, index, position.xyz, normal, index == 0u && sharp_filter());
    let next = index + 1u;
    if next < (*light).num_cascades {
        let far_bound = (*light).cascades[index].far_bound;
        let next_near = (1.0 - (*light).cascades_overlap_proportion) * far_bound;
        if -view_z >= next_near {
            shadow = mix(shadow, cascade_shadow(sun, next, position.xyz, normal, false), (-view_z - next_near) / (far_bound - next_near));
        }
    } else {
        let far_bound = (*light).cascades[index].far_bound;
        shadow = mix(shadow, 1.0, smoothstep(MOVER_FADE_FROM * far_bound, far_bound, -view_z));
    }
    return shadow;
}

// How much of a short ray towards the sun is clear in the depth buffer. The jitter stays the same each frame: Bevy's
// own changes per frame and shimmers without temporal smoothing.
fn contact_clear(world_position: vec3<f32>, frag_coord: vec2<f32>, to_sun: vec3<f32>) -> f32 {
@if(CONTACT_SHADOWS && DEPTH_PREPASS) {
    if contact_shadows_settings.linear_steps == 0u {
        return 1.0;
    }
    var march = raymarch::depth_ray_march_new_from_depth(vec2<f32>(textureDimensions(depth_prepass_texture)));
    raymarch::depth_ray_march_from_cs(&march, position_world_to_ndc(world_position));
    raymarch::depth_ray_march_to_ws(&march, world_position + to_sun * contact_shadows_settings.length);
    let far = smoothstep(CONTACT_STEPS_FULL_TO, CONTACT_STEPS_FEW_FROM, distance(view.world_position.xyz, world_position));
    march.linear_steps = u32(mix(f32(contact_shadows_settings.linear_steps), CONTACT_FEWEST_STEPS, far));
    march.depth_thickness_linear_z = contact_shadows_settings.thickness;
    march.march_behind_surfaces = true;
    march.jitter = interleaved_gradient_noise(frag_coord, 0u);
    let result = raymarch::depth_ray_march_march(&march);
    if result.hit {
        return clamp((result.hit_penetration_frac - 0.5) * 2.0, 0.0, 1.0);
    }
}
    return 1.0;
}

// The share of the sky straight above a point. The point moves off its surface first, so a wall's taps stay out of its own building.
fn sky_open(position: vec3<f32>, normal: vec3<f32>) -> f32 {
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if !is_sky_level(i) {
            continue;
        }
        let spread = SKY_KERNEL_TEXELS * lights.directional_lights[i].cascades[0].texel_size;
        let moved = position + (normal + lights.directional_lights[i].direction_to_light) * spread;
        let local = shadows::world_to_directional_light_local(i, 0u, vec4(moved, 1.0));
        if local.w == 0.0 {
            return 1.0;
        }
        let layer = i32(lights.directional_lights[i].depth_texture_base_index);
        let tap = SKY_KERNEL_TEXELS / f32(textureDimensions(directional_shadow_textures).x);
        return 0.5 * (sample_shadow_map_hardware(local.xy + vec2(tap, tap), local.z, layer)
            + sample_shadow_map_hardware(local.xy + vec2(-tap, -tap), local.z, layer));
    }
    return 1.0;
}

// x the sun's direct share, contact shadow in. y the sun map alone. z the sky left by what stands above the point.
fn sun_seen(position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> vec3<f32> {
    let open = mix(ROOFED_SKY, 1.0, sky_open(position.xyz, normal));
    let sun = sun_index();
    if sun >= lights.n_directional_lights {
        return vec3(0.0, 1.0, open);
    }
    let to_sun = lights.directional_lights[sun].direction_to_light;
    let facing = dot(normal, to_sun);
    if facing <= 0.0 {
        return vec3(0.0, 1.0, open);
    }
    let map = sun_map(sun, position, normal);
    var direct = map * saturate(facing / SUN_FACING_FADE);
    let contact = 1.0 - smoothstep(CONTACT_NEAR, CONTACT_FAR, distance(view.world_position.xyz, position.xyz));
    if direct > 0.0 && contact > 0.0 {
        direct *= mix(1.0, contact_clear(position.xyz, frag_coord, to_sun), contact);
    }
    return vec3(direct, mix(1.0, map, saturate(facing / SKY_FACING_FADE)), open);
}

// `diffuse` in display light, `specular` and `bounce` in linear light. `sky` is what a probe sees of the sky: x plus y, z, w
// along a direction; x below 0 without a probe to read.
struct LampLight {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
    bounce: vec3<f32>,
    sky: vec4<f32>,
}

fn no_lamps() -> LampLight {
    return LampLight(vec3(0.0), vec3(0.0), vec3(0.0), vec4(-1.0, 0.0, 0.0, 0.0));
}

// A point lamp that keeps a cube map of the movers is a Bevy light too. Its shadow depth bias field holds this mark, far
// above any real bias, plus the lamp's number in the book.
const LAMP_MARK: f32 = 1048576.0;
// The atlas side in units. Matches `lamps/atlas.rs`.
const ATLAS_UNITS: f32 = 128.0;
const ATLAS_GUARD: f32 = 3.0;
const ATLAS_UNMAPPED: u32 = 7u;
const ATLAS_NEAR_MARGIN: f32 = 1.0;
const MOVER_DEPTH_MARGIN: f32 = 2.0;
const LAMP_CORE_SQUARED: f32 = 0.09;
const LAMP_ENERGY: f32 = 1.4485;
// In the lamp's radii from the eye: it fades out over this range, and past it `lamps.rs` hides it.
const LAMP_FADE_FROM: f32 = 21.0;
const LAMP_FADE_TO: f32 = 30.0;
const LAMP_SMALLEST: f32 = 0.01;
// A lamp's source is this share of its radius.
const LAMP_SOURCE_SHARE: f32 = 0.08;
// The book of lamps rides in the material's anisotropy texture, four floats a texel: a header (the corner of a grid of
// cells over the map and a cell's side; the grid's sides and where the cells start), four texels for every lamp, a
// texel for each cell (where its list starts and how many lamps it holds) and the lists, four lamps a texel. A list
// holds the lamps whose light gets into the cell. The numbers match `lamps/cells.rs`.
const BOOK_WIDTH: u32 = 2048u;
const BOOK_HEADER: u32 = 2u;
const BOOK_RECORD: u32 = 4u;
// As many point lamps as `lamps.rs` gives a cube map of the movers at its highest level.
const MOST_MOVER_CUBES: u32 = 4u;

fn book_texel(in: VertexOutput, index: u32) -> vec4<f32> {
    let at = vec2(index % BOOK_WIDTH, index / BOOK_WIDTH);
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return textureLoad(bindless_textures_2d[material_indices[slot].anisotropy_texture], at, 0);
}
@elif(PBR_ANISOTROPY_TEXTURE_SUPPORTED) {
    return textureLoad(pbr_bindings::anisotropy_texture, at, 0);
}
@else {
    return vec4(0.0);
}
}

// Bevy's 1 x 1 default image without a book.
fn has_book(in: VertexOutput) -> bool {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return textureDimensions(bindless_textures_2d[material_indices[slot].anisotropy_texture]).x == BOOK_WIDTH;
}
@elif(PBR_ANISOTROPY_TEXTURE_SUPPORTED) {
    return textureDimensions(pbr_bindings::anisotropy_texture).x == BOOK_WIDTH;
}
@else {
    return false;
}
}

// Where the list of the cell around `position` starts in the book, and how many lamps it holds.
fn lamp_list(in: VertexOutput, position: vec3<f32>) -> vec2<u32> {
    if !has_book(in) {
        return vec2(0u);
    }
    let grid = book_texel(in, 0u);
    let sides = book_texel(in, 1u);
    let cell = floor((position - grid.xyz) / grid.w);
    if any(cell < vec3(0.0)) || any(cell >= sides.xyz) {
        return vec2(0u);
    }
    let at = vec3<u32>(cell);
    let size = vec3<u32>(sides.xyz);
    return vec2<u32>(book_texel(in, u32(sides.w) + (at.z * size.y + at.y) * size.x + at.x).xy);
}

// In display light on the surface: a lamp giving less adds no highlight. Its shadow keeps its soft edge: a room lit by
// many lamps has every one of them under any such share, and their hard edges showed as squares.
const LAMP_GLOSS_FROM: f32 = 0.03;

fn is_lamp(light_id: u32) -> bool {
    return clustered_lights.data[light_id].shadow_depth_bias >= LAMP_MARK;
}

// The atlas rides in the material's occlusion texture; without one that is Bevy's 1 x 1 default image.
fn atlas_side(in: VertexOutput) -> f32 {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return f32(textureDimensions(bindless_textures_2d[material_indices[slot].occlusion_texture]).x);
}
@else {
    return f32(textureDimensions(pbr_bindings::occlusion_texture).x);
}
}

fn atlas_texel(in: VertexOutput, at: vec2<i32>) -> f32 {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return textureLoad(bindless_textures_2d[material_indices[slot].occlusion_texture], at, 0).r;
}
@else {
    return textureLoad(pbr_bindings::occlusion_texture, at, 0).r;
}
}

// The lamp's edge is the sun's width in world units where the tile is fine enough. On a coarse tile it is this many
// texels at least, wide enough to round the texels' steps away, and never past the most: the taps stay near a face's
// guard band.
const LAMP_EDGE_LEAST_TEXELS: f32 = 1.25;
const LAMP_EDGE_MOST_TEXELS: f32 = 2.5;
const LAMP_EDGE_TAPS: i32 = 8;

// A vector in the axes of one face of a lamp's cube: x right, y up, z out of the lamp. Matches `lamps/atlas.rs`.
fn face_view(v: vec3<f32>, face: u32) -> vec3<f32> {
    switch face {
        case 0u { return vec3(-v.z, v.y, v.x); }
        case 1u { return vec3(v.z, v.y, -v.x); }
        case 2u { return vec3(v.x, -v.z, v.y); }
        case 3u { return vec3(v.x, v.z, -v.y); }
        case 4u { return v; }
        default { return vec3(-v.x, v.y, -v.z); }
    }
}

// `low` the face's corner and `size` its side in texels, `at` a place in the atlas.
fn atlas_nearest(in: VertexOutput, low: vec2<f32>, size: f32, at: vec2<f32>, depth: f32) -> f32 {
    return select(0.0, 1.0, atlas_texel(in, vec2<i32>(clamp(at, low + 1.0, low + size - 1.0))) >= depth);
}

fn atlas_blended(in: VertexOutput, low: vec2<f32>, size: f32, at: vec2<f32>, depth: f32) -> f32 {
    let spot = clamp(at, low + 1.0, low + size - 1.0) - 0.5;
    let base = floor(spot);
    let blend = spot - base;
    let corner = vec2<i32>(base);
    let a = select(0.0, 1.0, atlas_texel(in, corner) >= depth);
    let b = select(0.0, 1.0, atlas_texel(in, corner + vec2(1, 0)) >= depth);
    let c = select(0.0, 1.0, atlas_texel(in, corner + vec2(0, 1)) >= depth);
    let d = select(0.0, 1.0, atlas_texel(in, corner + vec2(1, 1)) >= depth);
    return mix(mix(a, b, blend.x), mix(c, d, blend.x), blend.y);
}

// What a tap `by` texels off the point is held against: how far the point's own surface lies along that tap's ray, so
// a tilted surface does not shade itself, never more than a texel of depth a texel off. Over the radius, as a texel is.
fn receiver_depth(seen: vec3<f32>, normal: vec3<f32>, by: vec2<f32>, span: f32, texel_world: f32, margin: f32, radius: f32) -> f32 {
    let reach = length(seen);
    let ray = vec3(seen.x + by.x * seen.z / span, seen.y - by.y * seen.z / span, seen.z);
    let slope = dot(normal, ray);
    var along = reach;
    if abs(slope) > 1e-6 {
        along = dot(normal, seen) / slope * length(ray);
    }
    let most = texel_world * length(by);
    return (clamp(along, reach - most, reach + most) - margin) / radius;
}

// `from_lamp` the point seen from the lamp; a texel holds a distance over the radius, as `lamp_atlas.wesl` draws it.
// `tile` the tile's corner and `size` a face's side, in texels; `turn` an angle fixed to the pixel.
fn atlas_shadow(in: VertexOutput, tile: vec2<f32>, size: f32, from_lamp: vec3<f32>, normal: vec3<f32>, radius: f32, texel_world: f32, margin: f32, turn: f32) -> f32 {
    let m = abs(from_lamp);
    var face = 0u;
    if m.x >= m.y && m.x >= m.z {
        face = select(1u, 0u, from_lamp.x > 0.0);
    } else if m.y >= m.z {
        face = select(3u, 2u, from_lamp.y > 0.0);
    } else {
        face = select(5u, 4u, from_lamp.z > 0.0);
    }
    let seen = face_view(from_lamp, face);
    let low = tile + vec2(f32(face % 3u), f32(face / 3u)) * size;
    let span = 0.5 * size - ATLAS_GUARD;
    let at = low + 0.5 * size + vec2(seen.x, -seen.y) / max(seen.z, 1e-4) * span;
    let tilted = face_view(normal, face);
    let edge = clamp(SOFT_EDGE_UNITS / texel_world, LAMP_EDGE_LEAST_TEXELS, LAMP_EDGE_MOST_TEXELS);
    let rim = edge + 1.0;
    var settled = atlas_nearest(in, low, size, at, receiver_depth(seen, tilted, vec2(0.0), span, texel_world, margin, radius));
    for (var side = 0; side < 4; side++) {
        let by = vec2(f32((side + 1) % 2), f32(side % 2)) * select(rim, -rim, side > 1);
        settled += atlas_nearest(in, low, size, at + by, receiver_depth(seen, tilted, by, span, texel_world, margin, radius));
    }
    if settled < 0.5 {
        return 0.0;
    }
    if settled > 4.5 {
        return 1.0;
    }
    // The sun's edge: a disk of taps turned by a pattern fixed to the screen, grain that stands still.
    var lit = 0.0;
    for (var tap = 0; tap < LAMP_EDGE_TAPS; tap++) {
        let out = sqrt((f32(tap) + 0.5) / f32(LAMP_EDGE_TAPS)) * edge;
        let angle = f32(tap) * GOLDEN_ANGLE + turn;
        let by = vec2(cos(angle), sin(angle)) * out;
        lit += atlas_blended(in, low, size, at + by, receiver_depth(seen, tilted, by, span, texel_world, margin, radius));
    }
    return lit / f32(LAMP_EDGE_TAPS);
}

// A lamp's shadow from one tile: `tile` its corner and a face's side in texels.
fn tile_shadow(in: VertexOutput, tile: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, lamp_position: vec3<f32>, lamp_radius: f32, distance_to: f32, facing: f32, turn: f32) -> f32 {
    let texel_world = 2.0 * distance_to / tile.z;
    let tilt = min(sqrt(saturate(1.0 - facing * facing)) / max(facing, 0.1), 4.0);
    let moved = position + normal * (texel_world * SHADOW_NORMAL_OFFSET);
    return atlas_shadow(in, tile.xy, tile.z, moved - lamp_position, normal, lamp_radius, texel_world, texel_world * SHADOW_BIAS * (1.0 + 0.5 * tilt), turn);
}

// A point lamp near the eye also keeps a Bevy cube map, with movers only.
fn mover_shadow(id: u32, position: vec3<f32>, normal: vec3<f32>, to_lamp: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    let light = &clustered_lights.data[id];
    let from_lamp = position + normal * ATLAS_NEAR_MARGIN + to_lamp * MOVER_DEPTH_MARGIN - (*light).position_radius.xyz;
    let m = abs(from_lamp);
    let major = max(m.x, max(m.y, m.z));
    let zw = -major * (*light).light_custom_data.xy + (*light).light_custom_data.zw;
    // The light's source radius field holds how far the lamp's cube map has faded in.
    return mix(1.0, sample_shadow_cubemap(from_lamp * vec3(1.0, 1.0, -1.0), major, zw.x / zw.y, id, frag_coord), saturate((*light).position_radius.w));
}

// The probe grid rides in the material's metal and roughness texture, Bevy's 1 x 1 default without one: two volumes side
// by side, each a tile per layer of the grid, a tile the grid's columns with a repeat at each side, half as many layers
// as columns. The tile count across and the bounce range match `probes.rs`; the spacing comes in the ambient's alpha.
const PROBE_TILES_WIDE: f32 = 12.0;
const PROBE_BOUNCE_RANGE: f32 = 2.0;
const PROBE_SMALLEST_ATLAS: f32 = 64.0;

fn probe_atlas_size(in: VertexOutput) -> vec2<f32> {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return vec2<f32>(textureDimensions(bindless_textures_2d[material_indices[slot].metallic_roughness_texture]));
}
@else {
    return vec2<f32>(textureDimensions(pbr_bindings::metallic_roughness_texture));
}
}

fn probe_tap(in: VertexOutput, size: vec2<f32>, volume: f32, layer: f32, x: f32, z: f32) -> vec4<f32> {
    let tile_side = size.x / (2.0 * PROBE_TILES_WIDE);
    let layers = (tile_side - 2.0) * 0.5;
    let wrapped = layer - layers * floor(layer / layers);
    let tile = vec2(wrapped - PROBE_TILES_WIDE * floor(wrapped / PROBE_TILES_WIDE), floor(wrapped / PROBE_TILES_WIDE)) * tile_side;
    let uv = (tile + vec2(volume * tile_side * PROBE_TILES_WIDE + x + 1.5, z + 1.5)) / size;
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return textureSampleLevel(bindless_textures_2d[material_indices[slot].metallic_roughness_texture], bindless_samplers_filtering[material_indices[slot].metallic_roughness_sampler], uv, 0.0);
}
@else {
    return textureSampleLevel(pbr_bindings::metallic_roughness_texture, pbr_bindings::metallic_roughness_sampler, uv, 0.0);
}
}

// The surface's own side of the grid: half a step out of the wall it lies on.
fn read_probes(in: VertexOutput, position: vec3<f32>, normal: vec3<f32>, sum: ptr<function, LampLight>) {
    let spacing = lights.ambient_color.a;
    let size = probe_atlas_size(in);
    if spacing <= 0.0 || size.x < PROBE_SMALLEST_ATLAS {
        return;
    }
    let columns = size.x / (2.0 * PROBE_TILES_WIDE) - 2.0;
    let cell = position / spacing + normal * 0.5;
    let layer = floor(cell.y);
    let x = cell.x - columns * floor(cell.x / columns);
    let z = cell.z - columns * floor(cell.z / columns);
    let up = cell.y - layer;
    let shown = mix(probe_tap(in, size, 0.0, layer, x, z).rgb, probe_tap(in, size, 0.0, layer + 1.0, x, z).rgb, up) * PROBE_BOUNCE_RANGE;
    (*sum).bounce = shown * shown;
    let sky = mix(probe_tap(in, size, 1.0, layer, x, z), probe_tap(in, size, 1.0, layer + 1.0, x, z), up);
    (*sum).sky = vec4(sky.r, sky.gba * 2.0 - 1.0);
}

// Light from the placed lamps and the probes, before the surface's colour: terrain's layers share one sum.
fn lamp_light(in: VertexOutput, position: vec3<f32>, normal: vec3<f32>, frag_coord: vec2<f32>, ranges: clustering::ClusterableObjectIndexRanges, gloss: bool, surface: vec3<f32>) -> LampLight {
    var sum = no_lamps();
    // Special-lit surfaces take only special lights, and no lamp is one.
    if surface.z > 0.5 {
        return sum;
    }
    let unit = atlas_side(in) / ATLAS_UNITS;
    let rough = surface.x;
    let eye = to_eye(position);
    let turn = 6.2831853 * fract(52.9829189 * fract(dot(frag_coord, vec2(0.06711056, 0.00583715))));
    var cube_lamps: array<u32, MOST_MOVER_CUBES>;
    var cube_ids: array<u32, MOST_MOVER_CUBES>;
    var cubes = 0u;
    for (var i = ranges.first_point_light_index_offset; i < ranges.first_rect_light_index_offset && cubes < MOST_MOVER_CUBES; i++) {
        let id = clustering::get_clusterable_object_id(i);
        let light = &clustered_lights.data[id];
        if (*light).shadow_depth_bias < LAMP_MARK || ((*light).flags & POINT_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) == 0u {
            continue;
        }
        cube_lamps[cubes] = u32((*light).shadow_depth_bias - LAMP_MARK);
        cube_ids[cubes] = id;
        cubes += 1u;
    }
    let list = lamp_list(in, position);
    var four = vec4(0.0);
    for (var i = 0u; i < list.y; i++) {
        if i % 4u == 0u {
            four = book_texel(in, list.x + i / 4u);
        }
        let lamp = u32(four[i % 4u]);
        let record = BOOK_HEADER + lamp * BOOK_RECORD;
        let place = book_texel(in, record);
        let lamp_position = place.xyz;
        let to_light = lamp_position - position;
        let d2 = dot(to_light, to_light);
        let lamp_radius = abs(place.w);
        let inverse_range = 1.0 / (lamp_radius * lamp_radius);
        let x2 = d2 * inverse_range;
        if x2 >= 1.0 {
            continue;
        }
        let distance_to = sqrt(max(d2, 1e-4));
        let to_lamp = to_light / distance_to;
        let facing = dot(normal, to_lamp);
        if facing <= 0.0 {
            continue;
        }
        var cone = 1.0;
        if place.w < 0.0 {
            let spot = book_texel(in, record + 2u);
            let along = vec3(spot.xy, sqrt(max(0.0, 1.0 - dot(spot.xy, spot.xy))) * sign(spot.z));
            let share = saturate(dot(-along, to_lamp) * abs(spot.z) + spot.w);
            cone = share * share;
        }
        let from_eye = length(lamp_position - view.world_position.xyz) * sqrt(inverse_range);
        cone *= 1.0 - smoothstep(LAMP_FADE_FROM, LAMP_FADE_TO, from_eye);
        let look = book_texel(in, record + 1u);
        let color = look.rgb;
        let shown = lamp_shown(color, x2, cone);
        let share = dot(shown, LUMINANCE) * facing;
        if share < LAMP_SMALLEST {
            continue;
        }
        var shadow = saturate(facing / SUN_FACING_FADE);
        if unit >= 1.0 {
            // A lamp near the eye holds a sharp tile too: its corner, its face side and how far the shadow has gone
            // over to it, in the record's last texel.
            let sharp = book_texel(in, record + 3u);
            var lit = 1.0;
            let packed = u32(look.w);
            let level = (packed >> 16u) & 15u;
            if sharp.w < 1.0 && level != ATLAS_UNMAPPED {
                let plain = vec3(vec2(f32(packed & 255u), f32((packed >> 8u) & 255u)) * unit, unit * f32(1u << level));
                lit = tile_shadow(in, plain, position, normal, lamp_position, lamp_radius, distance_to, facing, turn);
            }
            if sharp.w > 0.0 {
                lit = mix(lit, tile_shadow(in, sharp.xyz, position, normal, lamp_position, lamp_radius, distance_to, facing, turn), sharp.w);
            }
            shadow *= lit;
        }
        for (var cube = 0u; cube < cubes && shadow > 0.0; cube++) {
            if cube_lamps[cube] == lamp {
                shadow *= mover_shadow(cube_ids[cube], position, normal, to_lamp, frag_coord);
                break;
            }
        }
        if shadow <= 0.0 {
            continue;
        }
        let a = saturate(rough * rough + lamp_radius * LAMP_SOURCE_SHARE / (2.0 * distance_to));
        add_lamp(&sum, shown, color, x2, cone, shadow, facing, to_lamp, normal, eye, gloss && share >= LAMP_GLOSS_FROM, a);
    }
    read_probes(in, position, normal, &sum);
    return sum;
}

// `color` in linear light, `x2` the squared distance over the radius; the result in display light.
fn lamp_shown(color: vec3<f32>, x2: f32, cone: f32) -> vec3<f32> {
    let x = sqrt(x2);
    return sqrt(color) * ((1.0 - x2 * (3.0 - 2.0 * x)) * cone);
}

// `a` is the highlight's roughness, widened by the lamp's size.
fn add_lamp(sum: ptr<function, LampLight>, shown: vec3<f32>, color: vec3<f32>, x2: f32, cone: f32, shadow: f32, facing: f32, to_lamp: vec3<f32>, normal: vec3<f32>, eye: vec3<f32>, gloss: bool, a: f32) {
    (*sum).diffuse += shown * (shadow * facing);
    if gloss {
        let window = saturate(1.0 - x2 * x2);
        let attenuation = LAMP_ENERGY * LAMP_CORE_SQUARED / max(x2, LAMP_CORE_SQUARED) * window * window * cone;
        (*sum).specular += color * (attenuation * shadow) * ggx(normal, to_lamp, eye, facing, a, vec3(DIELECTRIC_F0));
    }
}

fn fresnel_five(cosine: f32) -> f32 {
    let f1 = 1.0 - cosine;
    let f2 = f1 * f1;
    return f2 * f2 * f1;
}

fn ggx(normal: vec3<f32>, to_light: vec3<f32>, eye: vec3<f32>, facing: f32, a: f32, f0: vec3<f32>) -> vec3<f32> {
    let a2 = a * a;
    let k = a * 0.5;
    let sum = to_light + eye;
    let halfway = sum * inverseSqrt(max(dot(sum, sum), 1e-12));
    let nh = saturate(dot(normal, halfway));
    let nv = max(dot(normal, eye), 1e-4);
    let vh = saturate(dot(eye, halfway));
    let d = nh * nh * (a2 - 1.0) + 1.0;
    let visibility = 0.25 / ((facing * (1.0 - k) + k) * (nv * (1.0 - k) + k));
    return min(a2 / (d * d) * visibility, GGX_PEAK) * (f0 + (vec3(1.0) - f0) * fresnel_five(vh)) * facing;
}

fn ao_with_bounce(ao: f32, albedo: vec3<f32>) -> vec3<f32> {
    let a = 2.0404 * albedo - 0.3324;
    let b = -4.7951 * albedo + 0.6417;
    let c = 2.7552 * albedo + 0.6903;
    return max(vec3(ao), ((ao * a + b) * ao + c) * ao);
}

// What lights a surface besides its lamps and probes: the sun in linear light, the zone's ambient in display light, the
// fills' mean colour in linear light and their sum on the surface's normal in display light.
struct Around {
    sun_color: vec3<f32>,
    to_sun: vec3<f32>,
    ambient: vec3<f32>,
    sky: vec3<f32>,
    fills: vec3<f32>,
}

fn view_around(normal: vec3<f32>, lamps: LampLight, surface: vec3<f32>) -> Around {
    let special = surface.z > 0.5;
    var around = Around(vec3(0.0), vec3(0.0, 1.0, 0.0), lights.ambient_color.rgb, vec3(0.0), vec3(0.0));
    let sun = sun_index();
    if sun < lights.n_directional_lights {
        // The map's sun is never special-lit, so such a surface only takes its own fills.
        around.sun_color = select(lights.directional_lights[sun].color.rgb, vec3(0.0), special);
        around.to_sun = lights.directional_lights[sun].direction_to_light;
    }
    var fill_count = 0.0;
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if !casts_shadow(i) && !is_sun(i) && (lights.directional_lights[i].cascades_overlap_proportion > 0.5) == special {
            let fill = lights.directional_lights[i].color.rgb;
            around.sky += fill;
            fill_count += 1.0;
            let way = lights.directional_lights[i].direction_to_light;
            // A special sun is unshadowed. Otherwise a fill reaches only as far as the probe sees sky its way; open sky reads
            // 0.5 + 0.5 up by the probes' weights.
            var sees = 1.0;
            if lamps.sky.x >= 0.0 && !special {
                sees = saturate((lamps.sky.x + dot(lamps.sky.yzw, way)) / max(0.5 + 0.5 * way.y, 0.1));
            }
            around.fills += sqrt(fill) * (saturate(dot(normal, way)) * sees);
        }
    }
    around.sky /= max(fill_count, 1.0);
    if fill_count == 0.0 && !special && any(around.sun_color > vec3(0.0)) {
        sky_from_ambient(&around, normal);
    }
    return around;
}

// A map lit by one sun has no fill to stand for its sky, so its shade took no sky light at all. The zone's ambient
// stands in: whole on a surface facing up, half on a wall, none facing down. A map without a sun has no sky to add.
fn sky_from_ambient(around: ptr<function, Around>, normal: vec3<f32>) {
    let ambient = (*around).ambient;
    (*around).sky = ambient * ambient;
    (*around).fills = ambient * (0.5 + 0.5 * normal.y);
}

fn surface_light(albedo: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>, around: Around) -> vec3<f32> {
    let special = surface.z > 0.5;
    let sun_color = around.sun_color;
    let to_sun = around.to_sun;
    let rough = surface.x;
    let metal = surface.y;
    let a = rough * rough;
    let f0 = mix(vec3(DIELECTRIC_F0), albedo, metal);
    let eye = to_eye(position);
    let reflected = reflect(-eye, normal);
    // Probes carry what the zone's flat ambient and the sky left under a roof stand in for without them.
    let probed = lamps.sky.x >= 0.0;
    let open = select(seen.z, saturate((seen.z - ROOFED_SKY) / (1.0 - ROOFED_SKY)), probed);
    let sky_seen = select(mix(SKY_SHADOWED, 1.0, seen.y) * open, 1.0, special);
    let zone = select(around.ambient, vec3(0.0), probed);
    let sky = around.sky;
    let fills = around.fills;
    let ground = GROUND_ALBEDO * sun_color;
    let facing = saturate(dot(normal, to_sun));
    // A surface turned from the sun learns nothing from the sun map, so it must not count as under open sky.
    var env = (zone * zone + mix(ground, sky, saturate(reflected.y * 0.5 + 0.5)) * seen.z) * mix(REFLECTION_SHADOWED, 1.0, seen.y * saturate(facing / SKY_FACING_FADE));
    if lamps.sky.x >= 0.0 {
        env = zone * zone + sky * saturate(lamps.sky.x + dot(lamps.sky.yzw, reflected)) + lamps.bounce;
    }
    // Added over a dark texture, an untinted reflection greys it. A rough surface's keeps the texture's hue.
    let shown_color = sqrt(albedo);
    let hue = shown_color / max(max(shown_color.r, max(shown_color.g, shown_color.b)), 1e-3);
    env *= mix(hue, vec3(1.0), max(smoothstep(0.4, 0.8, 1.0 - rough), metal));
    // A dark texel is grime for a highlight only. Taking it off the diffuse too would square the texture's contrast.
    let cavity = mix(saturate(dot(sqrt(albedo), LUMINANCE) * 4.0), 1.0, metal);
    let occlusion = ao;
    let ambient = (zone + fills * sky_seen) * sqrt(ao_with_bounce(occlusion, albedo));

    let direct = seen.x;
    let nv = saturate(dot(normal, eye));
    let share = vec3(1.0) - (f0 + (max(vec3(1.0 - rough), f0) - f0) * fresnel_five(nv));
    let direct_light = ambient + sqrt(sun_color) * (facing * direct) + lamps.diffuse;
    // A bounce adds in linear light, so it fills what is dark and leaves retail's colours where the map's lights already reach.
    let shown = min(sqrt(direct_light * direct_light + lamps.bounce * occlusion), vec3(DIFFUSE_LIMIT));
    var lit = shown * shown * albedo * share * (1.0 - metal);
    if gloss {
        let spec = sun_color * direct * ggx(normal, to_sun, eye, facing, saturate(a + SUN_DISC), f0);
        let e0 = rough * vec4(-1.0, -0.0275, -0.572, 0.022) + vec4(1.0, 0.0425, 1.04, -0.04);
        let ab = vec2(-1.04, 1.04) * (min(e0.x * e0.x, exp2(-9.28 * nv)) * e0.x + e0.y) + e0.zw;
        // A rough surface returns less of the sky than the fit says, which would otherwise film every colour.
        let rough_reflection = mix(1.0 - 0.6 * smoothstep(0.5, 1.0, rough), 1.0, metal);
        env *= rough_reflection * (f0 * ab.x + ab.y) * saturate(pow(nv + occlusion, exp2(-16.0 * rough * rough - 1.0)) - 1.0 + occlusion);
        // `lamps.specular` is a dielectric's, and terrain's layers share it, so a metal's colour comes on here.
        lit += (spec + env + lamps.specular * mix(vec3(1.0), albedo / DIELECTRIC_F0, metal)) * cavity;
    }
    return lit;
}

fn modern_surface(color: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>) -> vec3<f32> {
    return lit_surface(color, position, normal, seen, ao, gloss, lamps, surface, view_around(normal, lamps, surface));
}

// `color` is the texture as the renderer decoded it; the result is what the frame stores, with no tone curve.
fn lit_surface(color: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>, around: Around) -> vec3<f32> {
    let display = to_display(color);
    let lit = surface_light(display * display, position, normal, seen, ao, gloss, lamps, surface, around);
    return to_linear(sqrt(max(lit, vec3(0.0))));
}

// What blending shows of a see-through surface: its highlight and reflection stay whole at any opacity, which takes more
// alpha than the texture's.
fn modern_glass(color: vec4<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>) -> vec4<f32> {
    var without_highlight = lamps;
    without_highlight.specular = vec3(0.0);
    let matte = modern_surface(color.rgb, position, normal, seen, ao, false, without_highlight, surface);
    let highlight = max(modern_surface(color.rgb, position, normal, seen, ao, gloss, lamps, surface) - matte, vec3(0.0));
    let alpha = saturate(color.a + dot(highlight, LUMINANCE));
    return vec4((matte * color.a + highlight) / max(alpha, 1e-4), alpha);
}

// The frame's alpha tells the sky from the world for the sun's rays: a solid surface of the sky stores 0, the world keeps
// 1. The world's frame is blended over the sky's by its alpha, so the world must never store less. The sky's camera
// carries the mark in its ambient light's alpha, which nothing else reads there. Matches `modern.rs`.
const SKY_VIEW_MARK: f32 = -0.5;

fn sky_mask(material_flags: u32, alpha: f32) -> f32 {
    let mode = material_flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS;
    let solid = mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_OPAQUE || mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_MASK;
    return select(alpha, 0.0, solid && lights.ambient_color.a < SKY_VIEW_MARK);
}

fn is_see_through(material_flags: u32) -> bool {
    return (material_flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS) == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_BLEND;
}

// Moving lights that are not real-time shadowed yet: added in display space as retail's are.
fn with_light(lit: vec3<f32>, color: vec3<f32>, light: vec3<f32>) -> vec3<f32> {
    if all(light <= vec3(0.0)) {
        return lit;
    }
    return to_linear(to_display(lit) + to_display(color) * light);
}

fn to_eye(world_position: vec3<f32>) -> vec3<f32> {
    let to_camera = view.world_position.xyz - world_position;
    return to_camera / max(length(to_camera), 1e-6);
}

fn ambient_occlusion(frag_coord: vec2<f32>) -> f32 {
@if(SCREEN_SPACE_AMBIENT_OCCLUSION) {
    let ssao = textureLoad(screen_space_ambient_occlusion_texture, vec2<i32>(frag_coord - view.viewport.xy), 0i).r;
    return mix(1.0, ssao, AO_STRENGTH);
}
@else {
    return 1.0;
}
}

// Actors and retail dynamic lights keep these until they take the shared light.

struct Relief {
    normal: vec3<f32>,
    gloss: f32,
    shininess: f32,
}

fn highlight(relief: Relief, to_light: vec3<f32>, eye: vec3<f32>) -> f32 {
    let facing = dot(relief.normal, to_light);
    if facing <= 0.0 || relief.gloss <= 0.0 {
        return 0.0;
    }
    let halfway = normalize(to_light + eye);
    return relief.gloss * pow(max(dot(relief.normal, halfway), 0.0), relief.shininess) * saturate(4.0 * facing);
}

fn actor_highlight(diffuse: vec3<f32>, normal: vec3<f32>, to_light: vec3<f32>, eye: vec3<f32>) -> vec3<f32> {
    let halfway = normalize(to_light + eye);
    return ACTOR_GLOSS * diffuse * pow(max(dot(normal, halfway), 0.0), ACTOR_SHININESS);
}

fn actor_sun(sun: f32, facing: f32) -> f32 {
    return mix(1.0, sun, smoothstep(0.0, ACTOR_TERMINATOR_COS, facing));
}

// From the sun's cascaded map alone; 1 without a shadowed sun in this view.
fn sun_map_visibility(world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    let sun = sun_index();
    if sun >= lights.n_directional_lights || !casts_shadow(sun) {
        return 1.0;
    }
    let view_z = dot(vec4(
        view.view_from_world[0].z,
        view.view_from_world[1].z,
        view.view_from_world[2].z,
        view.view_from_world[3].z
    ), world_position);
    // The sun's cascades hold movers only; the static world is in the cached levels.
    return min(static_sun_map(world_position, normal), shadows::fetch_directional_shadow(sun, world_position, normal, view_z, frag_coord));
}

fn contact_sun_index() -> u32 {
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if (lights.directional_lights[i].flags & DIRECTIONAL_LIGHT_FLAGS_CONTACT_SHADOWS_ENABLED_BIT) != 0u {
            return i;
        }
    }
    return lights.n_directional_lights;
}

fn sun_visibility(world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    var visibility = sun_map_visibility(world_position, normal, frag_coord);
    let sun = contact_sun_index();
    if visibility > 0.0 && sun < lights.n_directional_lights {
        visibility *= contact_clear(world_position.xyz, frag_coord, lights.directional_lights[sun].direction_to_light);
    }
    return visibility;
}
