// Lamp and sun shadows for each world camera pixel, cast after its depth prepass with the same code the lit
// surfaces run, which then read these in place of their own for the lamps held here and the sun.

import bevy_pbr::render::mesh_view_bindings::view;
import package::p2_render::lamp_shadow::{
    ATLAS_UNITS, Book, COLUMN_UNREAD, LAMP_SMALLEST, SCREEN_CHECK_BYTE, SCREEN_LIST_LAMPS, SCREEN_SLOTS, SCREEN_SUN_FROM, SCREEN_SUN_PIXELS,
    book_texel, lamp_at, lamp_list, lamp_shadows, pass_atlas, pass_book, tile_column,
};
import package::p2_render::modern_base::LUMINANCE;
import package::p2_render::sun_levels::LevelSource;
import package::p2_render::sun_shadow::{sun_held, sun_index, world_lookup};

@group(1) @binding(0) var depth: texture_depth_2d;
@group(1) @binding(1) var normals: texture_2d<f32>;
@group(1) @binding(6) var mask: texture_storage_2d<rgba32float, write>;

// Matches `lamps/shadow_mask.rs`.
const GROUP_SIDE: u32 = 8u;
// The prepass holds the plain normal, and relief tilts a lit surface's normal up to this tangent away,
// so a lamp that far past the plain normal's side may still light the surface.
const FACING_SLACK: f32 = 0.45;
const UNRANKED: f32 = -1e30;

var<workgroup> suns: array<array<u32, GROUP_SIDE>, GROUP_SIDE>;

@compute @workgroup_size(GROUP_SIDE, GROUP_SIDE, 1)
fn shade(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    let size = textureDimensions(depth);
    // The header, which `shadow_mask.rs` marks unwritten before the pass: 1, then two columns of the view's matrix,
    // so another view keeps its own shadows.
    if all(id.xy == vec2(0u)) {
        textureStore(mask, vec2(0u, size.y), vec4(1.0, 0.0, 0.0, 0.0));
        textureStore(mask, vec2(1u, size.y), view.clip_from_world[0]);
        textureStore(mask, vec2(2u, size.y), view.clip_from_world[3]);
    }
    let inside = all(id.xy < size);
    var sun = 0u;
    // The lamps' texel is stored before the sun's work starts, so the two never hold their registers at once.
    if inside {
        textureStore(mask, id.xy, screen_texel(id.xy));
        sun = sun_bytes(id.xy);
    }
    suns[local.y][local.x] = sun;
    workgroupBarrier();
    if inside && local.y % SCREEN_SUN_PIXELS == 0u {
        let rows = vec4(suns[local.y][local.x], suns[local.y + 1u][local.x], suns[local.y + 2u][local.x], suns[local.y + 3u][local.x]);
        textureStore(mask, vec2(id.x, size.y + SCREEN_SUN_FROM + id.y / SCREEN_SUN_PIXELS), vec4<f32>(rows));
    }
}

fn screen_texel(pixel: vec2<u32>) -> vec4<f32> {
    let z = textureLoad(depth, pixel, 0);
    let book = Book(0u, 0u, 0u, 0u, textureDimensions(pass_book), f32(textureDimensions(pass_atlas).x));
    let unit = book.side / ATLAS_UNITS;
    if z <= 0.0 || unit < 1.0 {
        return vec4(0.0);
    }
    let position = pixel_position(pixel, z);
    let normal = pixel_normal(pixel);
    let list = lamp_list(book, position);
    var column = COLUMN_UNREAD;
    var movers_near = true;
    if book.size.y >= 8u {
        column = tile_column(book, position);
        movers_near = column == 0u || book_texel(book, column).x != 0.0;
    }
    // The lamps that light the pixel most past the static world take the slots; one only the slack reaches
    // ranks below every faced one. A lamp left out gives no light, so a walled one never needs a slot.
    // The bytes follow the list's order, as the lit surfaces count them.
    var best: array<f32, SCREEN_SLOTS>;
    var which: array<u32, SCREEN_SLOTS>;
    var shadows: array<vec2<f32>, SCREEN_SLOTS>;
    var unheld_best = UNRANKED;
    for (var s = 0u; s < SCREEN_SLOTS; s++) {
        best[s] = UNRANKED;
    }
    for (var i = 0u; i < min(list.y, SCREEN_LIST_LAMPS); i++) {
        let lamp = lamp_at(book, list, i, position, normal, view.world_position, FACING_SLACK);
        if lamp.share < LAMP_SMALLEST {
            continue;
        }
        var through = lamp_shadows(book, lamp, position, normal, book.side, unit, &column, movers_near, world_lookup());
        if through.x <= 0.0 {
            continue;
        }
        var score = select(-1.0 / (lamp.share * through.x), dot(lamp.shown, LUMINANCE) * lamp.facing * through.x, lamp.facing > 0.0);
        var index = i;
        for (var s = 0u; s < SCREEN_SLOTS; s++) {
            if score > best[s] {
                let kept = best[s];
                let kept_index = which[s];
                let kept_through = shadows[s];
                best[s] = score;
                which[s] = index;
                shadows[s] = through;
                score = kept;
                index = kept_index;
                through = kept_through;
            }
        }
        unheld_best = max(unheld_best, score);
    }
    // The last slot fades out as the best lamp left out nears it, so a lamp's light never jumps where the two swap.
    let last = SCREEN_SLOTS - 1u;
    if unheld_best > 0.0 && best[last] > 0.0 {
        shadows[last] *= 1.0 - unheld_best / best[last];
    }
    var held = 0u;
    for (var s = 0u; s < SCREEN_SLOTS; s++) {
        if best[s] > UNRANKED {
            held |= 1u << which[s];
        }
    }
    var bytes = vec3(0u);
    for (var s = 0u; s < SCREEN_SLOTS; s++) {
        if best[s] > UNRANKED {
            let slot = countOneBits(held & ((1u << which[s]) - 1u));
            put_byte(&bytes, 2u * slot, shadow_byte(shadows[s].x));
            put_byte(&bytes, 2u * slot + 1u, shadow_byte(shadows[s].y));
        }
    }
    let check = (list.x + 1u) & 0xffffu;
    put_byte(&bytes, SCREEN_CHECK_BYTE, check >> 8u);
    return vec4(f32((check & 255u) | (held << 8u)), vec3<f32>(bytes));
}

fn shadow_byte(value: f32) -> u32 {
    return u32(round(saturate(value) * 255.0));
}

fn put_byte(bytes: ptr<function, vec3<u32>>, at: u32, value: u32) {
    (*bytes)[at / 3u] |= value << (8u * (at % 3u));
}

fn sun_bytes(pixel: vec2<u32>) -> u32 {
    let z = textureLoad(depth, pixel, 0);
    if z <= 0.0 {
        return 0u;
    }
    let held = sun_held(LevelSource(0u, 0u), sun_index(), vec4(pixel_position(pixel, z), 1.0), pixel_normal(pixel), vec2<f32>(pixel) + 0.5, world_lookup());
    let bytes = vec3<u32>(round(saturate(held) * 255.0));
    return bytes.x | (bytes.y << 8u) | (bytes.z << 16u);
}

fn pixel_position(pixel: vec2<u32>, z: f32) -> vec3<f32> {
    let uv = (vec2<f32>(pixel) + 0.5 - view.viewport.xy) / view.viewport.zw;
    let seen = view.world_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return seen.xyz / seen.w;
}

fn pixel_normal(pixel: vec2<u32>) -> vec3<f32> {
    return normalize(textureLoad(normals, pixel, 0).xyz * 2.0 - 1.0);
}
