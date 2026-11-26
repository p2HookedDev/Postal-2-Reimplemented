// Each surface patch's lamp visibility, cached once a map from the lamps' plain shadow tiles: a block of texels for
// every four lamps the patch lists, one lamp a channel.

import package::p2_render::lamp_shadow::{ATLAS_UNITS, BOOK_HEADER, BOOK_RECORD, Book, book_texel, pass_atlas, pass_book, plain_tile, shadow_tilt, tile_shadow};
import package::p2_render::sun_shadow::world_lookup;

// Matches `PatchJob` in `lamps/visibility.rs`. Block texel (i, j) is the point `origin + i * u + j * v`.
struct PatchJob {
    origin: vec3<f32>,
    groups: u32,
    u: vec3<f32>,
    list: u32,
    v: vec3<f32>,
    size: u32,
    normal: vec3<f32>,
    atlas: u32,
}

@group(1) @binding(0) var<storage, read> jobs: array<PatchJob>;
@group(1) @binding(1) var visibility: texture_storage_2d<rgba8unorm, write>;

// A block is this many groups a side. Matches `lamps/visibility.rs`.
const GROUP_SIDE: u32 = 8u;
const BLOCK_TILES: u32 = 3u;
const MOST_GROUPS: u32 = 65535u;

@compute @workgroup_size(GROUP_SIDE, GROUP_SIDE, 1)
fn cache(@builtin(workgroup_id) w: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let job = jobs[w.y + w.z * MOST_GROUPS];
    let texel = vec2(w.x % BLOCK_TILES, w.x / BLOCK_TILES) * GROUP_SIDE + l.xy;
    if any(texel >= vec2(job.size)) {
        return;
    }
    let book = Book(0u, 0u, 0u, textureDimensions(pass_book), f32(textureDimensions(pass_atlas).x), f32(textureDimensions(pass_atlas).y));
    let unit = book.side / ATLAS_UNITS;
    let at = vec2(job.atlas & 0xffffu, job.atlas >> 16u) + texel;
    for (var g = 0u; g < job.groups; g++) {
        let four = book_texel(book, job.list + g);
        var shown = vec4(1.0);
        for (var c = 0u; c < 4u; c++) {
            shown[c] = lamp_visibility(book, four[c], job, vec2<f32>(texel), unit);
        }
        textureStore(visibility, at + vec2(g * job.size, 0u), shown);
    }
}

// The mean of four points inside the texel; 1 for an empty slot, a lamp with no plain tile or a point past its
// reach. The normal is turned to the lamp's side, so a two-sided surface moves its points toward the lamp.
fn lamp_visibility(book: Book, slot: f32, job: PatchJob, texel: vec2<f32>, unit: f32) -> f32 {
    if slot < 0.0 {
        return 1.0;
    }
    let record = BOOK_HEADER + u32(slot) * BOOK_RECORD;
    let tile = plain_tile(u32(book_texel(book, record + 1u).w), unit);
    if tile.z <= 0.0 {
        return 1.0;
    }
    let place = book_texel(book, record);
    let radius = abs(place.w);
    var seen = 0.0;
    for (var quarter = 0u; quarter < 4u; quarter++) {
        let by = texel + vec2(f32(quarter % 2u), f32(quarter / 2u)) * 0.5 - 0.25;
        let spot = job.origin + job.u * by.x + job.v * by.y;
        let to_light = place.xyz - spot;
        let distance_to = sqrt(max(dot(to_light, to_light), 1e-4));
        if distance_to >= radius {
            seen += 1.0;
            continue;
        }
        let to_lamp = to_light / distance_to;
        let normal = select(-job.normal, job.normal, dot(job.normal, to_lamp) >= 0.0);
        seen += tile_shadow(book, tile.xy, tile.z, 0u, true, spot, normal, place.xyz, radius, distance_to, shadow_tilt(normal, to_lamp), book.side, world_lookup());
    }
    return seen * 0.25;
}
