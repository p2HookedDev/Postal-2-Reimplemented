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

// Liquid standing on a surface (`liquid_layer.wesl`): what of the surface's colour shows through it and what it scatters
// back, how wet the surface is under it, the liquid's surface normal and its roughness.
struct Pool {
    through: vec3<f32>,
    color: vec3<f32>,
    wet: f32,
    normal: vec3<f32>,
    roughness: f32,
    // What colours the highlight and the reflection: a thin oil film's interference.
    sheen: vec3<f32>,
    // Light it gives off itself, linear and past 1: burning fuel.
    glow: vec3<f32>,
    // How soaked the surface is: 1 under the liquid, fading over a ring past its edge that only darkens.
    soak: f32,
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
// The edge lies where the depth is this share of the film and fades over this many Unreal units at the least; one
// broad world noise of this size moves it this far in and out, so no pool is a circle.
const POOL_EDGE: f32 = 0.25;
const POOL_SOFT_UU: f32 = 0.6;
const POOL_WARP_SIZE_UU: f32 = 16.0;
const POOL_WARP_UU: f32 = 1.5;
// The surface stays soaked, darker and without gloss, this many units past where the liquid fades out.
const POOL_SOAK_UU: f32 = 1.5;
// A texel whose floor lies this far off the fragment's height is another surface: a wall, a kerb's other side, another
// floor. Only the texels on the fragment's own floor are read, so liquid runs up to walls and kerbs and stops there.
const POOL_REACH_UU: f32 = 8.0;
// A plane tile's decal (walls, ceilings, slopes) carries this tag bit. Its texels hold the face's offset along the
// plane's normal, the decal's projecting axis; a face a little off the tile's snapped direction leans up to this far
// from that offset across the tile. Only fragments facing the tile's normal take its pool.
const POOL_WALL_TILE: u32 = 0x800000u;
const POOL_WALL_REACH_UU: f32 = 6.0;
const POOL_FACE_MATCH: f32 = 0.9;
// Seconds of one phase of running liquid's ripples, their size in Unreal units, and how much they tilt the surface.
const POOL_FLOW_PERIOD: f32 = 1.2;
const POOL_RIPPLE_UU: f32 = 5.0;
const POOL_RIPPLE: f32 = 0.15;
// Unreal units a second at which running liquid ripples fully.
const POOL_RUNNING: f32 = 6.0;
// How much a soaking liquid darkens the floor, the most on a dark texel.
const SOAKED: f32 = 0.6;
const POOL_BLOOD: u32 = 3u;
const POOL_GAS: u32 = 1u;
const POOL_PUKE: u32 = 4u;
const POOL_BLOODY_PUKE: u32 = 5u;
const POOL_NAPALM: u32 = 8u;
// An oil film's thickness in nanometres, the spread world noise gives it, and how strongly it colours the highlight
// where its patches and grazing angles show it.
const POOL_OIL_NM: f32 = 380.0;
const POOL_OIL_SPREAD_NM: f32 = 260.0;
const POOL_OIL: f32 = 0.3;
// Gasoline darkens the floor it soaks by this much more.
const POOL_GAS_DARK: f32 = 0.8;
// The fire on the layer (`liquid_layer.wesl`): a speed channel at or past FIRE_BASE holds heat * 32 + scorch past it,
// each in steps up to LEVELS. Burning fuel glows from ember to flame this bright; scorch darkens the floor by this
// much and adds a little soot.
const POOL_FIRE_BASE: f32 = 1024.0;
const POOL_FIRE_LEVELS: f32 = 31.0;
const POOL_EMBER: vec3<f32> = vec3(1.0, 0.22, 0.03);
const POOL_FLAME: vec3<f32> = vec3(1.0, 0.55, 0.14);
const POOL_FIRE_GLOW: f32 = 2.0;
const POOL_SCORCH_DARK: f32 = 0.75;
const POOL_SOOT: vec3<f32> = vec3(0.008, 0.007, 0.006);
// A paste's lumps: their size in Unreal units and how much they tilt the surface. Its chunks lie in cells of this
// side, smaller bits in cells of that; this share of cells hold one, which stands this high and this much thicker.
const POOL_LUMP_UU: f32 = 5.0;
const POOL_LUMP_TILT: f32 = 0.5;
const POOL_CHUNK_UU: f32 = 3.2;
const POOL_BIT_UU: f32 = 1.3;
const POOL_CHUNKS: f32 = 0.55;
const POOL_CHUNK_RISE_UU: f32 = 0.5;
const POOL_CHUNK_DEPTH: f32 = 1.0;
// Puke's colours: bile, beige and orange-brown it mottles between, and its chunks' carrot, corn, bread and greens.
const POOL_PUKE_BILE: vec3<f32> = vec3(0.17, 0.19, 0.03);
const POOL_PUKE_BEIGE: vec3<f32> = vec3(0.36, 0.28, 0.15);
const POOL_PUKE_ORANGE: vec3<f32> = vec3(0.30, 0.13, 0.03);
const POOL_PUKE_CARROT: vec3<f32> = vec3(0.55, 0.18, 0.03);
const POOL_PUKE_CORN: vec3<f32> = vec3(0.52, 0.40, 0.06);
const POOL_PUKE_BREAD: vec3<f32> = vec3(0.50, 0.42, 0.28);
const POOL_PUKE_GREEN: vec3<f32> = vec3(0.10, 0.14, 0.03);

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
    // Gas: clear, faintly amber, glossy.
    PoolOptics(vec3(0.03, 0.08, 0.25), vec3(0.0), 0.02),
    // Urine: the floor through clear yellow.
    PoolOptics(vec3(0.10, 0.33, 1.4), vec3(0.02, 0.016, 0.0), 0.04),
    // Blood: near-black red where thick, crimson only where thin, a mirror when fresh.
    PoolOptics(vec3(7.0, 45.0, 50.0), vec3(0.035, 0.0015, 0.001), 0.03),
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
    return Pool(vec3(1.0), vec3(0.0), 0.0, normal, 1.0, vec3(1.0), vec3(0.0), 0.0);
}

// What a burning texel of `heat` in [0, 1] gives off at `ground`, a place in the surface's own plane, flickering by
// world noise.
fn pool_flame(ground: vec2<f32>, heat: f32) -> vec3<f32> {
    let t = mesh_view_bindings::globals.time;
    let n = pool_noise(ground / 7.0 + vec2(t * 0.7, -t * 1.9)) * 0.6 + pool_noise(ground / 2.5 - vec2(t * 2.3, t * 0.9)) * 0.4;
    let hot = heat * (0.45 + 1.1 * n);
    return mix(POOL_EMBER, POOL_FLAME, saturate(hot - 0.4)) * (POOL_FIRE_GLOW * hot);
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

// Four hashes in [0, 1) of the integer cell `c`, from integers: steady at any distance from the world's origin.
fn pool_cell(c: vec2<f32>) -> vec4<f32> {
    var v = vec4<u32>(vec2<u32>(vec2<i32>(c)), 0x9e3779b9u, 0x85ebca6bu);
    v = v * 1664525u + 1013904223u;
    v.x += v.y * v.w;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v.w += v.y * v.z;
    v ^= v >> vec4(16u);
    v.x += v.y * v.w;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v.w += v.y * v.z;
    return vec4<f32>(v >> vec4(8u)) / 16777216.0;
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

// A paste's chunk at the fragment: how much of one stands there, its tilt and its cell's hashes.
struct PoolChunk {
    amount: f32,
    tilt: vec2<f32>,
    seed: vec4<f32>,
}

// A liquid's look at the fragment: the colour it scatters, its roughness, its surface's tilt in the plane's own two
// axes, and what light keeps crossing it to the surface and back.
struct PoolLook {
    body: vec3<f32>,
    roughness: f32,
    tilt: vec2<f32>,
    through: vec3<f32>,
}

// A paste's chunks at `ground`, a place in the surface's own plane, each a lumpy dome in its own cell; near the rim they
// stand proud of the outline.
fn pool_chunks(ground: vec2<f32>, soft: f32) -> PoolChunk {
    var chunk = PoolChunk(0.0, vec2(0.0), vec4(0.0));
    let lumpy = ground + (vec2(pool_noise(ground / 1.5), pool_noise(ground / 1.5 + 19.0)) - 0.5) * 1.2;
    for (var g = 0; g < 2; g++) {
        let side = select(POOL_BIT_UU, POOL_CHUNK_UU, g == 0);
        let p = lumpy / side + f32(g) * 0.5;
        let cell = floor(p);
        let h = pool_cell(cell + f32(g) * 157.0);
        let r = mix(0.28, 0.48, h.z);
        let off = (p - (cell + r + h.xy * (1.0 - 2.0 * r))) / r;
        let d = length(off);
        let c = (1.0 - smoothstep(1.0 - soft / (r * side), 1.0, d)) * select(0.0, 1.0, h.w < POOL_CHUNKS);
        if c > chunk.amount {
            chunk = PoolChunk(c, -off / max(sqrt(max(1.0 - d * d, 0.0)), 0.3) * (POOL_CHUNK_RISE_UU / (r * side)), h);
        }
    }
    return chunk;
}

// The look of liquid of `kind`, `blood` of it blood and `thickness` units deep at `ground`, a place in the plane's own
// axes. `splat` is the game's puke texture there (`puke_texture`), which colours puke as far as it covers.
fn pool_look(kind: u32, blood: f32, wet: f32, thickness: f32, ground: vec2<f32>, chunk: PoolChunk, splat: vec4<f32>) -> PoolLook {
    var optics = POOL_OPTICS[min(kind, 8u)];
    let bleeding = POOL_OPTICS[POOL_BLOOD];
    optics = PoolOptics(mix(optics.sigma, bleeding.sigma, blood), mix(optics.body, bleeding.body, blood), mix(optics.roughness, bleeding.roughness, blood));
    var tilt = vec2(0.0);
    var deep = thickness;
    if kind == POOL_PUKE || kind == POOL_BLOODY_PUKE {
        // Vomit: bile, beige and orange-brown in big patches; chunks with their own colour, rise and roughness; a
        // thin glossy film of it in the low ground between them.
        let mottle = mix(mix(POOL_PUKE_BILE, POOL_PUKE_BEIGE, smoothstep(0.3, 0.7, pool_noise(ground / 19.0))), POOL_PUKE_ORANGE, smoothstep(0.55, 0.8, pool_noise(ground / 7.0 + 23.0)));
        let pick = fract(chunk.seed.w * 7.31 + chunk.seed.z * 3.7);
        let bit = select(select(select(POOL_PUKE_GREEN, POOL_PUKE_BREAD, pick < 0.8), POOL_PUKE_CORN, pick < 0.55), POOL_PUKE_CARROT, pick < 0.3);
        let drawn = mix(mottle * 0.7, bit * mix(0.8, 1.1, chunk.seed.x), chunk.amount);
        let own = mix(drawn, splat.rgb * mix(1.0, mix(0.85, 1.1, chunk.seed.x), chunk.amount), splat.a);
        optics.body = mix(own, optics.body, select(0.0, blood, kind == POOL_BLOODY_PUKE));
        optics.roughness = mix(0.06, mix(0.4, 0.8, chunk.seed.y), chunk.amount);
        let lumps = pool_noise_slope(ground / POOL_LUMP_UU) / POOL_LUMP_UU;
        tilt += lumps * (POOL_LUMP_TILT * wet) + chunk.tilt * chunk.amount;
        deep = mix(deep * 0.6, deep + POOL_CHUNK_DEPTH, chunk.amount);
    }
    // Light crosses the liquid down to the surface and back up.
    var through = exp(-optics.sigma * (2.0 * deep));
    if kind == POOL_GAS {
        through *= POOL_GAS_DARK;
    }
    return PoolLook(optics.body, optics.roughness, tilt, through);
}

// An oil film over `sheen`: its colours show only where a drifting patch meets a grazing angle (`seen` the cosine off
// the surface), thicker toward the middle (`middle` from 0 at the rim to 1) and marbled by world noise.
fn pool_oil(sheen: vec3<f32>, ground: vec2<f32>, middle: f32, seen: f32, show: f32) -> vec3<f32> {
    let drift = mesh_view_bindings::globals.time * 0.02;
    let marble = pool_noise(ground / 9.0 + pool_noise(ground / 23.0 + drift) * 3.0 - drift);
    let nm = POOL_OIL_NM + POOL_OIL_SPREAD_NM * (marble - 0.5) * 2.0 + 60.0 * saturate(middle);
    let film = thin_film(seen, nm);
    let slick = smoothstep(0.55, 0.8, pool_noise(ground / 16.0 + 5.0 + drift));
    let shown = POOL_OIL * show * slick * smoothstep(0.45, 0.9, 1.0 - seen);
    return mix(sheen, sheen * (0.35 + 1.3 * film), shown);
}

// A liquid decal with this bit carries the game's puke splat (`liquids/mod.rs` `PUKE_SPLAT_TAG`), as a child of a
// puke tile's decal in the same box. All of them hold the one texture.
const PUKE_SPLAT: u32 = 0x400000u;
// No splat decal looked for yet.
const PUKE_UNSOUGHT: i32 = -2;

// The puke splat decal's texture index among the decals at `world`, or -1.
@if(CLUSTERED_DECALS_ARE_USABLE)
fn puke_splat_index(world: vec3<f32>, ranges: ClusterableObjectIndexRanges) -> i32 {
    var decal_ranges = ranges;
    var iterator = clustered_decal_iterator_new(world, &decal_ranges);
    while clustered_decal_iterator_next(&iterator) {
        if (iterator.tag & (PROJECTOR | 0x7u | PUKE_SPLAT)) == (PROJECTOR | FB_LIQUID | PUKE_SPLAT) {
            return iterator.metallic_roughness_texture_index;
        }
    }
    return -1;
}

// The game's puke splat sprite at `ground`, a place in a surface's own plane: three copies tile the plane, each at its
// own size in Unreal units (the scripts' splats run 8 to 40), turn and shift, so one copy's gaps fill from another's.
// Its linear colour, weighted by the copies' alpha, and how much of the place they cover; none for `index` below 0.
fn puke_texture(index: i32, ground: vec2<f32>, pixel: f32) -> vec4<f32> {
    if index < 0 {
        return vec4(0.0);
    }
    let side = f32(textureDimensions(mesh_view_bindings::clustered_decal_textures[index]).x);
    var sum = vec4(0.0);
    for (var k = 0; k < 3; k++) {
        let f = f32(k);
        let size = 15.0 + 11.0 * f;
        let turn = 2.1 * f;
        let p = (mat2x2<f32>(cos(turn), sin(turn), -sin(turn), cos(turn)) * ground) / size + vec2(0.37, 0.71) * f;
        let lod = log2(max(pixel * side / size, 1.0));
        let t = textureSampleLevel(mesh_view_bindings::clustered_decal_textures[index], mesh_view_bindings::clustered_decal_sampler, fract(p), lod);
        sum += vec4(t.rgb * t.a, t.a);
    }
    return vec4(sum.rgb / max(sum.a, 1e-4), saturate(sum.a * 2.0));
}

// A floor tile's pool lies on floors and a plane tile's on every face turned to its normal: walls, slopes, ceilings.
// The atlas is read as a cubic B-spline, smooth in its value and its slope, so no texel and no tile shows; the edge is
// the depth's contour, soft and moved by broad world noise, with a soaked ring past it.
fn pool_at(pbr_input: PbrInput, ranges: ClusterableObjectIndexRanges, normal: vec3<f32>) -> Pool {
    var pool = no_pool(normal);
@if(CLUSTERED_DECALS_ARE_USABLE) {
    let on_floor = normal.y > 0.5;
    let world = pbr_input.world_position.xyz;
    // Sought only once puke is found here, as few places hold any.
    var puke = PUKE_UNSOUGHT;
    var decal_ranges = ranges;
    var iterator = clustered_decal_iterator_new(world, &decal_ranges);
    while clustered_decal_iterator_next(&iterator) {
        // The liquid's texture rides as the decal's metallic-roughness map, which Bevy's own decal pass reads only under a
        // base colour, so a standard material lying in a pool does not take its data as colour.
        if (iterator.tag & (PROJECTOR | 0x7u)) != (PROJECTOR | FB_LIQUID) || iterator.metallic_roughness_texture_index < 0 {
            continue;
        }
        let wall_tile = (iterator.tag & POOL_WALL_TILE) != 0u;
        if (iterator.tag & PUKE_SPLAT) != 0u || (!wall_tile && !on_floor) {
            continue;
        }
        let index = iterator.metallic_roughness_texture_index;
        let slot = (iterator.tag >> 8u) & 0xffu;
        let corner = vec2<i32>(i32(slot % POOL_SLOTS_SIDE), i32(slot / POOL_SLOTS_SIDE)) * POOL_SLOT_SIDE + POOL_APRON;
        let p = clamp(iterator.uv, vec2(0.0), vec2(1.0)) * POOL_SIDE - 0.5;
        let nearest = textureLoad(mesh_view_bindings::clustered_decal_textures[index], corner + vec2<i32>(round(p)), 0);
        if nearest.w < 0.0 {
            continue;
        }
        let m = mesh_view_bindings::clustered_decals.decals[clustered_forward::get_clusterable_object_id(u32(iterator.decal_index_offset))].local_from_world;
        // Texture u runs along the decal's local x and v against its local y: one texel is `POOL_TEXEL_UU` units. It
        // projects along its local z, up on a floor and the plane's normal on a plane tile.
        let along_u = normalize(vec3(m[0].x, m[1].x, m[2].x));
        let along_v = -normalize(vec3(m[0].y, m[1].y, m[2].y));
        let along_n = normalize(vec3(m[0].z, m[1].z, m[2].z));
        if wall_tile && dot(normal, along_n) < POOL_FACE_MATCH {
            continue;
        }
        // The fragment's place along the projecting axis, which the texels store for their floor or face.
        let standing = select(world.y, dot(world, along_n), wall_tile);
        let reach = select(POOL_REACH_UU, POOL_WALL_REACH_UU, wall_tile);
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
        // Heat and scorch, in steps.
        var fire = vec2(0.0);
        // The kind and blood share of the wettest texel around: at a rim the nearest may be dry.
        var wettest = vec2(0.0);
        // In texels: the depth's slope and the liquid surface's, which runs downhill.
        var depth_slope = vec2(0.0);
        var height_slope = vec2(0.0);
        for (var j = 0; j < 4; j++) {
            for (var i = 0; i < 4; i++) {
                let t = textureLoad(mesh_view_bindings::clustered_decal_textures[index], corner + vec2<i32>(base) + vec2(i - 1, j - 1), 0);
                if abs(t.z - standing) > reach {
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
                if t.w >= POOL_FIRE_BASE {
                    let code = t.w - POOL_FIRE_BASE;
                    let heat = floor(code / 32.0);
                    fire += vec2(heat, code - heat * 32.0) * w;
                } else {
                    speed += max(t.w, 0.0) * w;
                }
                depth_slope += t.x * s;
                height_slope += (t.z + t.x) * s;
            }
        }
        if weight < 0.05 {
            continue;
        }
        // Renormalised over the taps on this surface: the value and its slope as the quotient's.
        depth /= weight;
        height /= weight;
        speed /= weight;
        fire /= weight * POOL_FIRE_LEVELS;
        depth_slope = (depth_slope - depth * weight_slope) / weight;
        height_slope = (height_slope - height * weight_slope) / weight;
        let depth_world = (along_u * depth_slope.x + along_v * depth_slope.y) / POOL_TEXEL_UU;
        let pixel = 2.0 * distance(world, mesh_view_bindings::view.world_position.xyz) / (mesh_view_bindings::view.clip_from_view[1][1] * mesh_view_bindings::view.viewport.w);
        let kind = u32(wettest.y);
        let paste = kind == POOL_PUKE || kind == POOL_BLOODY_PUKE;
        let oily = kind == POOL_GAS || kind == POOL_NAPALM;
        let blood = saturate(fract(wettest.y) / 0.98);
        // World noise reads the fragment's place in the tile's own plane, so floors, walls and ceilings share one look.
        let ground = vec2(dot(world, along_u), dot(world, along_v));
        let flow = -(along_u * height_slope.x + along_v * height_slope.y);
        let rising = length(depth_world);
        let warp = (pool_noise(ground / POOL_WARP_SIZE_UU) - 0.5) * 2.0 * POOL_WARP_UU;
        let inside = clamp((depth - POOL_FILM * POOL_EDGE) / max(rising, 1e-4), -16.0, 16.0) + warp;
        let soft = max(POOL_SOFT_UU, 0.7 * pixel);
        var wet = smoothstep(-soft, soft, inside);
        var chunk = PoolChunk(0.0, vec2(0.0), vec4(0.0));
        if paste {
            chunk = pool_chunks(ground, soft);
            wet = max(wet, chunk.amount * saturate(inside + 2.0));
        }
        let soak = max(wet, smoothstep(-soft - POOL_SOAK_UU, -soft, inside));
        // Scorch mottled by world noise, and on a dry surface, the soot mark and what still glows there.
        var soot = 0.0;
        if fire.y > 0.0 {
            soot = fire.y * (0.7 + 0.3 * pool_noise(ground / 3.0));
        }
        if wet <= pool.wet {
            pool.soak = max(pool.soak, soak);
            if pool.wet <= 0.0 && fire.x + fire.y > 0.004 {
                pool = Pool(vec3(1.0 - POOL_SCORCH_DARK * soot), POOL_SOOT * soot, 0.0, normal, 1.0, vec3(1.0), pool_flame(ground, fire.x), pool.soak);
            }
            continue;
        }
        var splat = vec4(0.0);
        if paste {
            if puke == PUKE_UNSOUGHT {
                puke = puke_splat_index(world, ranges);
            }
            splat = puke_texture(puke, ground, pixel);
        }
        let look = pool_look(kind, blood, wet, max(depth, 0.0), ground, chunk, splat);
        // The liquid's surface: the plane tilted by the depth's slope, rippling where it runs downhill.
        var tilt = depth_world + along_u * look.tilt.x + along_v * look.tilt.y;
        var roughness = look.roughness;
        // A burning film shimmers and dulls a little.
        if fire.x > 0.01 {
            let t = mesh_view_bindings::globals.time;
            let shimmer = pool_noise_slope(ground / 3.0 + vec2(t * 1.3, -t * 2.1)) / 3.0;
            tilt += (along_u * shimmer.x + along_v * shimmer.y) * (0.15 * fire.x);
            roughness = mix(roughness, 0.12, fire.x);
        }
        let running = saturate(speed / POOL_RUNNING) * select(0.0, 1.0, dot(flow, flow) > 1e-8);
        if running > 0.0 {
            let way = normalize(flow);
            let phase = mesh_view_bindings::globals.time / POOL_FLOW_PERIOD;
            for (var k = 0; k < 2; k++) {
                let along = fract(phase + 0.5 * f32(k));
                let carried = world - way * (speed * along * POOL_FLOW_PERIOD);
                let ripple = pool_noise_slope(vec2(dot(carried, along_u), dot(carried, along_v)) / POOL_RIPPLE_UU + f32(k) * 17.0) / POOL_RIPPLE_UU;
                tilt += (along_u * ripple.x + along_v * ripple.y) * (POOL_RIPPLE * running * (1.0 - abs(2.0 * along - 1.0)));
            }
        }
        let surface = normalize(normal - (tilt - normal * dot(tilt, normal)));
        let scattered = look.body * (1.0 - look.through);
        var sheen = vec3(1.0);
        if oily {
            // Mostly a dark glossy wet surface with an oil sheen in patches. Fire burns it off.
            let seen = abs(dot(normalize(mesh_view_bindings::view.world_position.xyz - world), surface));
            sheen = pool_oil(sheen, ground, depth / POOL_FILM, seen, wet * (1.0 - fire.x));
        }
        var glow = vec3(0.0);
        if fire.x > 0.0 {
            glow = pool_flame(ground, fire.x);
        }
        let scorched = 1.0 - POOL_SCORCH_DARK * soot;
        pool = Pool(mix(vec3(1.0), look.through, wet) * scorched, max(scattered, vec3(0.0)) * wet + POOL_SOOT * soot, wet, surface, roughness, sheen, glow, max(soak, pool.soak));
    }
}
    return pool;
}

// The texture under a pool: the surface darkens as it soaks and shows through the liquid, which adds what it scatters.
fn wet_color(color: vec3<f32>, pool: Pool) -> vec3<f32> {
    let soaked = color * mix(vec3(1.0), color, SOAKED * pool.soak);
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
