// Projector decals (tagged by `projector_tag`) are retail's projector pass over the finished surface,
// unlit in display space: `project`, after the lighting. Every clustered decal of the game is a projector or a wound,
// both tagged as projectors, so no decal blends into the base colour before the lighting; only with our liquids on do
// the game's blood ones stand in the world's pools as blood (`pool_at`), lit with them.

import bevy_pbr::render::{
    pbr_types::{PbrInput, STANDARD_MATERIAL_FLAGS_UNLIT_BIT},
    clustered_forward,
    clustered_forward::ClusterableObjectIndexRanges,
    forward_io::VertexOutput,
    mesh_functions::get_tag,
    mesh_view_bindings,
};
import bevy_pbr::decal::clustered::get_view_z;
import package::p2_render::color::{BLOOD_BODY, BLOOD_SIGMA, to_display, to_linear};
import package::p2_render::lamp_shadow::{BOOK_WIDTH, Book, DECAL_BINS_FROM_END, book_of, book_texel};

// `pool_at`'s tap sums in half floats on a card that has them (`shaders.rs` sets the flag).
@if(HALF_FLOATS) enable f16;
@if(HALF_FLOATS) alias half = f16;
@else alias half = f32;

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
const LUMA: vec3<f32> = vec3(0.2126, 0.7152, 0.0722);

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

// Where a pixel finds the decals over it: its screen tile's list in the lamp book (`decal_bins.wesl`), entries `start`
// to `end` from texel `binned` with our liquid's decals before `start`, or with `binned` below 0 Bevy's cluster list
// between those offsets.
struct DecalList {
    book: Book,
    binned: i32,
    start: i32,
    end: i32,
}

// Matches `decal_bins.wesl`.
const BIN_HEAD_TEXELS: u32 = 4u;
const BIN_LIST_TEXELS: u32 = 16u;
const BIN_LISTED_SPAN: u32 = 64u;

fn cluster_decals(book: Book, ranges: ClusterableObjectIndexRanges) -> DecalList {
@if(SKY_VIEW) {
    return DecalList(book, -1, 0, 0);
}
@if(!SKY_VIEW) {
    return DecalList(book, -1, i32(ranges.first_decal_offset), i32(ranges.last_clusterable_object_index_offset));
}
}

// The pixel's tile list where the book holds one written this frame for this view, else Bevy's cluster list. A surface
// drawn after the opaque pass (`binned` false) is not in the depth the lists were cut by.
fn decal_list(in: VertexOutput, pbr_input: PbrInput, ranges: ClusterableObjectIndexRanges, binned: bool) -> DecalList {
    let book = book_of(in);
    let clusters = cluster_decals(book, ranges);
    if !binned || clusters.end == clusters.start || book.size.x != BOOK_WIDTH || book.size.y < DECAL_BINS_FROM_END {
        return clusters;
    }
    let head = (book.size.y - DECAL_BINS_FROM_END) * BOOK_WIDTH;
    let grid = book_texel(book, head);
    if grid.w != 1.0 || any(book_texel(book, head + 1u) != mesh_view_bindings::view.clip_from_world[0]) || any(book_texel(book, head + 2u) != mesh_view_bindings::view.clip_from_world[3]) {
        return clusters;
    }
    let tile = vec2<u32>(pbr_input.frag_coord.xy / grid.y);
    let start = head + BIN_HEAD_TEXELS + (tile.y * u32(grid.x) + tile.x) * BIN_LIST_TEXELS;
    let counts = book_texel(book, start).x;
    if counts < 0.0 {
        return clusters;
    }
    let packed = u32(counts);
    return DecalList(book, i32(start), i32(packed / BIN_LISTED_SPAN), i32(packed % BIN_LISTED_SPAN));
}

// All of `list`, our liquid's decals with the rest.
fn whole_list(list: DecalList) -> DecalList {
    if list.binned < 0 {
        return list;
    }
    return DecalList(list.book, list.binned, 0, list.end);
}

// Of `list`, the part that holds our liquid's decals.
fn liquid_list(list: DecalList) -> DecalList {
    if list.binned < 0 {
        return list;
    }
    return DecalList(list.book, list.binned, 0, list.start);
}

// The index in Bevy's decal buffer of the `at`th decal of `list`.
fn listed_decal(list: DecalList, at: i32) -> i32 {
    if list.binned < 0 {
        return i32(clustered_forward::get_clusterable_object_id(u32(at)));
    }
    let entry = u32(at) + 1u;
    return i32(book_texel(list.book, u32(list.binned) + entry / 4u)[entry % 4u]);
}

// A walk along a `DecalList`; `decal` is the index in Bevy's decal buffer of the one last found.
struct DecalWalk {
    at: i32,
    decal: i32,
    base_color_texture_index: i32,
    normal_map_texture_index: i32,
    metallic_roughness_texture_index: i32,
    uv: vec2<f32>,
    tag: u32,
    world_position: vec3<f32>,
}

fn decal_walk(list: DecalList, world: vec3<f32>) -> DecalWalk {
    return DecalWalk(list.start - 1, -1, -1, -1, -1, vec2(0.0), 0u, world);
}

// Whether the box of `decal` holds the walk's place, the walk taking its fields when it does.
@if(CLUSTERED_DECALS_ARE_USABLE)
fn decal_holds(walk: ptr<function, DecalWalk>, decal: i32) -> bool {
    let local = (mesh_view_bindings::clustered_decals.decals[decal].local_from_world * vec4((*walk).world_position, 1.0)).xyz;
    if any(local < vec3(-0.5)) || any(local > vec3(0.5)) {
        return false;
    }
    (*walk).decal = decal;
    (*walk).base_color_texture_index = i32(mesh_view_bindings::clustered_decals.decals[decal].base_color_texture_index);
    (*walk).normal_map_texture_index = i32(mesh_view_bindings::clustered_decals.decals[decal].normal_map_texture_index);
    (*walk).metallic_roughness_texture_index = i32(mesh_view_bindings::clustered_decals.decals[decal].metallic_roughness_texture_index);
    (*walk).uv = local.xy * vec2(1.0, -1.0) + vec2(0.5);
    (*walk).tag = mesh_view_bindings::clustered_decals.decals[decal].tag;
    return true;
}

// The next decal of `list` over the walk's place.
@if(CLUSTERED_DECALS_ARE_USABLE)
fn next_decal(walk: ptr<function, DecalWalk>, list: DecalList) -> bool {
    (*walk).at += 1;
    while (*walk).at < list.end {
        if decal_holds(walk, listed_decal(list, (*walk).at)) {
            return true;
        }
        (*walk).at += 1;
    }
    return false;
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
fn decal_alpha_soft(index: i32, uv: vec2<f32>) -> f32 {
    return textureSampleLevel(mesh_view_bindings::clustered_decal_textures[index], mesh_view_bindings::clustered_decal_sampler, uv, 1.0).a;
}

@if(CLUSTERED_DECALS_ARE_USABLE)
fn faces_projector(decal: i32, normal: vec3<f32>) -> bool {
    let m = mesh_view_bindings::clustered_decals.decals[decal].local_from_world;
    let direction = -normalize(vec3(m[0].z, m[1].z, m[2].z));
    return dot(normalize(normal), direction) < -0.0001;
}

// The texel a game projector or wound leaves on `surface` at the fragment, or a negative alpha where it draws none.
// BSP takes a projector only where it faces it; a `bUnlit` static mesh still takes projectors (unverified gap).
@if(CLUSTERED_DECALS_ARE_USABLE)
fn projector_texel(pbr_input: PbrInput, tag: u32, index: i32, decal: i32, uv: vec2<f32>, surface: u32) -> vec4<f32> {
    let refused = vec4(-1.0);
    let wound = (tag & WOUND) != 0u && surface == 0u;
    if (tag & (PROJECTOR | 0x7u)) == (PROJECTOR | FB_LIQUID) {
        return refused;
    }
    if (tag & PROJECTOR) == 0u || index < 0 || ((tag & surface & TARGETS) == 0u && !wound) {
        return refused;
    }
    // `ProjectTag` filters actors only: BSP and terrain take the projector regardless.
    if (tag & TAGGED) != 0u && (surface & TARGET_MESH) != 0u && (surface & TAGGED) == 0u {
        return refused;
    }
    if (tag & ON_MOVER) != 0u && (surface & ON_MOVER) == 0u {
        return refused;
    }
    // Only BSP's unlit flag means retail's (a surface without a lightmap, mesh_view); terrain and meshes are
    // flagged unlit because their shaders light them themselves, which hid every projector on them.
    if surface == TARGET_BSP && (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) != 0u && (tag & ON_UNLIT) == 0u {
        return refused;
    }
    if (surface == TARGET_BSP || (tag & WOUND) != 0u) && !faces_projector(decal, pbr_input.world_normal) {
        return refused;
    }
    let texel = decal_texel(index, uv);
    if (tag & ALPHA_TEST) != 0u && texel.a * 255.0 <= f32((tag >> 22u) & 0xffu) {
        return refused;
    }
    return texel;
}

// What of a decal is left as it fades out (tag bits 10..15).
fn unfaded(tag: u32) -> f32 {
    return 1.0 - f32((tag >> FADE_SHIFT) & FADE_MASK) / 31.0;
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
    // What colours the reflection of the surroundings alone: blood keeps its hue there, its glints stay white.
    tint: vec3<f32>,
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
// How much of the film's real slope its surface shows: a thin liquid's edge is a fine line of light, not a bevel.
const POOL_RISE: f32 = 0.35;
const POOL_WARP_SIZE_UU: f32 = 16.0;
const POOL_WARP_UU: f32 = 2.5;
// A floor texel clearly darker than the texture around it, grout or a crack, draws the edge up to this many units
// further out: from `POOL_GROUT_FROM` darker in display space, fully at `POOL_GROUT_FROM + POOL_GROUT_DARK`. On tile the
// liquid creeps along the joints first; a texture's own grain stays under the dead band. Terrain passes the even tone.
const POOL_GROUT_UU: f32 = 3.0;
const POOL_GROUT_FROM: f32 = 0.1;
const POOL_GROUT_DARK: f32 = 0.25;
const POOL_EVEN_TONE: f32 = 1.0;
// The surface stays soaked, darker and without gloss, this many units past where the liquid fades out.
const POOL_SOAK_UU: f32 = 0.6;
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
// Blood dried to a dull crust.
const BLOOD_DRIED_ROUGHNESS: f32 = 0.7;
// Fresh blood's reflection of its surroundings keeps its hue: an even white lift over its dark body reads as a paste.
const BLOOD_SHEEN: vec3<f32> = vec3(1.0, 0.3, 0.3);
// Fresh blood is uneven: broad patches lighter or darker, duller clots, a surface gently swelled, and a meniscus at
// its edge that catches the light.
const BLOOD_MOTTLE_UU: f32 = 11.0;
const BLOOD_CLOT_UU: f32 = 5.0;
const BLOOD_CLOT_ROUGHNESS: f32 = 0.12;
const BLOOD_SWELL_UU: f32 = 9.0;
const BLOOD_SWELL: f32 = 0.25;
const BLOOD_MENISCUS: f32 = 0.4;
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
    // Blood: crimson, the floor showing through only where it is thin, a mirror when fresh.
    PoolOptics(BLOOD_SIGMA, BLOOD_BODY, 0.03),
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
    return Pool(vec3(1.0), vec3(0.0), 0.0, normal, 1.0, vec3(1.0), vec3(1.0), vec3(0.0), 0.0);
}

// How sharply the pool mirrors the room, handed to the screen reflections in the frame's alpha past 1 (`reflections.wesl`);
// negative where it is mostly blood (its tint more than half way to BLOOD_SHEEN), whose reflection keeps blood's hue.
fn pool_mirror(pool: Pool) -> f32 {
    let mirror = pool.wet * (1.0 - smoothstep(0.2, 0.5, pool.roughness));
    return select(mirror, -mirror, pool.tint.g < 0.65);
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

// The ramp of a random direction at the integer corner `c`, `off` from it. The direction comes from an integer hash: a
// sine hash is not steady far from the world's origin, and the slopes taken of this noise magnify any step.
fn pool_ramp(c: vec2<f32>, off: vec2<f32>) -> f32 {
    let q = vec2<u32>(vec2<i32>(c)) * vec2(1597334673u, 3812015801u);
    let h = (q.x ^ q.y) * 1597334673u;
    let way = vec2(f32(h >> 20u), f32((h >> 8u) & 0xfffu)) - 2047.5;
    return dot(way, off) * inverseSqrt(dot(way, way));
}

// Gradient noise in [0, 1] around 0.5, with value noise's spread, feature size and slopes: value noise's contours run
// along its cell edges, and a threshold of it draws squares.
fn pool_noise(at: vec2<f32>) -> f32 {
    let p = at * 0.6;
    let i = floor(p);
    let f = p - i;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let low = mix(pool_ramp(i, f), pool_ramp(i + vec2(1.0, 0.0), f - vec2(1.0, 0.0)), u.x);
    let high = mix(pool_ramp(i + vec2(0.0, 1.0), f - vec2(0.0, 1.0)), pool_ramp(i + vec2(1.0, 1.0), f - vec2(1.0, 1.0)), u.x);
    return saturate(0.5 + mix(low, high, u.y));
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
// `base` is the liquid blood's share is mixed into: the kind itself, or beside blood the other liquid there.
fn pool_look(kind: u32, base: u32, blood: f32, wet: f32, thickness: f32, ground: vec2<f32>, chunk: PoolChunk, splat: vec4<f32>) -> PoolLook {
    var optics = POOL_OPTICS[min(base, 8u)];
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
fn puke_splat_index(world: vec3<f32>, list: DecalList) -> i32 {
    let liquids = liquid_list(list);
    var walk = decal_walk(liquids, world);
    while next_decal(&walk, liquids) {
        if (walk.tag & (PROJECTOR | 0x7u | PUKE_SPLAT)) == (PROJECTOR | FB_LIQUID | PUKE_SPLAT) {
            return walk.metallic_roughness_texture_index;
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

// The game's blood splat as blood standing on the surface, `wet` the share its texture covers there: as deep as it
// covers, so its edge bends the surface as a pool's rim does; drying dulls it.
@if(CLUSTERED_DECALS_ARE_USABLE)
fn splat_tilt(tag: u32, index: i32, decal: i32, uv: vec2<f32>) -> vec3<f32> {
    // Read two texels apart on the half-size mip: one texel apart, the 4-bit alpha's steps facet the highlight in squares.
    let pitch = 2.0 / vec2<f32>(textureDimensions(mesh_view_bindings::clustered_decal_textures[index]));
    let slope = vec2(
        decal_alpha_soft(index, uv + vec2(pitch.x, 0.0)) - decal_alpha_soft(index, uv - vec2(pitch.x, 0.0)),
        decal_alpha_soft(index, uv + vec2(0.0, pitch.y)) - decal_alpha_soft(index, uv - vec2(0.0, pitch.y))
    ) / (2.0 * pitch) * unfaded(tag);
    // The texture spans the decal: u along its local x, v against its local y.
    let m = mesh_view_bindings::clustered_decals.decals[decal].local_from_world;
    return (vec3(m[0].x, m[1].x, m[2].x) * slope.x - vec3(m[0].y, m[1].y, m[2].y) * slope.y) * (POOL_FILM * POOL_RISE);
}

// The game's blood splats over a fragment so far: their cover combined, and their slopes and drying weighted by cover.
struct SplatSum {
    wet: f32,
    weight: f32,
    dry: f32,
    tilt: vec3<f32>,
}

@if(CLUSTERED_DECALS_ARE_USABLE)
fn add_blood_splat(sum: ptr<function, SplatSum>, pbr_input: PbrInput, tag: u32, index: i32, decal: i32, uv: vec2<f32>, receiver: u32) {
    let wet = projector_texel(pbr_input, tag, index, decal, uv, receiver).a * unfaded(tag);
    if wet > 0.0 {
        let dry = (tag >> DRY_SHIFT) & DRY_MASK;
        // A dry splat is too dull for its rim's bend to show.
        if dry < DRY_MASK {
            (*sum).tilt += splat_tilt(tag, index, decal, uv);
        }
        (*sum).dry += f32(dry) / 15.0 * wet;
        (*sum).weight += wet;
        (*sum).wet = 1.0 - (1.0 - (*sum).wet) * (1.0 - wet);
    }
}

// The game's blood splats over a fragment as one film: their cover combined, their slopes added, so overlapping
// splats leave no crease in the highlight.
fn blood_splat(normal: vec3<f32>, tilt: vec3<f32>, alpha: f32, dryness: f32) -> Pool {
    // The game blends its splats in display space: a texel's alpha hides that share of the floor's display colour, which
    // is more of its linear light. The floor under it is not soaked darker.
    let wet = 1.0 - pow(1.0 - alpha, 2.2);
    let surface = normalize(normal - (tilt - normal * dot(tilt, normal)));
    let look = pool_look(POOL_BLOOD, POOL_BLOOD, 1.0, wet, wet * POOL_FILM, vec2(0.0), PoolChunk(0.0, vec2(0.0), vec4(0.0)), vec4(0.0));
    return Pool(mix(vec3(1.0), look.through, wet), look.body * (1.0 - look.through) * wet, wet, surface, mix(look.roughness, BLOOD_DRIED_ROUGHNESS, dryness), vec3(1.0), BLOOD_SHEEN, vec3(0.0), 0.0);
}

// The game's blood splats on `receiver` alone, never our liquids: a body's wounds, where a floor pool must not lie.
fn splat_pool(pbr_input: PbrInput, list: DecalList, normal: vec3<f32>, receiver: u32) -> Pool {
    var pool = no_pool(normal);
@if(CLUSTERED_DECALS_ARE_USABLE) {
    var sum = SplatSum(0.0, 0.0, 0.0, vec3(0.0));
    var walk = decal_walk(list, pbr_input.world_position.xyz);
    while next_pool_decal(&walk, list, false, false) {
        add_blood_splat(&sum, pbr_input, walk.tag, walk.base_color_texture_index, walk.decal, walk.uv, receiver);
    }
    if sum.wet > 0.0 {
        pool = blood_splat(normal, sum.tilt, sum.wet, sum.dry / sum.weight);
    }
}
    return pool;
}

// A floor tile's pool lies on floors and a plane tile's on every face turned to its normal: walls, slopes, ceilings.
// The atlas is read as a cubic B-spline, smooth in its value and its slope, so no texel and no tile shows; the edge is
// the depth's contour, soft and moved by broad world noise, with a soaked ring past it. The game's blood splats that
// draw on `receiver` (the projector targets `project` takes) stand on it too. `tone` is the floor texel's luma over the texture's around it.
fn pool_at(pbr_input: PbrInput, list: DecalList, normal: vec3<f32>, receiver: u32, tone: f32) -> Pool {
    var pool = no_pool(normal);
@if(CLUSTERED_DECALS_ARE_USABLE) {
    let on_floor = normal.y > 0.5;
    let world = pbr_input.world_position.xyz;
    // Sought only once puke is found here, as few places hold any.
    var puke = PUKE_UNSOUGHT;
    var splats = SplatSum(0.0, 0.0, 0.0, vec3(0.0));
    let whole = whole_list(list);
    var walk = decal_walk(whole, world);
    while next_pool_decal(&walk, whole, true, on_floor) {
        if (walk.tag & (PROJECTOR | 0x7u)) != (PROJECTOR | FB_LIQUID) {
            add_blood_splat(&splats, pbr_input, walk.tag, walk.base_color_texture_index, walk.decal, walk.uv, receiver);
            continue;
        }
        let wall_tile = (walk.tag & POOL_WALL_TILE) != 0u;
        let index = walk.metallic_roughness_texture_index;
        let slot = (walk.tag >> 8u) & 0xffu;
        let corner = vec2<i32>(i32(slot % POOL_SLOTS_SIDE), i32(slot / POOL_SLOTS_SIDE)) * POOL_SLOT_SIDE + POOL_APRON;
        let p = clamp(walk.uv, vec2(0.0), vec2(1.0)) * POOL_SIDE - 0.5;
        let nearest = textureLoad(mesh_view_bindings::clustered_decal_textures[index], corner + vec2<i32>(round(p)), 0);
        if nearest.w < 0.0 {
            continue;
        }
        let m = mesh_view_bindings::clustered_decals.decals[walk.decal].local_from_world;
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
        var weight_h: half = 0.0;
        var weight_slope_h = vec2<half>(0.0);
        var depth_h: half = 0.0;
        // The liquid surface's height over the fragment's place, in 32 bit: the world's heights times the half float
        // weight slope would put noise into the flow.
        var height = 0.0;
        var speed_h: half = 0.0;
        // Heat and scorch, in steps.
        var fire_h = vec2<half>(0.0);
        // The kind and blood share of the wettest texel around: at a rim the nearest may be dry.
        var wettest_h = vec2<half>(0.0);
        // The blood share by depth over the taps, a blood texel all blood: a volume fraction, so mixed kinds blend.
        var share_h: half = 0.0;
        // The depth and kind of the deepest tap that is not blood.
        var other_h = vec2<half>(0.0);
        // In texels: the depth's slope and the liquid surface's, which runs downhill.
        var depth_slope_h = vec2<half>(0.0);
        var height_slope = vec2(0.0);
        for (var j = 0; j < 4; j++) {
            for (var i = 0; i < 4; i++) {
                let t = textureLoad(mesh_view_bindings::clustered_decal_textures[index], corner + vec2<i32>(base) + vec2(i - 1, j - 1), 0);
                if abs(t.z - standing) > reach {
                    continue;
                }
                let w = wx[i] * wy[j];
                let s = vec2(sx[i] * wy[j], wx[i] * sy[j]);
                let wh = half(w);
                let sh = vec2<half>(s);
                let xh = half(t.x);
                weight_h += wh;
                weight_slope_h += sh;
                depth_h += xh * wh;
                height += (t.z - standing + t.x) * w;
                share_h += xh * wh * half(select(fract(t.y) / 0.98, 1.0, u32(t.y) == POOL_BLOOD));
                if xh > wettest_h.x {
                    wettest_h = vec2<half>(t.xy);
                }
                if u32(t.y) != POOL_BLOOD && xh > other_h.x {
                    other_h = vec2<half>(xh, half(floor(t.y)));
                }
                if t.w >= POOL_FIRE_BASE {
                    let code = t.w - POOL_FIRE_BASE;
                    let heat = floor(code / 32.0);
                    fire_h += vec2<half>(vec2(heat, code - heat * 32.0)) * wh;
                } else {
                    speed_h += half(max(t.w, 0.0)) * wh;
                }
                depth_slope_h += xh * sh;
                height_slope += (t.z - standing + t.x) * s;
            }
        }
        let weight = f32(weight_h);
        let weight_slope = vec2<f32>(weight_slope_h);
        var depth = f32(depth_h);
        var speed = f32(speed_h);
        var fire = vec2<f32>(fire_h);
        let wettest = vec2<f32>(wettest_h);
        let share = f32(share_h);
        let other = vec2<f32>(other_h);
        var depth_slope = vec2<f32>(depth_slope_h);
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
        let blood = saturate(select(fract(wettest.y) / 0.98, share / (depth * weight), depth > 1e-5));
        let bloody = select(blood, 0.0, paste);
        // World noise reads the fragment's place in the tile's own plane, so floors, walls and ceilings share one look.
        let ground = vec2(dot(world, along_u), dot(world, along_v));
        let flow = -(along_u * height_slope.x + along_v * height_slope.y);
        let rising = length(depth_world);
        let warp = (pool_noise(ground / POOL_WARP_SIZE_UU) - 0.5) * 2.0 * POOL_WARP_UU;
        let grout = POOL_GROUT_UU * saturate((POOL_EVEN_TONE - POOL_GROUT_FROM - tone) / POOL_GROUT_DARK);
        let inside = clamp((depth - POOL_FILM * POOL_EDGE) / max(rising, 1e-4), -16.0, 16.0) + warp + grout;
        let soft = max(POOL_SOFT_UU, 0.7 * pixel);
        var wet = smoothstep(-soft, soft, inside);
        var chunk = PoolChunk(0.0, vec2(0.0), vec4(0.0));
        if paste {
            chunk = pool_chunks(ground, soft);
            wet = max(wet, chunk.amount * saturate(inside + 2.0));
        }
        let soak = max(wet, smoothstep(-soft - POOL_SOAK_UU, -soft, inside) * (1.0 - bloody));
        // Scorch mottled by world noise, and on a dry surface, the soot mark and what still glows there.
        var soot = 0.0;
        if fire.y > 0.0 {
            soot = fire.y * (0.7 + 0.3 * pool_noise(ground / 3.0));
        }
        if wet <= pool.wet {
            pool.soak = max(pool.soak, soak);
            if pool.wet <= 0.0 && fire.x + fire.y > 0.004 {
                pool = Pool(vec3(1.0 - POOL_SCORCH_DARK * soot), POOL_SOOT * soot, 0.0, normal, 1.0, vec3(1.0), vec3(1.0), pool_flame(ground, fire.x), pool.soak);
            }
            continue;
        }
        var splat = vec4(0.0);
        if paste {
            if puke == PUKE_UNSOUGHT {
                puke = puke_splat_index(world, list);
            }
            splat = puke_texture(puke, ground, pixel);
        }
        let mixed_into = select(kind, u32(other.y), kind == POOL_BLOOD && other.x > 0.0);
        let look = pool_look(kind, mixed_into, blood, wet, max(depth, 0.0), ground, chunk, splat);
        // The liquid's surface: the plane tilted by the depth's slope, rippling where it runs downhill.
        var tilt = depth_world * POOL_RISE + along_u * look.tilt.x + along_v * look.tilt.y;
        var roughness = look.roughness;
        var body = look.body;
        if bloody > 0.0 {
            let clot = smoothstep(0.62, 0.8, pool_noise(ground / BLOOD_CLOT_UU + 41.0)) * bloody;
            body *= mix(1.0, mix(0.7, 1.2, pool_noise(ground / BLOOD_MOTTLE_UU + 7.0)) * mix(1.0, 0.7, clot), bloody);
            roughness += BLOOD_CLOT_ROUGHNESS * clot;
            let swell = pool_noise_slope(ground / BLOOD_SWELL_UU + 13.0) / BLOOD_SWELL_UU;
            tilt += (along_u * swell.x + along_v * swell.y) * (BLOOD_SWELL * bloody);
            tilt += depth_world / max(rising, 1e-4) * (BLOOD_MENISCUS * bloody * 4.0 * wet * (1.0 - wet));
        }
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
        let scattered = body * (1.0 - look.through);
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
        pool = Pool(mix(vec3(1.0), look.through, wet) * scorched, max(scattered, vec3(0.0)) * wet + POOL_SOOT * soot, wet, surface, roughness, sheen, mix(vec3(1.0), BLOOD_SHEEN, bloody), glow, max(soak, pool.soak));
    }
    if splats.wet > 0.0 {
        let under = blood_splat(normal, splats.tilt, splats.wet, splats.dry / splats.weight);
        if pool.wet > 0.0 {
            // The splat lies on the surface under our liquid, which lets through what its absorption leaves.
            let wet = pool.wet;
            pool = Pool(pool.through * under.through, pool.color + pool.through * under.color, 1.0 - (1.0 - wet) * (1.0 - under.wet), normalize(mix(under.normal, pool.normal, wet)), mix(under.roughness, pool.roughness, wet), pool.sheen, mix(under.tint, pool.tint, wet), pool.glow, max(pool.soak, under.soak));
        } else {
            let soaked = pool.soak;
            pool = under;
            pool.soak = max(pool.soak, soaked);
        }
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

// The next decal of `list` over the walk's place, passing over by their tag, before their box is tested, the decals
// `pool_at` and `splat_pool` never read: all but the game's blood splats, which carry a normal map of their own while
// our liquids are on, and with `liquids` our liquid's tiles that may lie here (a floor tile only on a floor, no puke
// splat's child). The liquid's texture rides as the decal's metallic-roughness map, which Bevy's own decal pass reads
// only under a base colour, so a standard material lying in a pool does not take its data as colour.
@if(CLUSTERED_DECALS_ARE_USABLE)
fn next_pool_decal(walk: ptr<function, DecalWalk>, list: DecalList, liquids: bool, on_floor: bool) -> bool {
    (*walk).at += 1;
    while (*walk).at < list.end {
        let decal = listed_decal(list, (*walk).at);
        let tag = mesh_view_bindings::clustered_decals.decals[decal].tag;
        var wanted = i32(mesh_view_bindings::clustered_decals.decals[decal].normal_map_texture_index) >= 0;
        if (tag & (PROJECTOR | 0x7u)) == (PROJECTOR | FB_LIQUID) {
            wanted = liquids && (tag & PUKE_SPLAT) == 0u && ((tag & POOL_WALL_TILE) != 0u || on_floor)
                && i32(mesh_view_bindings::clustered_decals.decals[decal].metallic_roughness_texture_index) >= 0;
        }
        if wanted && decal_holds(walk, decal) {
            return true;
        }
        (*walk).at += 1;
    }
    return false;
}

// The next decal of `list` over the walk's place, passing over by their tag, before their box is tested, the decals
// `project` never draws here: our liquid's data, and the game's blood once `pool_at` drew it.
@if(CLUSTERED_DECALS_ARE_USABLE)
fn next_pasted_decal(walk: ptr<function, DecalWalk>, list: DecalList, pooled: bool) -> bool {
    (*walk).at += 1;
    while (*walk).at < list.end {
        let decal = listed_decal(list, (*walk).at);
        let tag = mesh_view_bindings::clustered_decals.decals[decal].tag;
        let marked = i32(mesh_view_bindings::clustered_decals.decals[decal].normal_map_texture_index) >= 0;
        if (tag & (PROJECTOR | 0x7u)) != (PROJECTOR | FB_LIQUID) && !(pooled && marked) && decal_holds(walk, decal) {
            return true;
        }
        (*walk).at += 1;
    }
    return false;
}

// Over the lit linear `color`, in cluster order; `texture` is read by material blending. `pooled`: the surface took
// `pool_at` with these targets, which drew the game's blood splats already.
fn project(pbr_input: PbrInput, list: DecalList, texture: vec3<f32>, color: vec4<f32>, surface: u32, pooled: bool) -> vec4<f32> {
    var out = color;
@if(CLUSTERED_DECALS_ARE_USABLE) {
    var walk = decal_walk(list, pbr_input.world_position.xyz);
    var base = vec3(0.0);
    var frame = vec3(0.0);
    var shine = 1.0;
    var drawn = false;
    while next_pasted_decal(&walk, list, pooled) {
        let texel = projector_texel(pbr_input, walk.tag, walk.base_color_texture_index, walk.decal, walk.uv, surface);
        if texel.a < 0.0 {
            continue;
        }
        if !drawn {
            base = to_display(texture);
            frame = to_display(color.rgb);
            // The light on the surface, as its lit colour over its own, both in display space.
            shine = clamp(dot(frame, LUMA) / max(dot(base, LUMA), 0.04), 0.0, 2.0);
            drawn = true;
        }
        let projected = to_display(texel.rgb);
        let material = (walk.tag >> 8u) & 0x3u;
        var src = projected;
        if material == PB_MODULATE {
            src = projected * base;
        } else if material == PB_ALPHA_BLEND {
            src = mix(projected, base, texel.a);
        } else if material == PB_ADD {
            src = min(projected + base, vec3(1.0));
        }
        let under = frame;
        // With our liquids on, the game's blood pictures (marked by a normal map of their own) lie where no pool is
        // drawn (bodies, blended meshes, darken and see-through surfaces) as our fresh blood, lit as the surface under
        // them, whatever their blend.
        if walk.normal_map_texture_index >= 0 {
            frame = mix(frame, to_display(BLOOD_BODY) * shine, texel.a);
        } else {
            switch walk.tag & 0x7u {
                case FB_OVERWRITE: { frame = src; }
                case FB_MODULATE: { frame = 2.0 * src * frame; }
                case FB_ALPHA_BLEND: { frame = mix(frame, src, texel.a); }
                case FB_ALPHA_MODULATE: { frame = src + frame * (1.0 - texel.a); }
                case FB_TRANSLUCENT: { frame = src + frame; }
                case FB_DARKEN: { frame = frame * (1.0 - src); }
                case FB_BRIGHTEN: { frame = src + frame * (1.0 - src); }
                default: {}
            }
        }
        // Drying darkens what the splat changed, whatever its blend; fading takes the frame back to what lay under it.
        let dry = (walk.tag >> DRY_SHIFT) & DRY_MASK;
        if dry != 0u {
            frame = mix(frame, frame * DRY_TINT, f32(dry) / 15.0 * saturate(distance(frame, under) * 2.0));
        }
        let fade = (walk.tag >> FADE_SHIFT) & FADE_MASK;
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
