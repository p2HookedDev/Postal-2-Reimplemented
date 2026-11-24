struct Chunk {
    center: vec3<f32>,
    bucket: u32,
    half_size: vec3<f32>,
    instance: u32,
    first_index: u32,
    index_count: u32,
    base_vertex: i32,
    flags: u32,
    // One bit per zone the chunk is in, read with `ZONED`.
    zones: array<u32, 8>,
}

struct DrawArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
}

struct CullView {
    planes: array<vec4<f32>, 6>,
    first_args: u32,
    first_count: u32,
    casters_only: u32,
    unused: u32,
    // All set for a shadow view.
    seen_zones: array<vec4<u32>, 2>,
}

const CASTS: u32 = 1u;
const ZONED: u32 = 2u;

@group(0) @binding(0) var<storage> chunks: array<Chunk>;
@group(0) @binding(1) var<storage> bucket_firsts: array<u32>;
@group(0) @binding(2) var<storage, read_write> args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(4) var<uniform> view: CullView;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= arrayLength(&chunks) {
        return;
    }
    let chunk = chunks[id.x];
    if view.casters_only != 0u && (chunk.flags & CASTS) == 0u {
        return;
    }
    if (chunk.flags & ZONED) != 0u {
        var seen = 0u;
        for (var word = 0u; word < 8u; word++) {
            seen |= chunk.zones[word] & view.seen_zones[word >> 2u][word & 3u];
        }
        if seen == 0u {
            return;
        }
    }
    for (var i = 0u; i < 6u; i++) {
        let plane = view.planes[i];
        if dot(plane.xyz, chunk.center) + dot(abs(plane.xyz), chunk.half_size) + plane.w < 0.0 {
            return;
        }
    }
    let slot = atomicAdd(&counts[view.first_count + chunk.bucket], 1u);
    args[view.first_args + bucket_firsts[chunk.bucket] + slot] = DrawArgs(chunk.index_count, 1u, chunk.first_index, chunk.base_vertex, chunk.instance);
}
