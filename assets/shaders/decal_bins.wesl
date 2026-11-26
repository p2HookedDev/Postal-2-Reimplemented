import bevy_render::view::View;

struct Decal {
    local_from_world: mat4x4<f32>,
    image_indices: vec4<i32>,
    world_position: vec3<f32>,
    bounding_sphere_radius: f32,
    tag: u32,
    pad_a: u32,
    pad_b: u32,
    pad_c: u32,
}

struct Bins {
    grid: vec4<u32>,
    lists: vec4<u32>,
}

@group(0) @binding(0) var depth: texture_depth_2d;
@group(0) @binding(1) var<uniform> view: View;
@group(0) @binding(2) var<storage, read> decals: array<Decal>;
@group(0) @binding(3) var book: texture_storage_2d<rgba32float, write>;
@group(0) @binding(4) var<uniform> bins: Bins;

// Matches `lamps/cells.rs` and `decal_bins.rs`.
const BOOK_WIDTH: u32 = 2048u;
const LIST_TEXELS: u32 = 16u;
const HEAD_TEXELS: u32 = 4u;
const MOST_LISTED: u32 = 63u;
const MASK_WORDS: u32 = 32u;
const LISTED_SPAN: u32 = 64u;
// Matches `projector.wesl`: a decal of our liquid has these tag bits.
const PROJECTOR: u32 = 0x80000000u;
const FB_LIQUID: u32 = 7u;
const BOX_SLACK: f32 = 0.001;

var<workgroup> nearest: atomic<u32>;
var<workgroup> farthest: atomic<u32>;
var<workgroup> corners: array<vec3<f32>, 8>;
var<workgroup> mask: array<atomic<u32>, MASK_WORDS>;
var<workgroup> liquid_mask: array<atomic<u32>, MASK_WORDS>;
var<workgroup> list: array<u32, 64>;
var<workgroup> listed: u32;
var<workgroup> liquids_listed: u32;

fn book_at(texel: u32) -> vec2<u32> {
    return vec2(texel % BOOK_WIDTH, texel / BOOK_WIDTH);
}

fn meets(decal: u32) -> bool {
    let m = decals[decal].local_from_world;
    var low = vec3(1e30);
    var high = vec3(-1e30);
    for (var corner = 0u; corner < 8u; corner++) {
        let local = (m * vec4(corners[corner], 1.0)).xyz;
        low = min(low, local);
        high = max(high, local);
    }
    return all(high >= vec3(-0.5 - BOX_SLACK)) && all(low <= vec3(0.5 + BOX_SLACK));
}

@compute @workgroup_size(16, 16, 1)
fn bin(@builtin(workgroup_id) tile: vec3<u32>, @builtin(local_invocation_id) lane: vec3<u32>, @builtin(local_invocation_index) index: u32) {
    let side = bins.grid.z;
    if index == 0u {
        atomicStore(&nearest, 0u);
        atomicStore(&farthest, 0x7f800000u);
        listed = 0u;
        liquids_listed = 0u;
    }
    if index < MASK_WORDS {
        atomicStore(&mask[index], 0u);
        atomicStore(&liquid_mask[index], 0u);
    }
    workgroupBarrier();
    // Reverse depth: 0 is the sky, larger is nearer; a positive float orders as its bits do.
    let size = textureDimensions(depth);
    let step = side / 16u;
    for (var y = 0u; y < step; y++) {
        for (var x = 0u; x < step; x++) {
            let pixel = tile.xy * side + lane.xy * step + vec2(x, y);
            if all(pixel < size) {
                let z = textureLoad(depth, pixel, 0);
                if z > 0.0 {
                    atomicMax(&nearest, bitcast<u32>(z));
                    atomicMin(&farthest, bitcast<u32>(z));
                }
            }
        }
    }
    workgroupBarrier();
    let near_bits = atomicLoad(&nearest);
    if index < 8u && near_bits != 0u {
        let near = min(bitcast<f32>(near_bits) * 1.001, 1.0);
        let far = bitcast<f32>(atomicLoad(&farthest)) * 0.999;
        let edge = vec2<f32>(tile.xy * side + vec2(index & 1u, (index >> 1u) & 1u) * side);
        let uv = (edge - view.viewport.xy) / view.viewport.zw;
        let at = view.world_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, select(far, near, (index & 4u) != 0u), 1.0);
        corners[index] = at.xyz / at.w;
    }
    workgroupBarrier();
    if near_bits != 0u {
        for (var decal = index; decal < bins.grid.w; decal += 256u) {
            if meets(decal) {
                if (decals[decal].tag & (PROJECTOR | 0x7u)) == (PROJECTOR | FB_LIQUID) {
                    atomicOr(&liquid_mask[decal / 32u], 1u << (decal % 32u));
                } else {
                    atomicOr(&mask[decal / 32u], 1u << (decal % 32u));
                }
            }
        }
    }
    workgroupBarrier();
    if index == 0u {
        var count = 0u;
        var liquids = 0u;
        for (var family = 0u; family < 2u; family++) {
            for (var word = 0u; word < MASK_WORDS; word++) {
                var bits = select(atomicLoad(&mask[word]), atomicLoad(&liquid_mask[word]), family == 0u);
                while bits != 0u {
                    let bit = firstTrailingBit(bits);
                    bits &= bits - 1u;
                    if count < 64u {
                        list[count] = word * 32u + bit;
                    }
                    count++;
                }
            }
            if family == 0u {
                liquids = count;
            }
        }
        listed = count;
        liquids_listed = liquids;
    }
    workgroupBarrier();
    if all(tile.xy == vec2(0u)) && index == LIST_TEXELS {
        let head = bins.lists.x - HEAD_TEXELS;
        textureStore(book, book_at(head), vec4(f32(bins.grid.x), f32(side), f32(bins.grid.w), 1.0));
        textureStore(book, book_at(head + 1u), view.clip_from_world[0]);
        textureStore(book, book_at(head + 2u), view.clip_from_world[3]);
    }
    if index < LIST_TEXELS {
        let count = listed;
        var texel = vec4(0.0);
        for (var c = 0u; c < 4u; c++) {
            let slot = index * 4u + c;
            if slot == 0u {
                texel[c] = select(f32(count + liquids_listed * LISTED_SPAN), -1.0, count > MOST_LISTED);
            } else if slot - 1u < min(count, MOST_LISTED) {
                texel[c] = f32(list[slot - 1u]);
            }
        }
        let start = bins.lists.x + (tile.y * bins.grid.x + tile.x) * LIST_TEXELS;
        textureStore(book, book_at(start + index), texel);
    }
}
