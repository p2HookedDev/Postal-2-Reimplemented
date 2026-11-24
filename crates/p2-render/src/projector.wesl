// Projector decals (tagged by `projector_tag`) are retail's projector pass over the finished surface,
// unlit in display space: `project`, after the lighting. Every clustered decal of the game is a projector or a wound,
// both tagged as projectors, so no decal blends into the base colour before the lighting.

import bevy_pbr::render::{
    pbr_types::{PbrInput, STANDARD_MATERIAL_FLAGS_UNLIT_BIT},
    clustered_forward,
    clustered_forward::ClusterableObjectIndexRanges,
    mesh_functions::get_tag,
    mesh_view_bindings,
};
import bevy_pbr::decal::clustered::get_view_z;
import package::p2_render::color::{to_display, to_linear};
@if(CLUSTERED_DECALS_ARE_USABLE)
import bevy_pbr::decal::clustered::{clustered_decal_iterator_new, clustered_decal_iterator_next};

const PROJECTOR: u32 = 0x80000000u;
// `EProjectorBlending` (the material stage, tag bits 8..16).
const PB_MODULATE: u32 = 1u;
const PB_ALPHA_BLEND: u32 = 2u;
const PB_ADD: u32 = 3u;
// `EFrameBufferBlending` (tag bits 0..8).
const FB_OVERWRITE: u32 = 0u;
const FB_MODULATE: u32 = 1u;
const FB_ALPHA_BLEND: u32 = 2u;
const FB_ALPHA_MODULATE: u32 = 3u;
const FB_TRANSLUCENT: u32 = 4u;
const FB_DARKEN: u32 = 5u;
const FB_BRIGHTEN: u32 = 6u;
// Ours, used by no game projector: a pool of liquid, which the world's surfaces take before their light (`pool_at`).
const FB_LIQUID: u32 = 7u;
// The alpha test bit; `AlphaRef` in bits 22..30.
const ALPHA_TEST: u32 = 0x200000u;
// What a surface is, for the projector's targets (tag bits 16..21, `ProjectorActor::targets`): its shader passes one.
const TARGET_BSP: u32 = 0x10000u;
const TARGET_TERRAIN: u32 = 0x20000u;
const TARGET_ACTOR: u32 = 0x80000u;
const TARGET_MESH: u32 = 0xc0000u;
const TARGETS: u32 = 0xf0000u;
const ON_UNLIT: u32 = 0x100000u;
const TAGGED: u32 = 0x40000000u;
// The `MeshTag` bits of a section (map/spawned.rs `projector_mesh_tag`): its actor is in no projector's list, so it
// refuses them; or it is in one, so it also takes a tagged projector. No tag at all takes untagged ones only: merged
// static meshes, whose actors all accept and carry no tag a projector aims at.
const NO_PROJECTORS: u32 = 1u;
const ATTACHED: u32 = 2u;
// A static mesh lit as an actor still takes static mesh projectors.
const STATIC_MESH: u32 = 4u;
const MOVER: u32 = 8u;
// In a projector's tag: it follows a mover or Karma prop and draws on those only; in a surface: it is one.
const ON_MOVER: u32 = 0x80u;
// Modern decal aging (map/decal_aging.rs, render/wounds.rs): blood drying in bits 3..7, fading out in bits 10..15;
// 0 = as retail. A wound (bit 15) on actors also takes the bodies that refuse the game's projectors.
const DRY_SHIFT: u32 = 3u;
const DRY_MASK: u32 = 0xfu;
const FADE_SHIFT: u32 = 10u;
const FADE_MASK: u32 = 0x1fu;
const WOUND: u32 = 0x8000u;
const DRY_TINT: vec3<f32> = vec3(0.6, 0.42, 0.36);

fn section_target(instance_index: u32, kind: u32) -> u32 {
    let tag = get_tag(instance_index);
    if (tag & NO_PROJECTORS) != 0u {
        return 0u;
    }
    var surface = kind;
    if (tag & STATIC_MESH) != 0u {
        surface = TARGET_MESH;
    }
    if (tag & ATTACHED) != 0u {
        surface |= TAGGED;
    }
    if (tag & MOVER) != 0u {
        surface |= ON_MOVER;
    }
    return surface;
}

fn mesh_target(instance_index: u32) -> u32 {
    return section_target(instance_index, TARGET_MESH);
}

fn actor_target(instance_index: u32) -> u32 {
    return section_target(instance_index, TARGET_ACTOR);
}

fn cluster_ranges(pbr_input: PbrInput) -> ClusterableObjectIndexRanges {
@if(SKY_VIEW) {
    var none: ClusterableObjectIndexRanges;
    return none;
}
@if(!SKY_VIEW) {
    let view_z = get_view_z(pbr_input.world_position.xyz);
    let cluster_index = clustered_forward::view_fragment_cluster_index(pbr_input.frag_coord.xy, view_z, pbr_input.is_orthographic);
    return clustered_forward::unpack_clusterable_object_index_ranges(cluster_index);
}
}

@if(CLUSTERED_DECALS_ARE_USABLE)
fn decal_texel(index: i32, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(
        mesh_view_bindings::clustered_decal_textures[index],
        mesh_view_bindings::clustered_decal_sampler,
        uv,
        0.0
    );
}

@if(CLUSTERED_DECALS_ARE_USABLE)
fn faces_projector(offset: i32, normal: vec3<f32>) -> bool {
    let index = clustered_forward::get_clusterable_object_id(u32(offset));
    let m = mesh_view_bindings::clustered_decals.decals[index].local_from_world;
    let direction = -normalize(vec3(m[0].z, m[1].z, m[2].z));
    return dot(normalize(normal), direction) < -0.0001;
}

// Liquid standing on a floor (`liquid_layer.wesl`): what of the floor's colour shows through it and what it scatters
// back, how wet the floor is under it, the liquid's surface normal and its roughness.
struct Pool {
    through: vec3<f32>,
    color: vec3<f32>,
    wet: f32,
    normal: vec3<f32>,
    roughness: f32,
    // What colours the highlight and the reflection: a thin oil film's interference.
    sheen: vec3<f32>,
}

// The atlas (`liquids/layer.rs`): slots of 128 texels with a 2-texel apron, 8 to a row, 2 Unreal units a texel.
const POOL_SIDE: f32 = 128.0;
const POOL_APRON: i32 = 2;
const POOL_SLOT_SIDE: i32 = 132;
const POOL_SLOTS_SIDE: u32 = 8u;
const POOL_TEXEL_UU: f32 = 2.0;
const POOL_FILM: f32 = 0.3;
// Puke is a paste: it stands this deep (`liquid_layer.wesl` `film_of`).
const POOL_PASTE_FILM: f32 = 0.8;
// The edge lies where the depth is this share of the film; it is this many Unreal units soft at the least, and world
// noise moves it this far in and out.
const POOL_EDGE: f32 = 0.25;
const POOL_SOFT_UU: f32 = 0.6;
const POOL_WARP_UU: f32 = 2.5;
// A texel whose floor lies this far off the fragment's height is another surface: a wall, a kerb's other side, another
// floor. Only the texels on the fragment's own floor are read, so liquid runs up to walls and kerbs and stops there.
const POOL_REACH_UU: f32 = 8.0;
// Seconds of one phase of running liquid's ripples, their size in Unreal units, and how much they tilt the surface.
const POOL_FLOW_PERIOD: f32 = 1.2;
const POOL_RIPPLE_UU: f32 = 5.0;
const POOL_RIPPLE: f32 = 0.15;
// Unreal units a second at which running liquid ripples fully.
const POOL_RUNNING: f32 = 6.0;
// How much a soaking liquid darkens the floor, the most on a dark texel.
const SOAKED: f32 = 0.6;
const POOL_BLOOD: u32 = 3u;
// Dried blood: the colour it scatters and its roughness.
const POOL_DRIED: vec3<f32> = vec3(0.05, 0.012, 0.005);
const POOL_DRIED_ROUGHNESS: f32 = 0.45;
const POOL_GAS: u32 = 1u;
const POOL_PUKE: u32 = 4u;
const POOL_BLOODY_PUKE: u32 = 5u;
const POOL_NAPALM: u32 = 8u;
// An oil film's thickness in nanometres, the spread world noise gives it, and how strongly it colours the highlight.
const POOL_OIL_NM: f32 = 380.0;
const POOL_OIL_SPREAD_NM: f32 = 260.0;
const POOL_OIL: f32 = 0.85;
// A paste's lumps: their sizes in Unreal units, how much they tilt the surface, how much thicker a chunk stands.
const POOL_LUMP_UU: vec2<f32> = vec2(6.0, 2.2);
const POOL_LUMP_TILT: f32 = 0.7;
const POOL_CHUNK_UU: f32 = 1.2;
// Puke's colours: a yellow and a brown it mottles between, and the lighter and the orange bits in it.
const POOL_PUKE_YELLOW: vec3<f32> = vec3(0.30, 0.26, 0.06);
const POOL_PUKE_BROWN: vec3<f32> = vec3(0.20, 0.14, 0.04);
const POOL_PUKE_BITS: vec3<f32> = vec3(0.44, 0.36, 0.15);
const POOL_PUKE_CARROT: vec3<f32> = vec3(0.50, 0.20, 0.04);

// What a liquid on the floor absorbs a Unreal unit (per channel), the linear colour it scatters back, its roughness.
struct PoolOptics {
    sigma: vec3<f32>,
    body: vec3<f32>,
    roughness: f32,
}

// By `FluidType` byte; row 0 stands for a kind the table does not know. A private var, as only memory may be indexed
// by a value known at run time.
var<private> POOL_OPTICS: array<PoolOptics, 9> = array<PoolOptics, 9>(
    PoolOptics(vec3(1.0, 1.0, 1.0), vec3(0.05, 0.05, 0.05), 0.1),
    // Gas: clear, faintly amber.
    PoolOptics(vec3(0.02, 0.05, 0.15), vec3(0.0), 0.03),
    // Urine: the floor through clear yellow.
    PoolOptics(vec3(0.10, 0.33, 1.4), vec3(0.02, 0.016, 0.0), 0.04),
    // Blood: deep red, a thin rim lighter.
    PoolOptics(vec3(4.0, 40.0, 45.0), vec3(0.12, 0.001, 0.0006), 0.07),
    // Puke: a paste.
    PoolOptics(vec3(1.5, 2.0, 4.0), vec3(0.30, 0.24, 0.07), 0.35),
    // BloodyPuke
    PoolOptics(vec3(2.5, 8.0, 10.0), vec3(0.14, 0.02, 0.003), 0.3),
    // Gonorrhea
    PoolOptics(vec3(1.0, 0.8, 2.5), vec3(0.25, 0.28, 0.06), 0.2),
    // BloodyUrine
    PoolOptics(vec3(0.5, 4.0, 5.0), vec3(0.06, 0.012, 0.001), 0.05),
    // Napalm
    PoolOptics(vec3(0.8, 1.6, 3.0), vec3(0.10, 0.06, 0.01), 0.1),
);

fn no_pool(normal: vec3<f32>) -> Pool {
    return Pool(vec3(1.0), vec3(0.0), 0.0, normal, 1.0, vec3(1.0));
}

// The colour a thin film of `nm` nanometres reflects, seen at a cosine `cos_v` off its normal.
fn thin_film(cos_v: f32, nm: f32) -> vec3<f32> {
    let cos_t = sqrt(1.0 - (1.0 - cos_v * cos_v) / 1.96);
    return 0.5 + 0.5 * cos(6.2831853 * 2.8 * nm * cos_t / vec3(650.0, 530.0, 450.0));
}

fn pool_hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}

fn pool_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let low = mix(pool_hash(i), pool_hash(i + vec2(1.0, 0.0)), u.x);
    let high = mix(pool_hash(i + vec2(0.0, 1.0)), pool_hash(i + vec2(1.0, 1.0)), u.x);
    return mix(low, high, u.y);
}

// The slope of `pool_noise` at `p`, per unit of `p`.
fn pool_noise_slope(p: vec2<f32>) -> vec2<f32> {
    let here = pool_noise(p);
    return vec2(pool_noise(p + vec2(0.05, 0.0)) - here, pool_noise(p + vec2(0.0, 0.05)) - here) / 0.05;
}

// Cubic B-spline weights of the four texels around a sample a fraction `f` past the second, and their slopes.
fn spline_weights(f: f32) -> vec4<f32> {
    let g = 1.0 - f;
    return vec4(g * g * g, 3.0 * f * f * f - 6.0 * f * f + 4.0, -3.0 * f * f * f + 3.0 * f * f + 3.0 * f + 1.0, f * f * f) / 6.0;
}

fn spline_slopes(f: f32) -> vec4<f32> {
    let g = 1.0 - f;
    return vec4(-0.5 * g * g, 1.5 * f * f - 2.0 * f, -1.5 * f * f + f + 0.5, 0.5 * f * f);
}

// A liquid decal with this bit is a run down a wall (`liquids/run_draw.rs`): its row of the run atlas in bits 8 to
// 13, its kind in 22 to 25 and its point count in 26 to 30. A row holds `RUN_POINTS` texels of (u, v, radius, wetness).
const RUN_FLAG: u32 = 0x200000u;
const RUN_POINTS: i32 = 16;
// How thick a run's liquid stands at its middle against its radius, how far world noise moves its edge, and how much
// more a splat's head.
const RUN_THICKNESS: f32 = 0.5;
const RUN_WARP_UU: f32 = 0.35;
const RUN_SPLAT_WARP: f32 = 0.35;
// A wall that leans this far from upright at most takes runs.
const RUN_STEEP: f32 = 0.7;

// The run of the decal the iterator stands on, at the fragment: a rounded bead along its trail.
fn run_at(index: i32, tag: u32, uv: vec2<f32>, offset: i32, world: vec3<f32>, normal: vec3<f32>) -> Pool {
    var pool = no_pool(normal);
    let row = i32((tag >> 8u) & 0x3fu);
    let count = i32((tag >> 26u) & 0x1fu);
    let kind = (tag >> 22u) & 0xfu;
    let m = mesh_view_bindings::clustered_decals.decals[clustered_forward::get_clusterable_object_id(u32(offset))].local_from_world;
    let size = vec2(1.0 / length(vec3(m[0].x, m[1].x, m[2].x)), 1.0 / length(vec3(m[0].y, m[1].y, m[2].y)));
    let along_u = normalize(vec3(m[0].x, m[1].x, m[2].x));
    let along_v = -normalize(vec3(m[0].y, m[1].y, m[2].y));
    let here = uv * size;
    // The nearest point of the trail, its radius there, wetness, and the way from it to the fragment.
    var best = 1e9;
    var radius = 0.0;
    var wetness = 0.0;
    var way = vec2(0.0);
    var head = 0.0;
    var a = textureLoad(mesh_view_bindings::clustered_decal_textures[index], vec2(0, row), 0);
    for (var k = 0; k < RUN_POINTS; k++) {
        if k >= max(count, 1) {
            break;
        }
        let b = textureLoad(mesh_view_bindings::clustered_decal_textures[index], vec2(min(k + 1, count - 1), row), 0);
        let pa = a.xy * size;
        let pb = b.xy * size;
        let ab = pb - pa;
        let t = select(0.0, saturate(dot(here - pa, ab) / dot(ab, ab)), dot(ab, ab) > 1e-8);
        let closest = pa + ab * t;
        let r = mix(a.z, b.z, t);
        let d = distance(here, closest);
        if d - r < best {
            best = d - r;
            radius = r;
            wetness = mix(a.w, b.w, t);
            way = here - closest;
            head = select(0.0, 1.0 - t, k == 0);
        }
        a = b;
    }
    let pixel = 2.0 * distance(world, mesh_view_bindings::view.world_position.xyz) / (mesh_view_bindings::view.clip_from_view[1][1] * mesh_view_bindings::view.viewport.w);
    let warp = (pool_noise(here * 0.9 + vec2(f32(row) * 13.0)) - 0.5) * 2.0 * (RUN_WARP_UU + RUN_SPLAT_WARP * head * radius);
    let soft = max(0.25, pixel);
    let wet = (1.0 - smoothstep(-soft, soft, best + warp)) * saturate(wetness * 3.0);
    if wet <= 0.0 {
        return pool;
    }
    let across = saturate(length(way) / max(radius, 1e-3));
    let rise = sqrt(max(1.0 - across * across, 0.0));
    let out = normalize(along_u * way.x + along_v * way.y + normal * 1e-4);
    let surface = normalize(normal * rise + out * across);
    var optics = POOL_OPTICS[min(kind, 8u)];
    // Drying, blood browns and dulls.
    let dry = select(0.0, saturate(1.0 - wetness), kind == POOL_BLOOD);
    optics.body = mix(optics.body, POOL_DRIED, dry);
    optics.roughness = mix(optics.roughness, POOL_DRIED_ROUGHNESS, dry);
    let through = exp(-optics.sigma * (2.0 * RUN_THICKNESS * rise * min(radius, 1.5)));
    return Pool(mix(vec3(1.0), through, wet), optics.body * (1.0 - through) * wet, wet, normalize(mix(normal, surface, wet)), optics.roughness, vec3(1.0));
}

// Only a floor takes a pool, and only a wall a run. The atlas is read as a cubic B-spline, smooth in its value and its slope, so no texel and
// no tile shows; the edge is the depth's contour, as soft as a pixel and moved by world noise.
fn pool_at(pbr_input: PbrInput, ranges: ClusterableObjectIndexRanges, normal: vec3<f32>) -> Pool {
    var pool = no_pool(normal);
@if(CLUSTERED_DECALS_ARE_USABLE) {
    let on_floor = normal.y > 0.5;
    let on_wall = abs(normal.y) < RUN_STEEP;
    if !on_floor && !on_wall {
        return pool;
    }
    let world = pbr_input.world_position.xyz;
    var decal_ranges = ranges;
    var iterator = clustered_decal_iterator_new(world, &decal_ranges);
    while clustered_decal_iterator_next(&iterator) {
        if (iterator.tag & (PROJECTOR | 0x7u)) != (PROJECTOR | FB_LIQUID) || iterator.base_color_texture_index < 0 {
            continue;
        }
        let index = iterator.base_color_texture_index;
        if (iterator.tag & RUN_FLAG) != 0u {
            if on_wall {
                let run = run_at(index, iterator.tag, iterator.uv, iterator.decal_index_offset, world, normal);
                if run.wet > pool.wet {
                    pool = run;
                }
            }
            continue;
        }
        if !on_floor {
            continue;
        }
        let slot = (iterator.tag >> 8u) & 0xffu;
        let corner = vec2<i32>(i32(slot % POOL_SLOTS_SIDE), i32(slot / POOL_SLOTS_SIDE)) * POOL_SLOT_SIDE + POOL_APRON;
        let p = clamp(iterator.uv, vec2(0.0), vec2(1.0)) * POOL_SIDE - 0.5;
        let nearest = textureLoad(mesh_view_bindings::clustered_decal_textures[index], corner + vec2<i32>(round(p)), 0);
        if nearest.w < 0.0 {
            continue;
        }
        let base = floor(p);
        let wx = spline_weights(p.x - base.x);
        let wy = spline_weights(p.y - base.y);
        let sx = spline_slopes(p.x - base.x);
        let sy = spline_slopes(p.y - base.y);
        var weight = 0.0;
        var weight_slope = vec2(0.0);
        var depth = 0.0;
        var height = 0.0;
        var speed = 0.0;
        // The kind and blood share of the wettest texel around: at a rim the nearest may be dry.
        var wettest = vec2(0.0);
        // In texels: the depth's slope and the liquid surface's, which runs downhill.
        var depth_slope = vec2(0.0);
        var height_slope = vec2(0.0);
        for (var j = 0; j < 4; j++) {
            for (var i = 0; i < 4; i++) {
                let t = textureLoad(mesh_view_bindings::clustered_decal_textures[index], corner + vec2<i32>(base) + vec2(i - 1, j - 1), 0);
                if abs(t.z - world.y) > POOL_REACH_UU {
                    continue;
                }
                let w = wx[i] * wy[j];
                let s = vec2(sx[i] * wy[j], wx[i] * sy[j]);
                weight += w;
                weight_slope += s;
                depth += t.x * w;
                height += (t.z + t.x) * w;
                if t.x > wettest.x {
                    wettest = t.xy;
                }
                speed += max(t.w, 0.0) * w;
                depth_slope += t.x * s;
                height_slope += (t.z + t.x) * s;
            }
        }
        if weight < 0.05 {
            continue;
        }
        // Renormalised over the taps on this floor: the value and its slope as the quotient's.
        depth /= weight;
        height /= weight;
        speed /= weight;
        depth_slope = (depth_slope - depth * weight_slope) / weight;
        height_slope = (height_slope - height * weight_slope) / weight;
        let m = mesh_view_bindings::clustered_decals.decals[clustered_forward::get_clusterable_object_id(u32(iterator.decal_index_offset))].local_from_world;
        // Texture u runs along the decal's local x and v against its local y: one texel is `POOL_TEXEL_UU` units.
        let along_u = normalize(vec3(m[0].x, m[1].x, m[2].x));
        let along_v = -normalize(vec3(m[0].y, m[1].y, m[2].y));
        let depth_world = (along_u * depth_slope.x + along_v * depth_slope.y) / POOL_TEXEL_UU;
        let pixel = 2.0 * distance(world, mesh_view_bindings::view.world_position.xyz) / (mesh_view_bindings::view.clip_from_view[1][1] * mesh_view_bindings::view.viewport.w);
        let kind = u32(wettest.y);
        let paste = kind == POOL_PUKE || kind == POOL_BLOODY_PUKE;
        let oily = kind == POOL_GAS || kind == POOL_NAPALM;
        var warp = (pool_noise(world.xz / 30.0) * 0.65 + pool_noise(world.xz / 12.0) * 0.35 - 0.5) * 2.0 * POOL_WARP_UU;
        if paste {
            warp += (pool_noise(world.xz / 4.0) - 0.5) * 2.0 * POOL_WARP_UU;
        }
        let inside = clamp((depth - POOL_FILM * POOL_EDGE) / max(length(depth_world), 1e-4), -16.0, 16.0) + warp;
        let soft = max(POOL_SOFT_UU, pixel);
        let wet = smoothstep(-soft, soft, inside);
        if wet <= pool.wet {
            continue;
        }
        let blood = saturate(fract(wettest.y) / 0.98);
        var optics = POOL_OPTICS[min(kind, 8u)];
        let bleeding = POOL_OPTICS[POOL_BLOOD];
        optics = PoolOptics(mix(optics.sigma, bleeding.sigma, blood), mix(optics.body, bleeding.body, blood), mix(optics.roughness, bleeding.roughness, blood));
        // A thinning film is drying: blood browns and dulls.
        let dry = saturate(1.0 - depth / POOL_FILM) * blood;
        optics.body = mix(optics.body, POOL_DRIED, dry);
        optics.roughness = mix(optics.roughness, POOL_DRIED_ROUGHNESS, dry);
        // The liquid's surface: the floor's plane tilted by the depth's slope, rippling where it runs downhill.
        var tilt = depth_world;
        var thickness = max(depth, 0.0);
        if paste {
            // Chunks in a wet paste: lumps stand up, mottle its colour, and are rough where the paste between shines.
            let lumps = pool_noise(world.xz / POOL_LUMP_UU.x) * 0.6 + pool_noise(world.xz / POOL_LUMP_UU.y + 31.0) * 0.4;
            let chunk = smoothstep(0.5, 0.72, lumps);
            let carrot = smoothstep(0.78, 0.86, pool_noise(world.xz / 3.0 + 57.0));
            let mottle = mix(POOL_PUKE_YELLOW, POOL_PUKE_BROWN, pool_noise(world.xz / 14.0 + 7.0));
            let own = mix(mix(mottle, POOL_PUKE_BITS, chunk), POOL_PUKE_CARROT, carrot * (1.0 - chunk));
            optics.body = mix(own, optics.body, select(0.0, blood, kind == POOL_BLOODY_PUKE));
            optics.roughness = mix(0.12, 0.6, chunk);
            let bumps = pool_noise_slope(world.xz / POOL_LUMP_UU.x) / POOL_LUMP_UU.x * 0.6 + pool_noise_slope(world.xz / POOL_LUMP_UU.y + 31.0) / POOL_LUMP_UU.y * 0.4;
            tilt += vec3(bumps.x, 0.0, bumps.y) * (POOL_LUMP_TILT * wet);
            thickness += chunk * POOL_CHUNK_UU;
        }
        // Light crosses the liquid down to the floor and back up.
        let through = exp(-optics.sigma * (2.0 * thickness));
        let flow = -(along_u * height_slope.x + along_v * height_slope.y);
        let running = saturate(speed / POOL_RUNNING) * select(0.0, 1.0, dot(flow, flow) > 1e-8);
        if running > 0.0 {
            let way = normalize(flow);
            let phase = mesh_view_bindings::globals.time / POOL_FLOW_PERIOD;
            for (var k = 0; k < 2; k++) {
                let along = fract(phase + 0.5 * f32(k));
                let carried = world - way * (speed * along * POOL_FLOW_PERIOD);
                let ripple = pool_noise_slope(carried.xz / POOL_RIPPLE_UU + f32(k) * 17.0) / POOL_RIPPLE_UU;
                tilt += vec3(ripple.x, 0.0, ripple.y) * (POOL_RIPPLE * running * (1.0 - abs(2.0 * along - 1.0)));
            }
        }
        let surface = normalize(normal - (tilt - normal * dot(tilt, normal)));
        var sheen = vec3(1.0);
        var scattered = optics.body * (1.0 - through);
        if oily {
            // An oil film thicker toward the pool's middle, its thickness marbled by world noise that drifts slowly.
            let drift = mesh_view_bindings::globals.time * 0.02;
            let marble = pool_noise(world.xz / 9.0 + pool_noise(world.xz / 23.0 + drift) * 3.0 - drift);
            let nm = POOL_OIL_NM + POOL_OIL_SPREAD_NM * (marble - 0.5) * 2.0 + 60.0 * saturate(depth / POOL_FILM);
            let seen = abs(dot(normalize(mesh_view_bindings::view.world_position.xyz - world), surface));
            let film = thin_film(seen, nm);
            sheen = mix(vec3(1.0), 0.35 + 1.3 * film, POOL_OIL * wet);
            scattered += (film - 0.5) * 0.03;
        }
        pool = Pool(mix(vec3(1.0), through, wet), max(scattered, vec3(0.0)) * wet, wet, surface, optics.roughness, sheen);
    }
}
    return pool;
}

// The texture under a pool: the floor darkens as it soaks and shows through the liquid, which adds what it scatters.
fn wet_color(color: vec3<f32>, pool: Pool) -> vec3<f32> {
    let soaked = color * mix(vec3(1.0), color, SOAKED * pool.wet);
    return soaked * pool.through + pool.color;
}

// A liquid's surface is its own: the floor's relief does not show through it.
fn wet_normal(bumped: vec3<f32>, pool: Pool) -> vec3<f32> {
    return normalize(mix(bumped, pool.normal, pool.wet));
}

// Over the lit linear `color`, in cluster order; `texture` is read by material blending. BSP takes a
// projector only where it faces it; a `bUnlit` static mesh still takes projectors (unverified gap).
fn project(pbr_input: PbrInput, ranges: ClusterableObjectIndexRanges, texture: vec3<f32>, color: vec4<f32>, surface: u32) -> vec4<f32> {
    var out = color;
@if(CLUSTERED_DECALS_ARE_USABLE) {
    var decal_ranges = ranges;
    var iterator = clustered_decal_iterator_new(pbr_input.world_position.xyz, &decal_ranges);
    var base = vec3(0.0);
    var frame = vec3(0.0);
    var drawn = false;
    while clustered_decal_iterator_next(&iterator) {
        let wound = (iterator.tag & WOUND) != 0u && surface == 0u;
        if (iterator.tag & (PROJECTOR | 0x7u)) == (PROJECTOR | FB_LIQUID) {
            continue;
        }
        if (iterator.tag & PROJECTOR) == 0u || iterator.base_color_texture_index < 0 || ((iterator.tag & surface & TARGETS) == 0u && !wound) {
            continue;
        }
        // `ProjectTag` filters actors only: BSP and terrain take the projector regardless.
        if (iterator.tag & TAGGED) != 0u && (surface & TARGET_MESH) != 0u && (surface & TAGGED) == 0u {
            continue;
        }
        if (iterator.tag & ON_MOVER) != 0u && (surface & ON_MOVER) == 0u {
            continue;
        }
        // Only BSP's unlit flag means retail's (a surface without a lightmap, mesh_view); terrain and meshes are
        // flagged unlit because their shaders light them themselves, which hid every projector on them.
        if surface == TARGET_BSP && (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) != 0u && (iterator.tag & ON_UNLIT) == 0u {
            continue;
        }
        if (surface == TARGET_BSP || (iterator.tag & WOUND) != 0u) && !faces_projector(iterator.decal_index_offset, pbr_input.world_normal) {
            continue;
        }
        let texel = decal_texel(iterator.base_color_texture_index, iterator.uv);
        if (iterator.tag & ALPHA_TEST) != 0u && texel.a * 255.0 <= f32((iterator.tag >> 22u) & 0xffu) {
            continue;
        }
        if !drawn {
            base = to_display(texture);
            frame = to_display(color.rgb);
            drawn = true;
        }
        let projected = to_display(texel.rgb);
        let material = (iterator.tag >> 8u) & 0x3u;
        var src = projected;
        if material == PB_MODULATE {
            src = projected * base;
        } else if material == PB_ALPHA_BLEND {
            src = mix(projected, base, texel.a);
        } else if material == PB_ADD {
            src = min(projected + base, vec3(1.0));
        }
        let under = frame;
        switch iterator.tag & 0x7u {
            case FB_OVERWRITE: { frame = src; }
            case FB_MODULATE: { frame = 2.0 * src * frame; }
            case FB_ALPHA_BLEND: { frame = mix(frame, src, texel.a); }
            case FB_ALPHA_MODULATE: { frame = src + frame * (1.0 - texel.a); }
            case FB_TRANSLUCENT: { frame = src + frame; }
            case FB_DARKEN: { frame = frame * (1.0 - src); }
            case FB_BRIGHTEN: { frame = src + frame * (1.0 - src); }
            default: {}
        }
        // Drying darkens what the splat changed, whatever its blend; fading takes the frame back to what lay under it.
        let dry = (iterator.tag >> DRY_SHIFT) & DRY_MASK;
        if dry != 0u {
            frame = mix(frame, frame * DRY_TINT, f32(dry) / 15.0 * saturate(distance(frame, under) * 2.0));
        }
        let fade = (iterator.tag >> FADE_SHIFT) & FADE_MASK;
        if fade != 0u {
            frame = mix(frame, under, f32(fade) / 31.0);
        }
        frame = min(frame, vec3(1.0));
    }
    if drawn {
        out = vec4(to_linear(frame), color.a);
    }
}
    return out;
}
