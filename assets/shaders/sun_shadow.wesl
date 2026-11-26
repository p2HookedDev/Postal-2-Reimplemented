import bevy_pbr::render::{
    mesh_view_bindings::{view, lights, directional_shadow_textures},
    mesh_view_types::{DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT, DIRECTIONAL_LIGHT_FLAGS_CONTACT_SHADOWS_ENABLED_BIT},
    shadows,
    shadow_sampling::sample_shadow_map_hardware,
};
import package::p2_render::sun_levels::{
    LevelAtlas, LevelRecord, LevelSource, atlas_quad, level_atlas, level_count, level_is_sky, level_local, level_record, level_rows_xy, level_tap, level_texel_size, level_xy, sky_tap,
};
@if(CONTACT_SHADOWS)
import bevy_pbr::render::mesh_view_bindings::contact_shadows_settings;
@if(CONTACT_SHADOWS && DEPTH_PREPASS)
import bevy_pbr::{render::{mesh_view_bindings::depth_prepass_texture, utils::interleaved_gradient_noise, view_transformations::position_world_to_ndc}, ssr::raymarch};

const SHADOW_BIAS: f32 = 1.0;
const SHADOW_NORMAL_OFFSET: f32 = 1.5;
@if(SHADOW_SOFTER)
const SOFT_EDGE_UNITS: f32 = 8.0;
@if(SHADOW_SOFT && !SHADOW_SOFTER)
const SOFT_EDGE_UNITS: f32 = 4.0;
@if(!SHADOW_SOFT && !SHADOW_SOFTER)
const SOFT_EDGE_UNITS: f32 = 1.5;
const SOFT_EDGE_LEAST_TEXELS: f32 = 0.75;
const SOFT_EDGE_MOST_TEXELS: f32 = 8.0;
const SOFT_EDGE_TAPS: i32 = 12;
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
const SHADOW_SLOPE_CAP_TEXELS: f32 = 16.0;
const LEVEL_FADE_FROM: f32 = 0.45;
const LEVEL_USABLE: f32 = 0.75;
const SHADOW_NORMAL_OFFSET_MOST: f32 = 8.0;
const LAST_LEVEL_FADE_FROM: f32 = 0.55;
const LAST_LEVEL_FADE_TO: f32 = 0.74;
const MOVER_FADE_FROM: f32 = 0.8;
const SUN_FACING_FADE: f32 = 0.05;
const SKY_FACING_FADE: f32 = 0.1;
const CONTACT_NEAR: f32 = 1024.0;
const CONTACT_FAR: f32 = 2048.0;
const CONTACT_STEPS_FULL_TO: f32 = 256.0;
const CONTACT_STEPS_FEW_FROM: f32 = 1024.0;
const CONTACT_FEWEST_STEPS: f32 = 4.0;
const SKY_KERNEL_TEXELS: f32 = 2.0;
const SKY_FLAT_MOST: f32 = 0.005;
const SKY_SLOPED_LEAST: f32 = 0.02;
const ROOFED_SKY: f32 = 0.1;

// The sun's cascade overlap, which also tells it from the fills when it draws no shadow map. Matches
// `modern.rs`.
const SUN_OVERLAP: f32 = 0.1;

const HELD_EDGE_LEAST_TEXELS: f32 = 2.75;
const HELD_EDGE_TAPS: i32 = 20;
const GOLDEN_ANGLE: f32 = 2.3999632;

struct ShadowLookup {
    offset: f32,
    sky: f32,
    seed: vec2<f32>,
}

fn world_lookup() -> ShadowLookup {
    return ShadowLookup(1.0, 1.0, vec2(0.0));
}

fn held_lookup(lookup: ShadowLookup) -> bool {
    return lookup.offset == 0.0;
}

fn disk_turn(seed: vec2<f32>) -> f32 {
    return 6.2831853 * fract(52.9829189 * fract(dot(seed, vec2(0.06711056, 0.00583715))));
}

fn vogel_tap(tap: i32, count: i32, turn: f32) -> vec2<f32> {
    let angle = f32(tap) * GOLDEN_ANGLE + turn;
    return vec2(cos(angle), sin(angle)) * sqrt((f32(tap) + 0.5) / f32(count));
}

fn casts_shadow(i: u32) -> bool {
    return (lights.directional_lights[i].flags & DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u;
}

fn is_sun(i: u32) -> bool {
    return abs(lights.directional_lights[i].cascades_overlap_proportion - SUN_OVERLAP) < 0.01;
}

fn sun_index() -> u32 {
    @if(!SKY_VIEW)
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if is_sun(i) {
            return i;
        }
    }
    return lights.n_directional_lights;
}

fn has_sun() -> bool {
    return sun_found(sun_index());
}

fn sun_found(sun: u32) -> bool {
    return sun < lights.n_directional_lights;
}

fn levels_read(sun: u32) -> bool {
    return sun_found(sun) && casts_shadow(sun);
}

fn view_z_of(position: vec4<f32>) -> f32 {
    return dot(vec4(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), position);
}

fn sharp_filter() -> bool {
@if(SHADOW_FILTER_METHOD_HARDWARE_2X2) {
    return false;
}
@else {
    return true;
}
}

struct ShadowSource {
    clip_from_world: mat4x4<f32>,
    texel_world: f32,
    to_sun: vec3<f32>,
    map_size: f32,
    sun: u32,
    cascade: u32,
    level: bool,
    record: LevelRecord,
}

fn cascade_source(sun: u32, cascade_index: u32) -> ShadowSource {
    let light = &lights.directional_lights[sun];
    let cascade = &(*light).cascades[cascade_index];
    return ShadowSource((*cascade).clip_from_world, (*cascade).texel_size, (*light).direction_to_light, f32(textureDimensions(directional_shadow_textures).x), sun, cascade_index, false, LevelRecord());
}

fn level_source(record: LevelRecord) -> ShadowSource {
    return ShadowSource(record.clip_from_world, record.texel_size, record.direction_to_light, record.size, 0u, 0u, true, record);
}

fn shadow_tap(atlas: LevelAtlas, shade: ShadowSource, uv: vec2<f32>, offset: vec2<f32>, depth: f32, slope: vec2<f32>) -> f32 {
    let at = depth + dot(offset, slope);
    if shade.level {
        return level_tap(atlas, shade.record, uv + offset, at);
    }
    return sample_shadow_map_hardware(uv + offset, at, i32(lights.directional_lights[shade.sun].depth_texture_base_index + shade.cascade));
}

const SUN_SPREAD: f32 = 0.02;
const BLOCKER_TAPS: i32 = 8;

@if(SHADOW_CONTACT_HARDENING)
fn raw_depth(atlas: LevelAtlas, shade: ShadowSource, uv: vec2<f32>) -> f32 {
    if shade.level {
        let spot = clamp(uv * shade.record.size - 0.5, vec2(0.0), vec2(shade.record.size - 2.0));
        let quad = atlas_quad(atlas, shade.record.corner + floor(spot) + 1.0);
        return max(max(quad.x, quad.y), max(quad.z, quad.w));
    }
    let size = vec2<i32>(textureDimensions(directional_shadow_textures).xy);
    let texel = clamp(vec2<i32>(uv * vec2<f32>(size)), vec2(0), size - 1);
    return textureLoad(directional_shadow_textures, texel, i32(lights.directional_lights[shade.sun].depth_texture_base_index + shade.cascade), 0);
}

@if(SHADOW_CONTACT_HARDENING)
fn caster_gap(atlas: LevelAtlas, shade: ShadowSource, uv: vec2<f32>, depth: f32, slope: vec2<f32>, depth_per_unit: f32, reach: f32) -> f32 {
    var sum = 0.0;
    var found = 0.0;
    for (var tap = 0; tap < BLOCKER_TAPS; tap++) {
        let offset = vogel_tap(tap, BLOCKER_TAPS, 0.0) * reach;
        let receiver = depth + dot(offset, slope);
        let stored = raw_depth(atlas, shade, uv + offset);
        if stored > receiver {
            sum += stored - receiver;
            found += 1.0;
        }
    }
    if found == 0.0 {
        return 0.0;
    }
    return sum / found / max(depth_per_unit, 1e-6);
}

fn cascade_shadow(atlas: LevelAtlas, shade: ShadowSource, position: vec3<f32>, normal: vec3<f32>, sharp: bool, lookup: ShadowLookup) -> f32 {
    let to_sun = shade.to_sun;
    let facing = dot(normal, to_sun);
    let tilt = min(sqrt(saturate(1.0 - facing * facing)) / max(facing, 0.1), 4.0);
    let texel_world = shade.texel_world;
    let margin = texel_world * SHADOW_BIAS * (1.0 + 0.5 * tilt);
    let moved = vec4(position + normal * (min(texel_world * SHADOW_NORMAL_OFFSET, SHADOW_NORMAL_OFFSET_MOST) * lookup.offset) + to_sun * (margin * lookup.offset), 1.0);
    var local = level_local(shade.record, moved);
    if !shade.level {
        local = shadows::world_to_directional_light_local(shade.sun, shade.cascade, moved);
    }
    if local.w == 0.0 {
        return 1.0;
    }
    let matrix = shade.clip_from_world;
    let along_x = vec3(matrix[0].x, matrix[1].x, matrix[2].x);
    let along_y = vec3(matrix[0].y, matrix[1].y, matrix[2].y);
    let along_z = vec3(matrix[0].z, matrix[1].z, matrix[2].z);
    let texel = 1.0 / shade.map_size;
    let cap = SHADOW_SLOPE_CAP_TEXELS * texel_world * length(along_z) / texel;
    let depth_step = max(dot(normal, along_z) / dot(along_z, along_z), 1e-6);
    let slope = clamp(vec2(-2.0, 2.0) * vec2(dot(normal, along_x) / dot(along_x, along_x), dot(normal, along_y) / dot(along_y, along_y)) / depth_step, vec2(-cap), vec2(cap));
    let held = held_lookup(lookup);
    if !sharp && !held {
        return shadow_tap(atlas, shade, local.xy, vec2(0.0), local.z, slope);
    }
    let least = select(SOFT_EDGE_LEAST_TEXELS, HELD_EDGE_LEAST_TEXELS, held);
    var radius = clamp(SOFT_EDGE_UNITS / texel_world, least, SOFT_EDGE_MOST_TEXELS) * texel;
    var rim = radius + texel;
@if(SHADOW_CONTACT_HARDENING) {
    if !held {
        rim = (SOFT_EDGE_MOST_TEXELS + 1.0) * texel;
    }
}
    let settled = shadow_tap(atlas, shade, local.xy, vec2(0.0), local.z, slope)
        + shadow_tap(atlas, shade, local.xy, vec2(rim, 0.0), local.z, slope)
        + shadow_tap(atlas, shade, local.xy, vec2(0.0, rim), local.z, slope)
        + shadow_tap(atlas, shade, local.xy, vec2(-rim, 0.0), local.z, slope)
        + shadow_tap(atlas, shade, local.xy, vec2(0.0, -rim), local.z, slope);
    if !held && settled < 1e-3 {
        return 0.0;
    }
    if !held && settled > 5.0 - 1e-3 {
        return 1.0;
    }
@if(SHADOW_CONTACT_HARDENING) {
    if !held {
        let gap = caster_gap(atlas, shade, local.xy, local.z, slope, length(along_z), rim - texel);
        radius = clamp((SOFT_EDGE_UNITS + gap * SUN_SPREAD) / texel_world, least, SOFT_EDGE_MOST_TEXELS) * texel;
    }
}
    if held {
        let turn = disk_turn(lookup.seed);
        var lit = 0.0;
        for (var tap = 0; tap < HELD_EDGE_TAPS; tap++) {
            lit += shadow_tap(atlas, shade, local.xy, vogel_tap(tap, HELD_EDGE_TAPS, turn) * radius, local.z, slope);
        }
        return lit / f32(HELD_EDGE_TAPS);
    }
    let clip = view.clip_from_world * vec4(position, 1.0);
    let pixel = (clip.xy / clip.w * 0.5 + 0.5) * view.viewport.zw;
    let turn = disk_turn(pixel);
    let spin = mat2x2<f32>(cos(turn), sin(turn), -sin(turn), cos(turn));
    var lit = 0.0;
    for (var tap = 0; tap < SOFT_EDGE_TAPS; tap++) {
        lit += shadow_tap(atlas, shade, local.xy, spin * SOFT_EDGE_DISK[tap] * radius, local.z, slope);
    }
    return lit / f32(SOFT_EDGE_TAPS);
}

fn static_sun_map(source: LevelSource, sun: u32, position: vec4<f32>, normal: vec3<f32>, lookup: ShadowLookup) -> f32 {
    if !levels_read(sun) {
        return 1.0;
    }
    let atlas = level_atlas(source);
    let count = level_count(atlas);
    var best = count;
    var next = count;
    var best_edge = 0.0;
    var best_texel = 0.0;
    var next_texel = 0.0;
    let eye_position = vec4(view.world_position.xyz, 1.0);
    for (var i = 0u; i < count; i += 1u) {
        if level_is_sky(atlas, i) {
            continue;
        }
        let rows = level_rows_xy(atlas, i);
        let local = level_xy(rows, position);
        let eye = level_xy(rows, eye_position);
        let edge = max(abs(local.x - eye.x), abs(local.y - eye.y));
        if edge >= LEVEL_USABLE || max(abs(local.x), abs(local.y)) >= 1.0 {
            continue;
        }
        let texel = level_texel_size(atlas, i);
        if best == count || texel < best_texel {
            next = best;
            next_texel = best_texel;
            best = i;
            best_texel = texel;
            best_edge = edge;
        } else if next == count || texel < next_texel {
            next = i;
            next_texel = texel;
        }
    }
    if best == count {
        return 1.0;
    }
    let best_record = level_record(atlas, best);
    if next == count {
        let half_width = 0.5 * best_record.texel_size * best_record.size;
        let fade = smoothstep(LAST_LEVEL_FADE_FROM, LAST_LEVEL_FADE_TO, distance(view.world_position.xyz, position.xyz) / half_width);
        if fade >= 1.0 {
            return 1.0;
        }
        return mix(cascade_shadow(atlas, level_source(best_record), position.xyz, normal, sharp_filter(), lookup), 1.0, fade);
    }
    let shadow = cascade_shadow(atlas, level_source(best_record), position.xyz, normal, sharp_filter(), lookup);
    if best_edge <= LEVEL_FADE_FROM {
        return shadow;
    }
    let next_record = level_record(atlas, next);
    return mix(shadow, cascade_shadow(atlas, level_source(next_record), position.xyz, normal, sharp_filter(), lookup), (best_edge - LEVEL_FADE_FROM) / (LEVEL_USABLE - LEVEL_FADE_FROM));
}

fn sun_map(source: LevelSource, sun: u32, position: vec4<f32>, normal: vec3<f32>, lookup: ShadowLookup) -> f32 {
    if !casts_shadow(sun) {
        return 1.0;
    }
    let world = static_sun_map(source, sun, position, normal, lookup);
    if world <= 0.0 {
        return 0.0;
    }
    return min(world, mover_sun_map(sun, position, normal, lookup));
}

fn mover_sun_map(sun: u32, position: vec4<f32>, normal: vec3<f32>, lookup: ShadowLookup) -> f32 {
    let light = &lights.directional_lights[sun];
    let view_z = view_z_of(position);
    let index = shadows::get_cascade_index(sun, view_z);
    if index >= (*light).num_cascades {
        return 1.0;
    }
    let atlas = LevelAtlas();
    let last = (*light).num_cascades - 1u;
    var shadow = cascade_shadow(atlas, cascade_source(sun, index), position.xyz, normal, index < last && sharp_filter(), lookup);
    let next = index + 1u;
    if next < (*light).num_cascades {
        let far_bound = (*light).cascades[index].far_bound;
        let next_near = (1.0 - (*light).cascades_overlap_proportion) * far_bound;
        if -view_z >= next_near {
            shadow = mix(shadow, cascade_shadow(atlas, cascade_source(sun, next), position.xyz, normal, next < last && sharp_filter(), lookup), (-view_z - next_near) / (far_bound - next_near));
        }
    } else {
        let far_bound = (*light).cascades[index].far_bound;
        shadow = mix(shadow, 1.0, smoothstep(MOVER_FADE_FROM * far_bound, far_bound, -view_z));
    }
    return shadow;
}

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

fn sky_open(source: LevelSource, sun: u32, position: vec3<f32>, normal: vec3<f32>) -> f32 {
    if !levels_read(sun) {
        return 1.0;
    }
    let atlas = level_atlas(source);
    let count = level_count(atlas);
    for (var i = 0u; i < count; i += 1u) {
        if !level_is_sky(atlas, i) {
            continue;
        }
        let record = level_record(atlas, i);
        let spread = SKY_KERNEL_TEXELS * record.texel_size;
        let moved = position + (normal + record.direction_to_light) * spread;
        let local = level_local(record, vec4(moved, 1.0));
        if local.w == 0.0 {
            return 1.0;
        }
        let depth = level_local(record, vec4(moved - record.direction_to_light * (spread + record.texel_size), 1.0)).z;
        let tap = SKY_KERNEL_TEXELS / record.size;
        let across = cross(normal, record.direction_to_light);
        let sloped = smoothstep(SKY_FLAT_MOST, SKY_SLOPED_LEAST, dot(across, across));
        let away = normal - record.direction_to_light * dot(normal, record.direction_to_light);
        let to_map = vec2(0.5, -0.5) * spread;
        let along = (record.clip_from_world * vec4(across * inverseSqrt(max(dot(across, across), 1e-8)) * sqrt(2.0), 0.0)).xy * to_map;
        let down = (record.clip_from_world * vec4(away * inverseSqrt(max(dot(away, away), 1e-8)), 0.0)).xy * to_map;
        let step = mix(vec2(tap, tap), along, sloped);
        let first = mix(vec2(tap, -tap), 0.5 * (down + step), sloped);
        let second = mix(vec2(-tap, tap), 0.5 * (down - step), sloped);
        let taps = sky_tap(atlas, record, local.xy + step, depth) + sky_tap(atlas, record, local.xy - step, depth)
            + sky_tap(atlas, record, local.xy + first, depth) + sky_tap(atlas, record, local.xy + second, depth);
        return select(1.0, taps.x / taps.y, taps.y > 0.0);
    }
    return 1.0;
}

fn sun_seen(source: LevelSource, position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> vec3<f32> {
    return sun_seen_from(source, sun_index(), position, normal, frag_coord, world_lookup());
}

fn sun_seen_from(source: LevelSource, sun: u32, position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>, lookup: ShadowLookup) -> vec3<f32> {
    return sun_faced(sun, normal, sun_held(source, sun, position, normal, frag_coord, lookup));
}

fn sun_held(source: LevelSource, sun: u32, position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>, lookup: ShadowLookup) -> vec3<f32> {
    var sky = lookup.sky;
    if !held_lookup(lookup) {
        sky = sky_open(source, sun, position.xyz, normal);
    }
    let open = mix(ROOFED_SKY, 1.0, sky);
    if sun >= lights.n_directional_lights {
        return vec3(0.0, 1.0, open);
    }
    let to_sun = lights.directional_lights[sun].direction_to_light;
    if dot(normal, to_sun) <= 0.0 {
        return vec3(0.0, 1.0, open);
    }
    let map = sun_map(source, sun, position, normal, lookup);
    var direct = map;
    let contact = 1.0 - smoothstep(CONTACT_NEAR, CONTACT_FAR, distance(view.world_position.xyz, position.xyz));
    // The march and the map see the same casters: the darker of the two, never both, or a person's shadow doubles.
    if direct > 0.0 && contact > 0.0 {
        direct = min(direct, mix(1.0, contact_clear(position.xyz, frag_coord, to_sun), contact));
    }
    return vec3(direct, map, open);
}

fn sun_faced(sun: u32, normal: vec3<f32>, held: vec3<f32>) -> vec3<f32> {
    if sun >= lights.n_directional_lights {
        return vec3(0.0, 1.0, held.z);
    }
    let facing = dot(normal, lights.directional_lights[sun].direction_to_light);
    if facing <= 0.0 {
        return vec3(0.0, 1.0, held.z);
    }
    return vec3(held.x * saturate(facing / SUN_FACING_FADE), mix(1.0, held.y, saturate(facing / SKY_FACING_FADE)), held.z);
}

fn sun_map_visibility(source: LevelSource, sun: u32, world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    if sun >= lights.n_directional_lights || !casts_shadow(sun) {
        return 1.0;
    }
    let view_z = view_z_of(world_position);
    return min(static_sun_map(source, sun, world_position, normal, world_lookup()), shadows::fetch_directional_shadow(sun, world_position, normal, view_z, frag_coord));
}

fn contact_sun_index() -> u32 {
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if (lights.directional_lights[i].flags & DIRECTIONAL_LIGHT_FLAGS_CONTACT_SHADOWS_ENABLED_BIT) != 0u {
            return i;
        }
    }
    return lights.n_directional_lights;
}

fn sun_visibility(source: LevelSource, sun: u32, world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    var visibility = sun_map_visibility(source, sun, world_position, normal, frag_coord);
    let contact_sun = contact_sun_index();
    if visibility > 0.0 && contact_sun < lights.n_directional_lights {
        visibility = min(visibility, contact_clear(world_position.xyz, frag_coord, lights.directional_lights[contact_sun].direction_to_light));
    }
    return visibility;
}
