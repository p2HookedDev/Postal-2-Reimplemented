import bevy_pbr::render::{
    mesh_view_bindings::{view, lights, directional_shadow_textures},
    mesh_view_types::{DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT, DIRECTIONAL_LIGHT_FLAGS_CONTACT_SHADOWS_ENABLED_BIT},
    shadows,
    shadow_sampling::sample_shadow_map_hardware,
};
@if(CONTACT_SHADOWS)
import bevy_pbr::render::mesh_view_bindings::contact_shadows_settings;
@if(CONTACT_SHADOWS && DEPTH_PREPASS)
import bevy_pbr::{render::{mesh_view_bindings::depth_prepass_texture, utils::interleaved_gradient_noise, view_transformations::position_world_to_ndc}, ssr::raymarch};

// In shadow map texels: the depth margin and the move along the normal.
const SHADOW_BIAS: f32 = 1.0;
const SHADOW_NORMAL_OFFSET: f32 = 1.5;
// The radius of a shadow's soft edge in world units, the same in every level and cascade. A level whose texels are
// wider than this takes the least radius its texels allow.
const SOFT_EDGE_UNITS: f32 = 1.5;
const SOFT_EDGE_LEAST_TEXELS: f32 = 0.75;
const SOFT_EDGE_MOST_TEXELS: f32 = 8.0;
const SOFT_EDGE_TAPS: i32 = 12;
// Tap n unturned: sqrt((n + 0.5) / 12) out along the angle n times the golden angle, 2.3999632.
const SOFT_EDGE_DISK: array<vec2<f32>, 12> = array<vec2<f32>, 12>(
    vec2(0.204124145, 0.000000000),
    vec2(-0.260699246, 0.238821907),
    vec2(0.039904120, -0.454687799),
    vec2(0.328594758, 0.428593224),
    vec2(-0.603011434, -0.106664010),
    vec2(0.571224959, -0.363366728),
    vec2(-0.191062876, 0.710747244),
    vec2(-0.364379602, -0.701589271),
    vec2(0.790556879, 0.288709464),
    vec2(-0.822442295, 0.339492767),
    vec2(0.396471272, -0.847236998),
    vec2(0.292982695, 0.934074127),
);
// A receiver plane steeper than this many texels of depth per texel is noise.
const SHADOW_SLOPE_CAP_TEXELS: f32 = 16.0;
// In a level's half widths from the eye, not from its centre, which jumps at each redraw: a level is used out to where
// it surely holds the point, and blends into the next coarser one from `LEVEL_FADE_FROM`, as their shadows stand a
// texel's bias apart.
const LEVEL_FADE_FROM: f32 = 0.45;
const LEVEL_USABLE: f32 = 0.75;
// In world units: the most a shadow lookup moves along the normal, which a coarse level's wide texels would make a
// visible shift of the whole shadow.
const SHADOW_NORMAL_OFFSET_MOST: f32 = 8.0;
// In the outermost level's half widths from the eye: its shadows fade out over this range, inside what the level
// holds wherever its stepped centre stands.
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
// In the sky level's texels: how far its four taps spread, which softens a roof's edge.
const SKY_KERNEL_TEXELS: f32 = 2.0;
// The sky left under a roof, standing for the bounce no light here carries (a guess, not seen in game).
const ROOFED_SKY: f32 = 0.1;

// The sun's cascade overlap, which also tells it from the fills when it draws no shadow map. Matches `modern.rs`.
const SUN_OVERLAP: f32 = 0.1;

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
    return sun_found(sun_index());
}

fn sun_found(sun: u32) -> bool {
    return sun < lights.n_directional_lights;
}

fn view_z_of(position: vec4<f32>) -> f32 {
    return dot(vec4(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), position);
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
    let spin = mat2x2<f32>(cos(turn), sin(turn), -sin(turn), cos(turn));
    var lit = 0.0;
    for (var tap = 0; tap < SOFT_EDGE_TAPS; tap++) {
        lit += shadow_tap(local.xy, spin * SOFT_EDGE_DISK[tap] * radius, local.z, slope, layer);
    }
    return lit / f32(SOFT_EDGE_TAPS);
}

// The finest level holding the point, fading into the next one near its edge. Past the last level there is no shadow.
fn static_sun_map(position: vec4<f32>, normal: vec3<f32>) -> f32 {
    let none = lights.n_directional_lights;
    var best = none;
    var next = none;
    var best_edge = 0.0;
    var best_texel = 0.0;
    var next_texel = 0.0;
    let eye_position = vec4(view.world_position.xyz, 1.0);
    for (var i = 0u; i < none; i += 1u) {
        if !is_cached_level(i) {
            continue;
        }
        let level = &lights.directional_lights[i].cascades[0];
        let clip_from_world = (*level).clip_from_world;
        let local = (clip_from_world * position).xy;
        let eye = (clip_from_world * eye_position).xy;
        let edge = max(abs(local.x - eye.x), abs(local.y - eye.y));
        if edge >= LEVEL_USABLE || max(abs(local.x), abs(local.y)) >= 1.0 {
            continue;
        }
        let texel = (*level).texel_size;
        if best == none || texel < best_texel {
            next = best;
            next_texel = best_texel;
            best = i;
            best_texel = texel;
            best_edge = edge;
        } else if next == none || texel < next_texel {
            next = i;
            next_texel = texel;
        }
    }
    if best == none {
        return 1.0;
    }
    if next == none {
        let half_width = 0.5 * best_texel * f32(textureDimensions(directional_shadow_textures).x);
        let fade = smoothstep(LAST_LEVEL_FADE_FROM, LAST_LEVEL_FADE_TO, distance(view.world_position.xyz, position.xyz) / half_width);
        if fade >= 1.0 {
            return 1.0;
        }
        return mix(cascade_shadow(best, 0u, position.xyz, normal, sharp_filter()), 1.0, fade);
    }
    let shadow = cascade_shadow(best, 0u, position.xyz, normal, sharp_filter());
    if best_edge <= LEVEL_FADE_FROM {
        return shadow;
    }
    return mix(shadow, cascade_shadow(next, 0u, position.xyz, normal, sharp_filter()), (best_edge - LEVEL_FADE_FROM) / (LEVEL_USABLE - LEVEL_FADE_FROM));
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
    let view_z = view_z_of(position);
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
    return sun_seen_from(sun_index(), position, normal, frag_coord);
}

fn sun_seen_from(sun: u32, position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> vec3<f32> {
    let open = mix(ROOFED_SKY, 1.0, sky_open(position.xyz, normal));
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

// From the sun's cascaded map alone; 1 without a shadowed sun in this view.
fn sun_map_visibility(sun: u32, world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    if sun >= lights.n_directional_lights || !casts_shadow(sun) {
        return 1.0;
    }
    let view_z = view_z_of(world_position);
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

fn sun_visibility(sun: u32, world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    var visibility = sun_map_visibility(sun, world_position, normal, frag_coord);
    let contact_sun = contact_sun_index();
    if visibility > 0.0 && contact_sun < lights.n_directional_lights {
        visibility *= contact_clear(world_position.xyz, frag_coord, lights.directional_lights[contact_sun].direction_to_light);
    }
    return visibility;
}
