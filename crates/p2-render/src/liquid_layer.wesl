// Liquid on the floors, in world tiles of 128 x 128 texels of 2 Unreal units (`liquids/layer.rs`). Virtual pipes
// between neighbouring texels move what stands above the film downhill; the scripts' puddles hold their film, drops
// add volume, and everything dries. `publish` writes each tile with a 2-texel apron into the atlas the world's
// surfaces read (`projector.wesl` `pool_at`).

const SIDE: i32 = 128;
const TEXELS: u32 = 16384u;
const APRON: i32 = 2;
const TEXEL_UU: f32 = 2.0;
const AREA: f32 = 4.0;
// A floor at or past this is a wall or not yet traced: liquid never goes there.
const WALL: f32 = 1e29;
// What `publish` writes as the floor where no liquid can be: past any fragment's height.
const NO_FLOOR: f32 = 60000.0;
const EMPTY: f32 = 1e-4;
// The blood share rides in the kind channel's fraction, scaled so 1 never carries into the next kind.
const SHARE_SCALE: f32 = 0.98;
const BLOOD: f32 = 3.0;
const PUKE: f32 = 4.0;
const BLOODY_PUKE: f32 = 5.0;
// Puke is a paste: it stands this deep before it flows.
const PASTE_FILM: f32 = 0.8;
// A neighbour whose floor lies this far below is over a lip: what flows there spills, and is counted for the runs down
// the face under it (`liquids/layer.rs` `lips`), per cell of 8 x 8 texels, in thousandths of a unit of volume.
const LEDGE: f32 = 6.0;
const SPILL_CELL: u32 = 8u;
const SPILL_CELLS: u32 = 256u;
const SPILL_SCALE: f32 = 1000.0;

// `LayerStep` in `liquids/layer_gpu.rs`.
struct Step {
    seconds: f32,
    film: f32,
    drying: f32,
    // Gravity times a pipe's section over its length: the flow a second gained per unit of height difference.
    pull: f32,
    damping: f32,
    pad: vec3<f32>,
}

// `LayerSlot` in `liquids/layer_gpu.rs`.
struct Slot {
    // The slot of each tile around this one, -1 where none, at (dy + 1) * 3 + dx + 1; this one at 4.
    around: array<i32, 9>,
    first_source: u32,
    sources: u32,
    // The slot's corner in the atlas, apron included.
    atlas_x: u32,
    atlas_y: u32,
}

// `LayerSource` in `liquids/layer_gpu.rs`: in the slot's texels the middle and the radius; `amount` the depth a hold
// keeps or the volume a drop adds.
struct Source {
    x: f32,
    y: f32,
    radius: f32,
    amount: f32,
    kind: f32,
    adds: u32,
    pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> step: Step;
@group(0) @binding(1) var<storage, read> slots: array<Slot>;
@group(0) @binding(2) var<storage, read> live: array<u32>;
@group(0) @binding(3) var<storage, read> sources: array<Source>;
// Depth, and the kind with the blood share in its fraction.
@group(0) @binding(4) var<storage, read> water_in: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> water_out: array<vec2<f32>>;
// Outflow a second to -x, +x, -y, +y, two halves a word.
@group(0) @binding(6) var<storage, read_write> flux: array<vec2<u32>>;
// In Bevy height (Unreal z from the world's origin), `WALL` and above where liquid cannot go.
@group(0) @binding(7) var<storage, read> floors: array<f32>;
@group(0) @binding(8) var atlas: texture_storage_2d<rgba16float, write>;
// Per slot and cell: the volume spilt over lips this frame, and the ways it went as bits.
@group(0) @binding(9) var<storage, read_write> spills: array<atomic<u32>>;

// A private var, as only memory may be indexed by a value known at run time.
var<private> WAYS: array<vec2<i32>, 4> = array<vec2<i32>, 4>(vec2(-1, 0), vec2(1, 0), vec2(0, -1), vec2(0, 1));

// The buffer index of texel (x, y) of `slot`, reaching into the tiles around it; -1 where no tile is.
fn texel_of(slot: u32, x: i32, y: i32) -> i32 {
    let dx = select(select(0, 1, x >= SIDE), -1, x < 0);
    let dy = select(select(0, 1, y >= SIDE), -1, y < 0);
    let other = slots[slot].around[(dy + 1) * 3 + dx + 1];
    if other < 0 || abs(x - dx * SIDE) >= SIDE || abs(y - dy * SIDE) >= SIDE {
        return -1;
    }
    return other * i32(TEXELS) + (y - dy * SIDE) * SIDE + x - dx * SIDE;
}

fn outflow(i: u32) -> vec4<f32> {
    let f = flux[i];
    return vec4(unpack2x16float(f.x), unpack2x16float(f.y));
}

fn kind_of(w: vec2<f32>) -> f32 {
    return floor(w.y);
}

fn share_of(w: vec2<f32>) -> f32 {
    return fract(w.y) / SHARE_SCALE;
}

fn film_of(kind: f32) -> f32 {
    return select(step.film, PASTE_FILM, kind == PUKE || kind == BLOODY_PUKE);
}

fn packed(depth: f32, kind: f32, share: f32) -> vec2<f32> {
    if depth <= EMPTY {
        return vec2(0.0);
    }
    return vec2(depth, kind + saturate(share) * SHARE_SCALE);
}

struct Here {
    slot: u32,
    x: i32,
    y: i32,
    index: u32,
}

fn here(id: vec3<u32>) -> Here {
    let slot = live[id.z];
    return Here(slot, i32(id.x), i32(id.y), slot * TEXELS + id.y * u32(SIDE) + id.x);
}

// The slot's sources on each of its texels: a hold raises the depth to its film, a drop adds its volume over its disc.
@compute @workgroup_size(8, 8, 1)
fn hold(@builtin(global_invocation_id) id: vec3<u32>) {
    let at = here(id);
    if floors[at.index] >= WALL {
        return;
    }
    let slot = slots[at.slot];
    var w = water_out[at.index];
    var depth = w.x;
    var kind = kind_of(w);
    var share = share_of(w);
    let middle = vec2(f32(at.x), f32(at.y)) + 0.5;
    for (var k = 0u; k < slot.sources; k++) {
        let source = sources[slot.first_source + k];
        let cover = saturate(source.radius - distance(middle, vec2(source.x, source.y)) + 0.5);
        if cover <= 0.0 {
            continue;
        }
        var added = 0.0;
        if source.adds != 0u {
            added = source.amount * cover / (3.14159265 * max(source.radius * source.radius, 1.0) * AREA);
        } else {
            added = max(source.amount * cover - depth, 0.0);
        }
        if added <= 0.0 {
            continue;
        }
        let blood = select(0.0, 1.0, source.kind == BLOOD);
        share = (share * depth + blood * added) / (depth + added);
        if added > depth {
            kind = source.kind;
        }
        depth += added;
    }
    water_out[at.index] = packed(depth, kind, share);
}

// Each texel's outflow to its four neighbours from the difference of the liquid's height, never more than stands
// above the film.
@compute @workgroup_size(8, 8, 1)
fn pipes(@builtin(global_invocation_id) id: vec3<u32>) {
    let at = here(id);
    let floor_here = floors[at.index];
    let depth = water_in[at.index].x;
    let film = film_of(kind_of(water_in[at.index]));
    if floor_here >= WALL || depth <= film {
        flux[at.index] = vec2(0u);
        return;
    }
    let height = floor_here + depth;
    let before = outflow(at.index);
    var out = vec4(0.0);
    for (var k = 0; k < 4; k++) {
        let j = texel_of(at.slot, at.x + WAYS[k].x, at.y + WAYS[k].y);
        if j < 0 || floors[j] >= WALL {
            continue;
        }
        let drop = height - floors[j] - water_in[j].x;
        out[k] = max(before[k] * step.damping + step.seconds * step.pull * drop, 0.0);
    }
    let leaving = (out.x + out.y + out.z + out.w) * step.seconds;
    let movable = (depth - film) * AREA;
    if leaving > movable {
        out *= movable / leaving;
    }
    flux[at.index] = vec2(pack2x16float(out.xy), pack2x16float(out.zw));
    let cell = (u32(at.y) / SPILL_CELL) * (u32(SIDE) / SPILL_CELL) + u32(at.x) / SPILL_CELL;
    let counted = (at.slot * SPILL_CELLS + cell) * 2u;
    for (var k = 0; k < 4; k++) {
        let j = texel_of(at.slot, at.x + WAYS[k].x, at.y + WAYS[k].y);
        if out[k] > 0.0 && j >= 0 && floors[j] < floor_here - LEDGE {
            atomicAdd(&spills[counted], u32(out[k] * step.seconds * SPILL_SCALE));
            atomicOr(&spills[counted + 1u], 1u << u32(k));
        }
    }
}

// The new depth from what flows in and out, the kind and blood share carried with the liquid, and drying.
@compute @workgroup_size(8, 8, 1)
fn level(@builtin(global_invocation_id) id: vec3<u32>) {
    let at = here(id);
    let w = water_in[at.index];
    let out = outflow(at.index);
    let kept = max(w.x - (out.x + out.y + out.z + out.w) * step.seconds / AREA, 0.0);
    var depth = kept;
    var blood = kept * share_of(w);
    var kind = kind_of(w);
    var most = kept;
    for (var k = 0; k < 4; k++) {
        let j = texel_of(at.slot, at.x + WAYS[k].x, at.y + WAYS[k].y);
        if j < 0 {
            continue;
        }
        // The neighbour's pipe back to this texel is the opposite way.
        let came = outflow(u32(j))[k ^ 1] * step.seconds / AREA;
        if came <= 0.0 {
            continue;
        }
        let other = water_in[j];
        depth += came;
        blood += came * share_of(other);
        if came > most {
            most = came;
            kind = kind_of(other);
        }
    }
    let share = blood / max(depth, EMPTY);
    depth = max(depth - step.drying * step.seconds, 0.0);
    water_out[at.index] = packed(depth, kind, share);
}

// The atlas texel at (x, y) of the slot's rect: depth, kind and blood share, the floor's height and the speed. A texel
// with no liquid within two texels, the reach of the surfaces' spline, carries a speed of -1: they skip it.
@compute @workgroup_size(8, 8, 1)
fn publish(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = live[id.z];
    let x = i32(id.x) - APRON;
    let y = i32(id.y) - APRON;
    if x >= SIDE + APRON || y >= SIDE + APRON {
        return;
    }
    let into = vec2<i32>(i32(slots[slot].atlas_x) + i32(id.x), i32(slots[slot].atlas_y) + i32(id.y));
    let j = texel_of(slot, x, y);
    if j < 0 || floors[j] >= WALL {
        textureStore(atlas, into, vec4(0.0, 0.0, NO_FLOOR, 0.0));
        return;
    }
    let w = water_in[j];
    let out = outflow(u32(j));
    let speed = length(vec2(out.y - out.x, out.w - out.z)) / (TEXEL_UU * max(w.x, step.film));
    var near = w.x > 0.0;
    for (var dy = -2; dy <= 2 && !near; dy++) {
        for (var dx = -2; dx <= 2 && !near; dx++) {
            let k = texel_of(slot, x + dx, y + dy);
            near = k >= 0 && water_in[k].x > 0.0;
        }
    }
    textureStore(atlas, into, vec4(w.x, w.y, floors[j], select(-1.0, speed, near)));
}
