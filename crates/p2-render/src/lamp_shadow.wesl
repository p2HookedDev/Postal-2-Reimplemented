import package::p2_render::modern_base::{LUMINANCE, LampLight, fresnel_five, ggx, modern_bits, to_eye};
import package::p2_render::sun_shadow::{SHADOW_BIAS, SHADOW_NORMAL_OFFSET, SOFT_EDGE_UNITS, SUN_FACING_FADE, ShadowLookup, disk_turn, held_lookup, vogel_tap};
import package::p2_render::probes::read_probes;
import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings::view,
    pbr_bindings,
};
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};
@if(DEPTH_PREPASS)
import bevy_pbr::render::mesh_view_bindings::depth_prepass_texture;

@if(LAMP_MASK_PASS)
@group(1) @binding(3) var pass_book: texture_2d<f32>;
@if(LAMP_MASK_PASS)
@group(1) @binding(4) var pass_atlas: texture_2d<f32>;
@if(LAMP_MASK_PASS)
@group(1) @binding(5) var pass_atlas_sampler: sampler;

fn no_lamps() -> LampLight {
    return LampLight(vec3(0.0), vec3(0.0), vec3(0.0), vec3(0.0), vec4(-1.0, 0.0, 0.0, 0.0), 1.0, vec3(0.0));
}

// The atlas side in units, and the guard band around a face in texels. Matches `lamps/atlas.rs`.
const ATLAS_UNITS: f32 = 128.0;
const ATLAS_GUARD: f32 = 3.0;
const ATLAS_UNMAPPED: u32 = 7u;
const LAMP_CORE_SQUARED: f32 = 0.09;
const LAMP_ENERGY: f32 = 1.4485;
const LAMP_FADE_FROM: f32 = 21.0;
const LAMP_FADE_TO: f32 = 30.0;
const LAMP_SMALLEST: f32 = 0.01;
const LAMP_SOURCE_SHARE: f32 = 0.08;
// The lamp size level sits above the modern flag's effect bits. Matches `material_class.rs`.
const LAMP_SIZE_SHIFT: u32 = 7u;
const LAMP_SIZE_TOP: f32 = 10.0;
const BOOK_SIZE_TEXEL: u32 = 6u;
// Set above the tile in a lamp's colour texel when its size texel holds a size of its own. Matches `lamps.rs`.
const BOOK_SIZED: u32 = 1048576u;
// The lamp book rides in the material's anisotropy texture, four floats a texel: a header (grid corner,
// cell side, sides, cell start), seven texels a lamp, a texel a cell (list start and count), then the
// lamp lists four a texel. Matches `lamps/cells.rs`.
const BOOK_WIDTH: u32 = 2048u;
const BOOK_HEADER: u32 = 2u;
const BOOK_RECORD: u32 = 7u;
// The rows the book ends with, counted back from its end: the movers' tile grid, their tiles' row, the capsules'
// fine and coarse rows. Matches `lamps/cells.rs`.
const TILE_GRID_FROM_END: u32 = 20u;
// The decals' screen tile lists (`decal_bins.wesl`) take the rows before the movers' ones.
const DECAL_BINS_FROM_END: u32 = 277u;
const TILES_FROM_END: u32 = 3u;
const CAPSULES_FROM_END: u32 = 2u;
// A detail face's corner is counted in these texels. Matches `lamps/faces.rs`.
const DETAIL_UNIT: f32 = 64.0;
// A texel holds a distance in a 16 bit float: its steps are this share of the distance at most.
const DEPTH_STEP: f32 = 0.001;

// Bindless indices and sizes of the book and atlas, looked up once a pixel. A 1x1 book is Bevy's default
// image (no book) and a 0-wide one is no book at all; the atlas rides in the occlusion texture and the
// screen pass's shadows in the normal map texture.
struct Book {
    pages: u32,
    atlas: u32,
    atlas_sampling: u32,
    screen: u32,
    size: vec2<u32>,
    side: f32,
}

fn book_of(in: VertexOutput) -> Book {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    let pages = material_indices[slot].anisotropy_texture;
    let atlas = material_indices[slot].occlusion_texture;
    return Book(pages, atlas, material_indices[slot].occlusion_sampler, material_indices[slot].normal_map_texture, textureDimensions(bindless_textures_2d[pages]), f32(textureDimensions(bindless_textures_2d[atlas]).x));
}
@elif(PBR_ANISOTROPY_TEXTURE_SUPPORTED) {
    return Book(0u, 0u, 0u, 0u, textureDimensions(pbr_bindings::anisotropy_texture), f32(textureDimensions(pbr_bindings::occlusion_texture).x));
}
@else {
    return Book(0u, 0u, 0u, 0u, vec2(0u, 1u), f32(textureDimensions(pbr_bindings::occlusion_texture).x));
}
}

fn book_texel(book: Book, index: u32) -> vec4<f32> {
    let at = vec2(index % BOOK_WIDTH, index / BOOK_WIDTH);
@if(LAMP_MASK_PASS) {
    return textureLoad(pass_book, at, 0);
}
@elif(BINDLESS) {
    return textureLoad(bindless_textures_2d[book.pages], at, 0);
}
@elif(PBR_ANISOTROPY_TEXTURE_SUPPORTED) {
    return textureLoad(pbr_bindings::anisotropy_texture, at, 0);
}
@else {
    return vec4(0.0);
}
}

fn lamp_list(book: Book, position: vec3<f32>) -> vec2<u32> {
    if book.size.x != BOOK_WIDTH {
        return vec2(0u);
    }
    let grid = book_texel(book, 0u);
    let sides = book_texel(book, 1u);
    let cell = floor((position - grid.xyz) / grid.w);
    if any(cell < vec3(0.0)) || any(cell >= sides.xyz) {
        return vec2(0u);
    }
    let at = vec3<u32>(cell);
    let size = vec3<u32>(sides.xyz);
    return vec2<u32>(book_texel(book, u32(sides.w) + (at.z * size.y + at.y) * size.x + at.x).xy);
}

const LAMP_GLOSS_FROM: f32 = 0.03;

fn atlas_quad(book: Book, side: f32, corner: vec2<f32>) -> vec4<f32> {
    let at = (corner + 1.0) / side;
@if(LAMP_MASK_PASS) {
    return textureGather(0, pass_atlas, pass_atlas_sampler, at);
}
@elif(BINDLESS) {
    return textureGather(0, bindless_textures_2d[book.atlas], bindless_samplers_filtering[book.atlas_sampling], at);
}
@else {
    return textureGather(0, pbr_bindings::occlusion_texture, pbr_bindings::occlusion_sampler, at);
}
}

// A lamp's edge is four blended reads on the corners of a texel-aligned square, as wide as the sun's edge
// in world units where the tile allows and between these in texels, so the reads stay inside the guard band.
const LAMP_EDGE_LEAST_TEXELS: f32 = 1.0;
const LAMP_EDGE_MOST_TEXELS: f32 = 2.0;
const HELD_LAMP_EDGE_TEXELS: f32 = 2.0;
const HELD_LAMP_TAPS: i32 = 16;

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

fn atlas_blended(book: Book, side: f32, low: vec2<f32>, size: f32, at: vec2<f32>, depth: f32) -> vec2<f32> {
    let spot = clamp(at, low + 1.0, low + size - 1.0) - 0.5;
    let base = floor(spot);
    let blend = spot - base;
    let lit = step(vec4(depth), atlas_quad(book, side, base));
    return vec2(mix(mix(lit.w, lit.z, blend.x), mix(lit.x, lit.y, blend.x), blend.y), lit.x + lit.y + lit.z + lit.w);
}

// The depth a tap `by` texels off the point is compared against: where the point's own surface lies along
// the tap's ray, so a tilted surface does not shade itself; at most a texel of depth a texel off.
// `reach` is `length(seen)` and `plane` is `dot(normal, seen)`, the same for every tap.
fn receiver_depth(seen: vec3<f32>, normal: vec3<f32>, reach: f32, plane: f32, by: vec2<f32>, span: f32, texel_world: f32, margin: f32, radius: f32) -> f32 {
    let ray = vec3(seen.x + by.x * seen.z / span, seen.y - by.y * seen.z / span, seen.z);
    let slope = dot(normal, ray);
    var along = reach;
    if abs(slope) > 1e-6 {
        along = plane / slope * length(ray);
    }
    let most = texel_world * length(by);
    return (clamp(along, reach - most, reach + most) - margin) / radius;
}

fn cube_face(from_lamp: vec3<f32>) -> u32 {
    let m = abs(from_lamp);
    if m.x >= m.y && m.x >= m.z {
        return select(1u, 0u, from_lamp.x > 0.0);
    }
    if m.y >= m.z {
        return select(3u, 2u, from_lamp.y > 0.0);
    }
    return select(5u, 4u, from_lamp.z > 0.0);
}

// `from_lamp` is the point seen from the lamp; a texel holds distance over the radius. `low` and `size` are
// the face's corner and side in texels, `side` the atlas's. A point just past the edge reads the guard band.
fn face_shadow(book: Book, low: vec2<f32>, size: f32, face: u32, from_lamp: vec3<f32>, normal: vec3<f32>, radius: f32, texel_world: f32, margin: f32, side: f32, lookup: ShadowLookup) -> f32 {
    return square_shadow(book, low, size, 0.5 * size - ATLAS_GUARD, face_view(from_lamp, face), face_view(normal, face), radius, texel_world, margin, side, lookup);
}

fn square_shadow(book: Book, low: vec2<f32>, size: f32, span: f32, seen: vec3<f32>, tilted: vec3<f32>, radius: f32, texel_world: f32, margin: f32, side: f32, lookup: ShadowLookup) -> f32 {
    let at = low + 0.5 * size + vec2(seen.x, -seen.y) / max(seen.z, 1e-4) * span;
    let half = 0.5 * clamp(SOFT_EDGE_UNITS / texel_world, LAMP_EDGE_LEAST_TEXELS, LAMP_EDGE_MOST_TEXELS);
    let reach = length(seen);
    let plane = dot(tilted, seen);
    if held_lookup(lookup) {
        let wide = max(half, HELD_LAMP_EDGE_TEXELS);
        let turn = disk_turn(lookup.seed);
        var shown = 0.0;
        for (var tap = 0; tap < HELD_LAMP_TAPS; tap++) {
            let by = vogel_tap(tap, HELD_LAMP_TAPS, turn) * wide;
            shown += atlas_blended(book, side, low, size, at + by, receiver_depth(seen, tilted, reach, plane, by, span, texel_world, margin, radius)).x;
        }
        return shown / f32(HELD_LAMP_TAPS);
    }
    var lit = vec2(0.0);
    for (var corner = 0; corner < 4; corner++) {
        // One diagonal first: where its eight texels agree, no edge is near and the other two reads are saved.
        if corner == 2 && (lit.y < 0.5 || lit.y > 7.5) {
            return lit.x * 0.5;
        }
        let by = vec2(select(-half, half, corner == 1 || corner == 2), select(-half, half, corner % 2 == 1));
        lit += atlas_blended(book, side, low, size, at + by, receiver_depth(seen, tilted, reach, plane, by, span, texel_world, margin, radius));
    }
    return lit.x * 0.25;
}

fn tile_shadow(book: Book, corner: vec2<f32>, size: f32, face: u32, plain: bool, position: vec3<f32>, normal: vec3<f32>, lamp_position: vec3<f32>, lamp_radius: f32, distance_to: f32, tilt: f32, side: f32, lookup: ShadowLookup) -> f32 {
    let texel_world = 2.0 * distance_to / size;
    let from_lamp = position + normal * (texel_world * SHADOW_NORMAL_OFFSET * lookup.offset) - lamp_position;
    let margin = texel_world * SHADOW_BIAS * (1.0 + 0.5 * tilt) * lookup.offset + distance_to * DEPTH_STEP;
    if !plain {
        return face_shadow(book, corner, size, face, from_lamp, normal, lamp_radius, texel_world, margin, side, lookup);
    }
    let own = cube_face(from_lamp);
    return face_shadow(book, corner + vec2(f32(own % 3u), f32(own / 3u)) * size, size, own, from_lamp, normal, lamp_radius, texel_world, margin, side, lookup);
}

fn detail_face(book: Book, record: u32, face: u32) -> vec3<f32> {
    let packed = u32(book_texel(book, record + 3u + face / 4u)[face % 4u]);
    if packed == 0u {
        return vec3(0.0);
    }
    let at = packed - 1u;
    return vec3(vec2(f32(at & 255u), f32((at >> 8u) & 255u)) * DETAIL_UNIT, DETAIL_UNIT * f32(1u << (at >> 16u)));
}

// A lamp's record ends with where its mover tiles start in the book's third-last row and how many there are.
// Each tile is two texels: its axis out of the lamp and the tangent of its half angle; its square's corner,
// side in texels and how far its shadow has faded in. Matches `lamps/movers.rs`.
const TILE_GRID_SIDE: u32 = 64u;
const TILE_GRID_COLUMN_TEXELS: u32 = 8u;
// How much wider than its tangent a tile reaches. Matches `lamps/tile_grid.rs`.
const REACH_WIDENING: f32 = 1.4143;

// The rows before the tiles hold a column grid around the eye (low corner in x and z, column side, 1), then
// `TILE_GRID_COLUMN_TEXELS` a column naming its tiles, -1 first when more shade it than fit.
// Returns where the column under `position` starts, 0 outside the grid. Matches `lamps/tile_grid.rs`.
fn tile_column(book: Book, position: vec3<f32>) -> u32 {
    let row = (book.size.y - TILE_GRID_FROM_END) * BOOK_WIDTH;
    let grid = book_texel(book, row);
    let column = floor((position.xz - grid.xy) / max(grid.z, 1.0));
    if grid.w <= 0.0 || any(column < vec2(0.0)) || any(column >= vec2(f32(TILE_GRID_SIDE))) {
        return 0u;
    }
    return row + 1u + (u32(column.y) * TILE_GRID_SIDE + u32(column.x)) * TILE_GRID_COLUMN_TEXELS;
}

fn mover_tile_shadow(book: Book, at: u32, lit: f32, from_lamp: vec3<f32>, normal: vec3<f32>, lamp_radius: f32, distance_to: f32, tilt: f32, side: f32, lookup: ShadowLookup) -> f32 {
    let aim = book_texel(book, at);
    let axis = aim.xyz;
    let along = dot(from_lamp, axis);
    let reach = along * aim.w * REACH_WIDENING;
    if along <= 0.0 || dot(from_lamp, from_lamp) - along * along > reach * reach {
        return lit;
    }
    let square = book_texel(book, at + 1u);
    if square.w <= 0.0 {
        return lit;
    }
    let helper = select(vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), abs(axis.y) > 0.9);
    let right = normalize(cross(helper, axis));
    let up = cross(axis, right);
    let size = square.z;
    let texel_world = 2.0 * aim.w * distance_to / size;
    let moved = from_lamp + normal * (texel_world * SHADOW_NORMAL_OFFSET * lookup.offset);
    let seen = vec3(dot(moved, right), dot(moved, up), dot(moved, axis));
    let span = (0.5 * size - ATLAS_GUARD) / aim.w;
    if seen.z <= 0.0 || any(abs(seen.xy) * span > vec2(seen.z * (0.5 * size - 1.0))) {
        return lit;
    }
    let tilted = vec3(dot(normal, right), dot(normal, up), dot(normal, axis));
    let margin = texel_world * SHADOW_BIAS * (1.0 + 0.5 * tilt) * lookup.offset + distance_to * DEPTH_STEP;
    let shade = square_shadow(book, square.xy, size, span, seen, tilted, lamp_radius, texel_world, margin, side, lookup);
    return min(lit, mix(1.0, shade, saturate(square.w)));
}

// `held` is the lamp's tile start and count; `column` the point's grid column, 0 outside it, where all tiles
// are walked. Names are read here rather than carried through the lamp loop, so the loop holds fewer registers.
fn mover_shadow(book: Book, held: vec4<f32>, column: u32, position: vec3<f32>, normal: vec3<f32>, lamp_position: vec3<f32>, lamp_radius: f32, distance_to: f32, tilt: f32, side: f32, lookup: ShadowLookup) -> f32 {
    let start = u32(held.x);
    let count = u32(held.y);
    let row = (book.size.y - TILES_FROM_END) * BOOK_WIDTH;
    let from_lamp = position - lamp_position;
    var names = vec4(-1.0);
    if column > 0u {
        names = book_texel(book, column);
    }
    var lit = 1.0;
    if names.x < 0.0 {
        for (var tile = 0u; tile < count && lit > 0.0; tile++) {
            lit = mover_tile_shadow(book, row + (start + tile) * 2u, lit, from_lamp, normal, lamp_radius, distance_to, tilt, side, lookup);
        }
        return lit;
    }
    for (var slot = 0u; slot < 4u * TILE_GRID_COLUMN_TEXELS && lit > 0.0; slot++) {
        if slot > 0u && slot % 4u == 0u {
            names = book_texel(book, column + slot / 4u);
        }
        let name = u32(names[slot % 4u]);
        if name == 0u {
            break;
        }
        let tile = name - 1u;
        if tile >= start && tile < start + count {
            lit = mover_tile_shadow(book, row + tile * 2u, lit, from_lamp, normal, lamp_radius, distance_to, tilt, side, lookup);
        }
    }
    return lit;
}

struct LampAt {
    record: u32,
    position: vec3<f32>,
    radius: f32,
    x2: f32,
    distance_to: f32,
    to_lamp: vec3<f32>,
    facing: f32,
    cone: f32,
    look: vec4<f32>,
    shown: vec3<f32>,
    share: f32,
}

fn lamp_at(book: Book, list: vec2<u32>, i: u32, position: vec3<f32>, normal: vec3<f32>, eye_at: vec3<f32>, slack: f32) -> LampAt {
    var at = LampAt(0u, vec3(0.0), 0.0, 1.0, 0.0, vec3(0.0), 0.0, 0.0, vec4(0.0), vec3(0.0), 0.0);
    let lamp = u32(book_texel(book, list.x + i / 4u)[i % 4u]);
    at.record = BOOK_HEADER + lamp * BOOK_RECORD;
    let place = book_texel(book, at.record);
    at.position = place.xyz;
    let to_light = at.position - position;
    let d2 = dot(to_light, to_light);
    at.radius = abs(place.w);
    let inverse_range = 1.0 / (at.radius * at.radius);
    at.x2 = d2 * inverse_range;
    if at.x2 >= 1.0 {
        return at;
    }
    at.distance_to = sqrt(max(d2, 1e-4));
    at.to_lamp = to_light / at.distance_to;
    at.facing = dot(normal, at.to_lamp);
    if at.facing <= -slack {
        return at;
    }
    let fade = 1.0 - smoothstep(LAMP_FADE_FROM, LAMP_FADE_TO, length(at.position - eye_at) * sqrt(inverse_range));
    if fade <= 0.0 {
        return at;
    }
    var cone = 1.0;
    if place.w < 0.0 {
        let spot = book_texel(book, at.record + 2u);
        let along = vec3(spot.xy, sqrt(max(0.0, 1.0 - dot(spot.xy, spot.xy))) * sign(spot.z));
        let share = saturate(dot(-along, at.to_lamp) * abs(spot.z) + spot.w);
        cone = share * share;
    }
    at.cone = cone * fade;
    at.look = book_texel(book, at.record + 1u);
    at.shown = lamp_shown(at.look.rgb, at.x2, at.cone);
    at.share = dot(at.shown, LUMINANCE) * (at.facing + slack);
    return at;
}

const COLUMN_UNREAD: u32 = 0xffffffffu;

fn lamp_shadows(book: Book, lamp: LampAt, position: vec3<f32>, ground: vec3<f32>, side: f32, unit: f32, column: ptr<function, u32>, movers_near: bool, lookup: ShadowLookup) -> vec2<f32> {
    let faces = dot(ground, lamp.to_lamp);
    let tilt = min(sqrt(saturate(1.0 - faces * faces)) / max(faces, 0.1), 4.0);
    let face = cube_face(position - lamp.position);
    let detail = detail_face(book, lamp.record, face);
    let packed = u32(lamp.look.w);
    let level = (packed >> 16u) & 15u;
    var walled = 1.0;
    if detail.z > 0.0 {
        walled = tile_shadow(book, detail.xy, detail.z, face, false, position, ground, lamp.position, lamp.radius, lamp.distance_to, tilt, side, lookup);
    } else if level != ATLAS_UNMAPPED {
        let corner = vec2(f32(packed & 255u), f32((packed >> 8u) & 255u)) * unit;
        walled = tile_shadow(book, corner, unit * f32(1u << level), face, true, position, ground, lamp.position, lamp.radius, lamp.distance_to, tilt, side, lookup);
    }
    var movers = 1.0;
    if walled > 0.0 && movers_near {
        let held = book_texel(book, lamp.record + 5u);
        if held.y > 0.0 {
            if *column == COLUMN_UNREAD {
                *column = tile_column(book, position);
            }
            movers = mover_shadow(book, held, *column, position, ground, lamp.position, lamp.radius, lamp.distance_to, tilt, side, lookup);
        }
    }
    return vec2(walled, movers);
}

// Screen pass texel: low byte the list start plus 1, a bit per held lamp, then a byte per held lamp's shadows
// and the start's second byte at `SCREEN_CHECK_BYTE`. A header row holds 1 and the view matrix; sun rows start
// `SCREEN_SUN_FROM` rows past the pixels, one texel per `SCREEN_SUN_PIXELS`. Matches `lamps/shadow_mask.rs`.
const SCREEN_LIST_LAMPS: u32 = 16u;
const SCREEN_SLOTS: u32 = 4u;
const SCREEN_CHECK_BYTE: u32 = 8u;
const SCREEN_SUN_FROM: u32 = 1u;
const SCREEN_SUN_PIXELS: u32 = 4u;
const SCREEN_DEPTH_SLACK: f32 = 0.0001;

// Which of the pixel's lamps the screen pass holds and their shadow byte channels, all 0 where it holds none.
// `unheld` becomes what an unheld lamp lets through once the pass ran: nothing, since the pass holds every
// lamp the static world lets through bar the weakest.
fn screen_shadows(book: Book, frag: vec4<f32>, start: u32, unheld: ptr<function, vec2<f32>>) -> vec4<u32> {
@if(BINDLESS && DEPTH_PREPASS) {
    let pixel = vec2<u32>(frag.xy);
    if !screen_written(book.screen, pixel) {
        return vec4(0u);
    }
    *unheld = vec2(0.0);
    if abs(textureLoad(depth_prepass_texture, vec2<i32>(frag.xy), 0) - frag.z) > frag.z * SCREEN_DEPTH_SLACK {
        return vec4(0u);
    }
    let texel = vec4<u32>(textureLoad(bindless_textures_2d[book.screen], pixel, 0));
    let check = (start + 1u) & 0xffffu;
    if (texel.x & 255u) != (check & 255u) || screen_byte(texel, SCREEN_CHECK_BYTE) != check >> 8u {
        return vec4(0u);
    }
    return vec4(texel.x >> 8u, texel.yzw);
}
@else {
    return vec4(0u);
}
}

// Whether the screen pass wrote this image for this view this frame: tall enough for the prepass rows,
// holding `pixel`, with a header of 1 and the view's matrix.
@if(BINDLESS && DEPTH_PREPASS)
fn screen_written(screen: u32, pixel: vec2<u32>) -> bool {
    let size = textureDimensions(bindless_textures_2d[screen]);
    let rows = textureDimensions(depth_prepass_texture).y;
    if size.y != rows + SCREEN_SUN_FROM + (rows + SCREEN_SUN_PIXELS - 1u) / SCREEN_SUN_PIXELS || any(pixel >= vec2(size.x, rows)) {
        return false;
    }
    return textureLoad(bindless_textures_2d[screen], vec2(0u, rows), 0).x == 1.0
        && all(textureLoad(bindless_textures_2d[screen], vec2(1u, rows), 0) == view.clip_from_world[0])
        && all(textureLoad(bindless_textures_2d[screen], vec2(2u, rows), 0) == view.clip_from_world[3]);
}

@if(SCREEN_SHADOWS_ONLY)
fn screen_sun(in: VertexOutput, frag: vec2<f32>) -> vec3<f32> {
    let screen = material_indices[mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu].normal_map_texture;
    let pixel = vec2<u32>(frag);
    if !screen_written(screen, pixel) {
        return vec3(1.0);
    }
    let row = textureDimensions(depth_prepass_texture).y + SCREEN_SUN_FROM + pixel.y / SCREEN_SUN_PIXELS;
    let held = u32(textureLoad(bindless_textures_2d[screen], vec2(pixel.x, row), 0)[pixel.y % SCREEN_SUN_PIXELS]);
    return vec3<f32>(vec3(held, held >> 8u, held >> 16u) & vec3(255u)) / 255.0;
}

@if(SCREEN_SHADOWS_ONLY)
fn screen_position(frag: vec4<f32>) -> vec3<f32> {
    let pixel = vec2<u32>(frag.xy);
    let z = textureLoad(depth_prepass_texture, vec2<i32>(pixel), 0);
    let uv = (vec2<f32>(pixel) + 0.5 - view.viewport.xy) / view.viewport.zw;
    let seen = view.world_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return seen.xyz / seen.w;
}

fn screen_shadow(held: vec4<u32>, slot: u32) -> vec2<f32> {
    return vec2(f32(screen_byte(held, 2u * slot)), f32(screen_byte(held, 2u * slot + 1u))) / 255.0;
}

fn screen_byte(held: vec4<u32>, at: u32) -> u32 {
    return (held[1u + at / 3u] >> (8u * (at % 3u))) & 255u;
}

// Light from placed lamps and probes, before surface colour; terrain layers share one sum. `ground` is the
// normal under any liquid: shadow maps hold the surface, and a liquid's tilt there cuts acne into highlights.
fn lamp_light(in: VertexOutput, position: vec3<f32>, normal: vec3<f32>, ground: vec3<f32>, gloss: bool, surface: vec3<f32>, flag: f32, lookup: ShadowLookup) -> LampLight {
    var sum = no_lamps();
    if surface.z > 0.5 {
        return sum;
    }
    let book = book_of(in);
    let sized = min(f32((modern_bits(flag) >> LAMP_SIZE_SHIFT) & 15u) / LAMP_SIZE_TOP, 1.0);
    let side = book.side;
    let unit = side / ATLAS_UNITS;
    let rough = surface.x;
    let eye = to_eye(position);
@if(SCREEN_SHADOWS_ONLY)
    let list = lamp_list(book, screen_position(in.position));
@else
    let list = lamp_list(book, position);
    var unshaded = vec3(0.0);
    var given = vec3(0.0);
    var reaching = vec3(0.0);
    var column = COLUMN_UNREAD;
    var movers_near = true;
@if(!SCREEN_SHADOWS_ONLY) {
    if list.y > 2u && unit >= 1.0 && book.size.y >= 8u {
        column = tile_column(book, position);
        movers_near = column == 0u || book_texel(book, column).x != 0.0;
    }
}
    var screen = vec4(0u);
    var unheld = vec2(1.0);
@if(!P2_SEE_THROUGH && !SKY_VIEW) {
    if unit >= 1.0 && list.y > 0u {
        screen = screen_shadows(book, in.position, list.x, &unheld);
    }
}
    for (var i = 0u; i < list.y; i++) {
        let lamp = lamp_at(book, list, i, position, normal, view.world_position.xyz, 0.0);
        if lamp.share < LAMP_SMALLEST {
            continue;
        }
        var shadow = saturate(lamp.facing / SUN_FACING_FADE);
        var walled = shadow;
        var untested = unit < 1.0;
        if unit >= 1.0 {
            var through = unheld;
            if i < SCREEN_LIST_LAMPS && (screen.x & (1u << i)) != 0u {
                through = screen_shadow(screen, countOneBits(screen.x & ((1u << i) - 1u)));
            } else {
@if(SCREEN_SHADOWS_ONLY)
                untested = true;
@if(!SCREEN_SHADOWS_ONLY)
                through = lamp_shadows(book, lamp, position, ground, side, unit, &column, movers_near, lookup);
            }
            shadow *= through.x;
            walled = shadow;
            shadow = min(shadow, through.y);
        }
        let lamp_gives = lamp.shown * (lamp.facing * walled);
        unshaded += lamp_gives;
        sum.unshadowed += select(vec3(0.0), lamp_gives, untested);
        given += lamp_gives * lamp_gives;
        reaching += lamp_gives * lamp_gives * (shadow / max(walled, 1e-4));
        if shadow <= 0.0 {
            continue;
        }
        var source = lamp.radius * LAMP_SOURCE_SHARE;
        if sized > 0.0 && (u32(lamp.look.w) & BOOK_SIZED) != 0u {
            source = mix(source, book_texel(book, lamp.record + BOOK_SIZE_TEXEL).x, sized);
        }
        let a = saturate(rough * rough + source / (2.0 * lamp.distance_to));
        add_lamp(&sum, lamp.shown, lamp.look.rgb, lamp.x2, lamp.cone, shadow, lamp.facing, lamp.to_lamp, normal, eye, gloss && lamp.share >= LAMP_GLOSS_FROM, a);
    }
    // A mover's shadow takes its lamp's share of the linear light and no more: summed in display light, one lamp of
    // four alike in shadow would take 44 % of the light for its 25 %.
    sum.diffuse = unshaded * sqrt(reaching / max(given, vec3(1e-8)));
    sum.grounded = grounded(book, position, normal);
    read_probes(in, position, normal, &sum, lookup.offset);
    return sum;
}

// A column of the grid is this many to a side; a capsule darkens out to this many of its radii, and never by
// more than the most. Matches `lamps/grounding.rs`.
const GROUND_SIDE: u32 = 32u;
const GROUND_REACH: f32 = 3.0;
const GROUND_MOST: f32 = 0.6;
const GROUND_HANDOVER: f32 = 2.0;

// Movers near the eye block light from all around as one capsule each. A book row holds a column grid
// around the eye (low corner in x and z, column side, count), up to four capsules a column, then the capsules.
// The book ends with two such rows: a fine grid near the eye and a coarse one past it.
fn grounded(book: Book, position: vec3<f32>, normal: vec3<f32>) -> f32 {
    if book.size.x != BOOK_WIDTH {
        return 1.0;
    }
    let row = (book.size.y - CAPSULES_FROM_END) * BOOK_WIDTH;
    let fine = grid_open(book, row, book_texel(book, row), position, normal);
    if fine.y >= GROUND_HANDOVER {
        return fine.x;
    }
    let coarse_grid = book_texel(book, row + BOOK_WIDTH);
    let rim = 0.5 * f32(GROUND_SIDE) * coarse_grid.z;
    let fade = 1.0 - smoothstep(rim - 3.0 * coarse_grid.z, rim - coarse_grid.z, distance(position.xz, view.world_position.xz));
    var far = 1.0;
    if fade > 0.0 {
        let coarse = grid_open(book, row + BOOK_WIDTH, coarse_grid, position, normal);
        far = select(1.0, mix(1.0, coarse.x, fade), coarse.y >= 0.0);
    }
    if fine.y < 0.0 {
        return far;
    }
    return mix(far, fine.x, fine.y / GROUND_HANDOVER);
}

fn grid_open(book: Book, row: u32, grid: vec4<f32>, position: vec3<f32>, normal: vec3<f32>) -> vec2<f32> {
    let inside = (position.xz - grid.xy) / max(grid.z, 1.0);
    let column = floor(inside);
    if any(column < vec2(0.0)) || any(column >= vec2(f32(GROUND_SIDE))) {
        return vec2(1.0, -1.0);
    }
    let edge = min(min(inside.x, inside.y), f32(GROUND_SIDE) - max(inside.x, inside.y));
    if grid.w <= 0.0 {
        return vec2(1.0, edge);
    }
    let near = book_texel(book, row + 1u + u32(column.y) * GROUND_SIDE + u32(column.x));
    var open = 1.0;
    for (var slot = 0u; slot < 4u; slot++) {
        let held = u32(near[slot]);
        if held == 0u {
            break;
        }
        let at = row + 1u + GROUND_SIDE * GROUND_SIDE + (held - 1u) * 2u;
        let low = book_texel(book, at);
        let along = book_texel(book, at + 1u).xyz - low.xyz;
        let to = low.xyz + along * saturate(dot(position - low.xyz, along) / max(dot(along, along), 1e-4)) - position;
        let far = max(length(to), low.w);
        let wide = low.w / far;
        open *= 1.0 - wide * wide * saturate(dot(normal, to) / far) * saturate(GROUND_REACH - far / low.w);
    }
    return vec2(max(open, 1.0 - GROUND_MOST), edge);
}

fn lamp_shown(color: vec3<f32>, x2: f32, cone: f32) -> vec3<f32> {
    let x = sqrt(x2);
    return sqrt(color) * ((1.0 - x2 * (3.0 - 2.0 * x)) * cone);
}

fn add_lamp(sum: ptr<function, LampLight>, shown: vec3<f32>, color: vec3<f32>, x2: f32, cone: f32, shadow: f32, facing: f32, to_lamp: vec3<f32>, normal: vec3<f32>, eye: vec3<f32>, gloss: bool, a: f32) {
    (*sum).diffuse += shown * (shadow * facing);
    if gloss {
        let window = saturate(1.0 - x2 * x2);
        let attenuation = LAMP_ENERGY * LAMP_CORE_SQUARED / max(x2, LAMP_CORE_SQUARED) * window * window * cone;
        let highlight = color * (attenuation * shadow) * ggx(normal, to_lamp, eye, facing, a, vec3(1.0));
        let halfway = to_lamp + eye;
        (*sum).specular += highlight;
        (*sum).specular_edge += highlight * fresnel_five(saturate(dot(eye, halfway * inverseSqrt(max(dot(halfway, halfway), 1e-12)))));
    }
}
