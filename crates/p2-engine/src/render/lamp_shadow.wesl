import package::p2_engine::modern_base::{DIELECTRIC_F0, LUMINANCE, LampLight, ggx, to_eye};
import package::p2_engine::sun_shadow::{SHADOW_BIAS, SHADOW_NORMAL_OFFSET, SOFT_EDGE_UNITS, SUN_FACING_FADE};
import package::p2_engine::probes::read_probes;
import bevy_pbr::render::{
    forward_io::VertexOutput,
    clustered_forward as clustering,
    mesh_view_bindings::view,
    pbr_bindings,
};
import bevy_pbr::render::mesh_view_bindings::clustered_lights;
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};

fn no_lamps() -> LampLight {
    return LampLight(vec3(0.0), vec3(0.0), vec3(0.0), vec4(-1.0, 0.0, 0.0, 0.0), 1.0);
}

// No retail light holds a shadow depth bias this large: a Bevy light that does is one of ours and takes no part in the
// retail dynamic lights.
const LAMP_MARK: f32 = 1048576.0;
// The atlas side in units. Matches `lamps/atlas.rs`.
const ATLAS_UNITS: f32 = 128.0;
const ATLAS_GUARD: f32 = 3.0;
const ATLAS_UNMAPPED: u32 = 7u;
const LAMP_CORE_SQUARED: f32 = 0.09;
const LAMP_ENERGY: f32 = 1.4485;
// In the lamp's radii from the eye: it fades out over this range, and past it `lamps.rs` hides it.
const LAMP_FADE_FROM: f32 = 21.0;
const LAMP_FADE_TO: f32 = 30.0;
const LAMP_SMALLEST: f32 = 0.01;
// A lamp's source is this share of its radius.
const LAMP_SOURCE_SHARE: f32 = 0.08;
// The book of lamps rides in the material's anisotropy texture, four floats a texel: a header (the grid's corner and
// cell side; its sides and where the cells start), six texels a lamp, a texel a cell (where its list starts and how
// many lamps it holds) and the lists of lamps whose light gets into the cell, four a texel. Matches `lamps/cells.rs`.
const BOOK_WIDTH: u32 = 2048u;
const BOOK_HEADER: u32 = 2u;
const BOOK_RECORD: u32 = 6u;
// A detail face's corner is counted in these texels. Matches `lamps/faces.rs`.
const DETAIL_UNIT: f32 = 64.0;
// A texel holds a distance in a 16 bit float: its steps are this share of the distance at most.
const DEPTH_STEP: f32 = 0.001;

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

fn book_rows(in: VertexOutput) -> u32 {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return textureDimensions(bindless_textures_2d[material_indices[slot].anisotropy_texture]).y;
}
@elif(PBR_ANISOTROPY_TEXTURE_SUPPORTED) {
    return textureDimensions(pbr_bindings::anisotropy_texture).y;
}
@else {
    return 1u;
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

// In display light on the surface: a lamp giving less adds no highlight. Its shadow keeps its soft edge.
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

// The four texels whose corners meet at `corner + 1`, in one read: from `corner`, the texel under it, the one under and
// right of it, the one right of it, and itself. `side` is the atlas's in texels.
fn atlas_quad(in: VertexOutput, side: f32, corner: vec2<f32>) -> vec4<f32> {
    let at = (corner + 1.0) / side;
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    return textureGather(0, bindless_textures_2d[material_indices[slot].occlusion_texture], bindless_samplers_filtering[material_indices[slot].occlusion_sampler], at);
}
@else {
    return textureGather(0, pbr_bindings::occlusion_texture, pbr_bindings::occlusion_sampler, at);
}
}

// The lamp's edge is four blended reads on the corners of a square fixed to the texels. The square is the sun's edge
// wide in world units where the tile allows, and between these in texels: the four reads then cover four texels by
// four at most, inside a face's guard band.
const LAMP_EDGE_LEAST_TEXELS: f32 = 1.0;
const LAMP_EDGE_MOST_TEXELS: f32 = 2.0;

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

// `low` the face's corner and `size` its side in texels, `at` a place in the atlas. The light blended from the four
// texels around `at`, and how many of the four are lit.
fn atlas_blended(in: VertexOutput, side: f32, low: vec2<f32>, size: f32, at: vec2<f32>, depth: f32) -> vec2<f32> {
    let spot = clamp(at, low + 1.0, low + size - 1.0) - 0.5;
    let base = floor(spot);
    let blend = spot - base;
    let lit = step(vec4(depth), atlas_quad(in, side, base));
    return vec2(mix(mix(lit.w, lit.z, blend.x), mix(lit.x, lit.y, blend.x), blend.y), lit.x + lit.y + lit.z + lit.w);
}

// What a tap `by` texels off the point is held against: how far the point's own surface lies along that tap's ray, so
// a tilted surface does not shade itself, never more than a texel of depth a texel off. Over the radius, as a texel is.
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

// The face of a lamp's cube a point seen from the lamp lies in.
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

// `from_lamp` the point seen from the lamp; a texel holds a distance over the radius, as `lamp_atlas.wesl` draws it.
// `low` the face's corner and `size` its side, in texels; `side` the atlas's. A point just past the face's side reads
// its guard band.
fn face_shadow(in: VertexOutput, low: vec2<f32>, size: f32, face: u32, from_lamp: vec3<f32>, normal: vec3<f32>, radius: f32, texel_world: f32, margin: f32, side: f32) -> f32 {
    return square_shadow(in, low, size, 0.5 * size - ATLAS_GUARD, face_view(from_lamp, face), face_view(normal, face), radius, texel_world, margin, side);
}

// The shadow read from one square of the atlas. `seen` and `tilted` are the point and its normal in the axes of the
// view the square was drawn through: x right, y up, z out of the lamp; `span` is how many texels a slope of 1 covers.
fn square_shadow(in: VertexOutput, low: vec2<f32>, size: f32, span: f32, seen: vec3<f32>, tilted: vec3<f32>, radius: f32, texel_world: f32, margin: f32, side: f32) -> f32 {
    let at = low + 0.5 * size + vec2(seen.x, -seen.y) / max(seen.z, 1e-4) * span;
    let half = 0.5 * clamp(SOFT_EDGE_UNITS / texel_world, LAMP_EDGE_LEAST_TEXELS, LAMP_EDGE_MOST_TEXELS);
    let reach = length(seen);
    let plane = dot(tilted, seen);
    var lit = vec2(0.0);
    for (var corner = 0; corner < 4; corner++) {
        // One diagonal first: where its eight texels agree, no edge is near and the other two reads are saved.
        if corner == 2 && (lit.y < 0.5 || lit.y > 7.5) {
            return lit.x * 0.5;
        }
        let by = vec2(select(-half, half, corner == 1 || corner == 2), select(-half, half, corner % 2 == 1));
        lit += atlas_blended(in, side, low, size, at + by, receiver_depth(seen, tilted, reach, plane, by, span, texel_world, margin, radius));
    }
    return lit.x * 0.25;
}

// A lamp's shadow from one face of `size` texels a side. `plain` is the corner of the lamp's plain tile, which holds
// its six faces side by side, and the face is the one the looked-up point lies in; a detail face has its own `corner`.
fn tile_shadow(in: VertexOutput, corner: vec2<f32>, size: f32, face: u32, plain: bool, position: vec3<f32>, normal: vec3<f32>, lamp_position: vec3<f32>, lamp_radius: f32, distance_to: f32, tilt: f32, side: f32) -> f32 {
    let texel_world = 2.0 * distance_to / size;
    let from_lamp = position + normal * (texel_world * SHADOW_NORMAL_OFFSET) - lamp_position;
    let margin = texel_world * SHADOW_BIAS * (1.0 + 0.5 * tilt) + distance_to * DEPTH_STEP;
    if !plain {
        return face_shadow(in, corner, size, face, from_lamp, normal, lamp_radius, texel_world, margin, side);
    }
    let own = cube_face(from_lamp);
    return face_shadow(in, corner + vec2(f32(own % 3u), f32(own / 3u)) * size, size, own, from_lamp, normal, lamp_radius, texel_world, margin, side);
}

// The detail square of one face of a lamp, from the last two texels of its record: its corner and its side in texels,
// the side 0 without one.
fn detail_face(in: VertexOutput, record: u32, face: u32) -> vec3<f32> {
    let packed = u32(book_texel(in, record + 3u + face / 4u)[face % 4u]);
    if packed == 0u {
        return vec3(0.0);
    }
    let at = packed - 1u;
    return vec3(vec2(f32(at & 255u), f32((at >> 8u) & 255u)) * DETAIL_UNIT, DETAIL_UNIT * f32(1u << (at >> 16u)));
}

// What moves casts into tiles of its own: a lamp's record ends with where its tiles start in the book's third row from
// the end and how many they are, two texels a tile: the tile's axis out of the lamp and the tangent of the angle it
// looks to either side; its square's corner and side in texels and how far its shadow has faded in. Matches
// `lamps/movers.rs`.
const TILE_GRID_SIDE: u32 = 64u;

// The five rows before the tiles hold a grid of columns around the eye: its low corner in x and z, a column's side and
// 1; then two texels a column naming up to eight tiles that may shade it from 1, -1 first when more do. Where the
// column under `position` starts in the book, 0 outside the grid. Matches `lamps/tile_grid.rs`.
fn tile_column(in: VertexOutput, position: vec3<f32>) -> u32 {
    let row = (book_rows(in) - 8u) * BOOK_WIDTH;
    let grid = book_texel(in, row);
    let column = floor((position.xz - grid.xy) / max(grid.z, 1.0));
    if grid.w <= 0.0 || any(column < vec2(0.0)) || any(column >= vec2(f32(TILE_GRID_SIDE))) {
        return 0u;
    }
    return row + 1u + (u32(column.y) * TILE_GRID_SIDE + u32(column.x)) * 2u;
}

// `lit` darkened by the tile whose two texels start at `at`.
fn mover_tile_shadow(in: VertexOutput, at: u32, lit: f32, from_lamp: vec3<f32>, normal: vec3<f32>, lamp_radius: f32, distance_to: f32, tilt: f32, side: f32) -> f32 {
    let aim = book_texel(in, at);
    let axis = aim.xyz;
    let along = dot(from_lamp, axis);
    let reach = along * aim.w * 1.4143;
    if along <= 0.0 || dot(from_lamp, from_lamp) - along * along > reach * reach {
        return lit;
    }
    let square = book_texel(in, at + 1u);
    if square.w <= 0.0 {
        return lit;
    }
    let helper = select(vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), abs(axis.y) > 0.9);
    let right = normalize(cross(helper, axis));
    let up = cross(axis, right);
    let size = square.z;
    let texel_world = 2.0 * aim.w * distance_to / size;
    let moved = from_lamp + normal * (texel_world * SHADOW_NORMAL_OFFSET);
    let seen = vec3(dot(moved, right), dot(moved, up), dot(moved, axis));
    let span = (0.5 * size - ATLAS_GUARD) / aim.w;
    if seen.z <= 0.0 || any(abs(seen.xy) * span > vec2(seen.z * (0.5 * size - 1.0))) {
        return lit;
    }
    let tilted = vec3(dot(normal, right), dot(normal, up), dot(normal, axis));
    let margin = texel_world * SHADOW_BIAS * (1.0 + 0.5 * tilt) + distance_to * DEPTH_STEP;
    let shade = square_shadow(in, square.xy, size, span, seen, tilted, lamp_radius, texel_world, margin, side);
    return min(lit, mix(1.0, shade, saturate(square.w)));
}

// `held` the lamp's tiles' start and count; `named` and `named_more` the point's column. Below 0 first, the point walks
// all the lamp's tiles.
fn mover_shadow(in: VertexOutput, held: vec4<f32>, named: vec4<f32>, named_more: vec4<f32>, position: vec3<f32>, normal: vec3<f32>, lamp_position: vec3<f32>, lamp_radius: f32, distance_to: f32, tilt: f32, side: f32) -> f32 {
    let start = u32(held.x);
    let count = u32(held.y);
    let row = (book_rows(in) - 3u) * BOOK_WIDTH;
    let from_lamp = position - lamp_position;
    var lit = 1.0;
    if named.x < 0.0 {
        for (var tile = 0u; tile < count && lit > 0.0; tile++) {
            lit = mover_tile_shadow(in, row + (start + tile) * 2u, lit, from_lamp, normal, lamp_radius, distance_to, tilt, side);
        }
        return lit;
    }
    for (var slot = 0u; slot < 8u && lit > 0.0; slot++) {
        let name = u32(select(named[slot % 4u], named_more[slot % 4u], slot >= 4u));
        if name == 0u {
            break;
        }
        let tile = name - 1u;
        if tile >= start && tile < start + count {
            lit = mover_tile_shadow(in, row + tile * 2u, lit, from_lamp, normal, lamp_radius, distance_to, tilt, side);
        }
    }
    return lit;
}

// Light from the placed lamps and the probes, before the surface's colour: terrain's layers share one sum.
fn lamp_light(in: VertexOutput, position: vec3<f32>, normal: vec3<f32>, frag_coord: vec2<f32>, ranges: clustering::ClusterableObjectIndexRanges, gloss: bool, surface: vec3<f32>) -> LampLight {
    var sum = no_lamps();
    // Special-lit surfaces take only special lights, and no lamp is one.
    if surface.z > 0.5 {
        return sum;
    }
    let side = atlas_side(in);
    let unit = side / ATLAS_UNITS;
    let rough = surface.x;
    let eye = to_eye(position);
    let list = lamp_list(in, position);
    var four = vec4(0.0);
    // The lamps' light past the static world, in display light as it is summed; the same lamp by lamp in linear light;
    // and what of that the movers' shadows let through. A lamp a wall hides counts in none: a cell's list may drop it.
    var unshaded = vec3(0.0);
    var given = vec3(0.0);
    var reaching = vec3(0.0);
    var column_read = false;
    var named = vec4(-1.0);
    var named_more = vec4(0.0);
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
        var walled = shadow;
        if unit >= 1.0 {
            let tilt = min(sqrt(saturate(1.0 - facing * facing)) / max(facing, 0.1), 4.0);
            // A face that would show its texels has a finer square of its own.
            let face = cube_face(position - lamp_position);
            let detail = detail_face(in, record, face);
            let packed = u32(look.w);
            let level = (packed >> 16u) & 15u;
            if detail.z > 0.0 {
                shadow *= tile_shadow(in, detail.xy, detail.z, face, false, position, normal, lamp_position, lamp_radius, distance_to, tilt, side);
            } else if level != ATLAS_UNMAPPED {
                let corner = vec2(f32(packed & 255u), f32((packed >> 8u) & 255u)) * unit;
                shadow *= tile_shadow(in, corner, unit * f32(1u << level), face, true, position, normal, lamp_position, lamp_radius, distance_to, tilt, side);
            }
            walled = shadow;
            if shadow > 0.0 {
                let held = book_texel(in, record + 5u);
                if held.y > 0.0 {
                    if !column_read {
                        column_read = true;
                        let column = tile_column(in, position);
                        if column > 0u {
                            named = book_texel(in, column);
                            named_more = book_texel(in, column + 1u);
                        }
                    }
                    shadow = min(shadow, mover_shadow(in, held, named, named_more, position, normal, lamp_position, lamp_radius, distance_to, tilt, side));
                }
            }
        }
        let lamp_gives = shown * (facing * walled);
        unshaded += lamp_gives;
        given += lamp_gives * lamp_gives;
        reaching += lamp_gives * lamp_gives * (shadow / max(walled, 1e-4));
        if shadow <= 0.0 {
            continue;
        }
        let a = saturate(rough * rough + lamp_radius * LAMP_SOURCE_SHARE / (2.0 * distance_to));
        add_lamp(&sum, shown, color, x2, cone, shadow, facing, to_lamp, normal, eye, gloss && share >= LAMP_GLOSS_FROM, a);
    }
    // A mover's shadow takes its lamp's share of the linear light and no more: summed in display light, one lamp of
    // four alike in shadow would take 44 % of the light for its 25 %.
    sum.diffuse = unshaded * sqrt(reaching / max(given, vec3(1e-8)));
    sum.grounded = grounded(in, position, normal);
    read_probes(in, position, normal, &sum);
    return sum;
}

// A column of the grid is this many to a side; a capsule darkens out to this many of its radii, and never by more than
// the most. Matches `lamps/grounding.rs`.
const GROUND_SIDE: u32 = 32u;
const GROUND_REACH: f32 = 3.0;
const GROUND_MOST: f32 = 0.6;

// What moves near the eye stands in the light from all around as a capsule each. A row of the book holds a grid of
// columns around the eye (its low corner in x and z, a column's side, how many capsules), up to four capsules a
// column, and the capsules: one end and the radius, then the other end. The book ends with two such rows: a fine grid
// near the eye, and a coarse one for a point past it.
fn grounded(in: VertexOutput, position: vec3<f32>, normal: vec3<f32>) -> f32 {
    if !has_book(in) {
        return 1.0;
    }
    var row = (book_rows(in) - 2u) * BOOK_WIDTH;
    var grid = book_texel(in, row);
    var column = floor((position.xz - grid.xy) / max(grid.z, 1.0));
    var fade = 1.0;
    if any(column < vec2(0.0)) || any(column >= vec2(f32(GROUND_SIDE))) {
        row += BOOK_WIDTH;
        grid = book_texel(in, row);
        column = floor((position.xz - grid.xy) / max(grid.z, 1.0));
        let rim = 0.5 * f32(GROUND_SIDE) * grid.z;
        fade = 1.0 - smoothstep(rim - 3.0 * grid.z, rim - grid.z, distance(position.xz, view.world_position.xz));
    }
    if grid.w <= 0.0 || fade <= 0.0 || any(column < vec2(0.0)) || any(column >= vec2(f32(GROUND_SIDE))) {
        return 1.0;
    }
    let near = book_texel(in, row + 1u + u32(column.y) * GROUND_SIDE + u32(column.x));
    var open = 1.0;
    for (var slot = 0u; slot < 4u; slot++) {
        let held = u32(near[slot]);
        if held == 0u {
            break;
        }
        let at = row + 1u + GROUND_SIDE * GROUND_SIDE + (held - 1u) * 2u;
        let low = book_texel(in, at);
        let along = book_texel(in, at + 1u).xyz - low.xyz;
        let to = low.xyz + along * saturate(dot(position - low.xyz, along) / max(dot(along, along), 1e-4)) - position;
        // A sphere at the capsule's nearest point: the share of the sky over the surface it covers.
        let far = max(length(to), low.w);
        let wide = low.w / far;
        open *= 1.0 - wide * wide * saturate(dot(normal, to) / far) * saturate(GROUND_REACH - far / low.w);
    }
    return mix(1.0, max(open, 1.0 - GROUND_MOST), fade);
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
