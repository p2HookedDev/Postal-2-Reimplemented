const SIDE: i32 = 128;
const TEXELS: u32 = 16384u;
const APRON: i32 = 2;
const TEXEL_UU: f32 = 2.0;
const AREA: f32 = 4.0;
const WALL: f32 = 1e29;
const NO_FLOOR: f32 = 60000.0;
const EMPTY: f32 = 1e-4;
// The blood share rides in the kind channel's fraction, scaled so 1 never carries into the next kind.
const SHARE_SCALE: f32 = 0.98;
const BLOOD: f32 = 3.0;
const PUKE: f32 = 4.0;
const BLOODY_PUKE: f32 = 5.0;
const GAS: f32 = 1.0;
const URINE: f32 = 2.0;
const BLOODY_URINE: f32 = 7.0;
const NAPALM: f32 = 8.0;
// Fire rides in the kind channel's whole part as (heat * 32 + scorch) * KINDS; `publish` hands it on in the speed channel as FIRE_BASE + heat * 32 + scorch, exact in a half float.
const KINDS: f32 = 16.0;
const LEVELS: f32 = 31.0;
const FIRE_BASE: f32 = 1024.0;
const FUEL: f32 = 0.02;
const SPREAD: f32 = 45.0;
const BURN: f32 = 0.1;
const NAPALM_BURN: f32 = 0.4;
const CHAR: f32 = 0.6;
const DIE: f32 = 3.0;
const SCORCH_LIFE: f32 = 40.0;
const ADDS: u32 = 1u;
const BURNING: u32 = 2u;
const PASTE_FILM: f32 = 0.8;

// `LayerStep` in `liquids/layer_gpu.rs`.
struct Step {
    seconds: f32,
    film: f32,
    drying: f32,
    pull: f32,
    damping: f32,
    seed: u32,
    pad: vec2<f32>,
}

struct Slot {
    // The slot of each tile around this one, -1 where none, at (dy + 1) * 3 + dx + 1; this one at 4.
    around: array<i32, 9>,
    first_source: u32,
    sources: u32,
    atlas_x: u32,
    atlas_y: u32,
}

struct Source {
    x: f32,
    y: f32,
    radius: f32,
    amount: f32,
    kind: f32,
    adds: u32,
    seed: u32,
}

const SHAPE_LOBES: f32 = 0.8;
const SHAPE_LOBE: f32 = 0.35;
const SHAPE_BUMPS: f32 = 2.6;
const SHAPE_BUMP: f32 = 0.07;
const SHAPE_LEAST: f32 = 3.0;
// `SHAPE_REACH` in `liquids/layer.rs`: past the farthest lobe, in radii.
const SHAPE_REACH: f32 = 1.45;

fn cover_of(radius: f32, rel: vec2<f32>) -> f32 {
    return saturate(radius - length(rel) + 0.5);
}

fn lattice_of(c: vec2<i32>) -> f32 {
    return unit_of(pcg(bitcast<u32>(c.x) ^ pcg(bitcast<u32>(c.y))));
}

fn shape_noise(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let c = vec2<i32>(cell);
    let f = p - cell;
    let t = f * f * (3.0 - 2.0 * f);
    let low = mix(lattice_of(c), lattice_of(c + vec2(1, 0)), t.x);
    let high = mix(lattice_of(c + vec2(0, 1)), lattice_of(c + vec2(1, 1)), t.x);
    return mix(low, high, t.y);
}

fn outline_of(source: Source, rel: vec2<f32>) -> f32 {
    let way = rel / max(length(rel), 1e-3);
    let s = vec2(unit_of(pcg(source.seed)), unit_of(pcg(source.seed ^ 0x68e31da4u))) * 256.0;
    let lobes = shape_noise(way * SHAPE_LOBES + s) - 0.5;
    let bumps = shape_noise(way * SHAPE_BUMPS + s.yx) - 0.5;
    return source.radius + 2.0 * (lobes * SHAPE_LOBE + bumps * SHAPE_BUMP) * max(source.radius - SHAPE_LEAST, 0.0);
}

@group(0) @binding(0) var<uniform> step: Step;
@group(0) @binding(1) var<storage, read> slots: array<Slot>;
@group(0) @binding(2) var<storage, read> live: array<u32>;
@group(0) @binding(3) var<storage, read> sources: array<Source>;
@group(0) @binding(4) var<storage, read> water_in: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> water_out: array<vec2<f32>>;
@group(0) @binding(6) var<storage, read_write> flux: array<vec2<u32>>;
@group(0) @binding(7) var<storage, read> floors: array<f32>;
@group(0) @binding(8) var atlas: texture_storage_2d<rgba16float, write>;

// A private var, as only memory may be indexed by a value known at run time.
var<private> WAYS: array<vec2<i32>, 4> = array<vec2<i32>, 4>(vec2(-1, 0), vec2(1, 0), vec2(0, -1), vec2(0, 1));

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
    let whole = floor(w.y);
    return whole - KINDS * floor(whole / KINDS);
}

fn fire_of(w: vec2<f32>) -> vec2<f32> {
    let code = floor(floor(w.y) / KINDS);
    let heat = floor(code / 32.0);
    return vec2(heat, code - heat * 32.0);
}

fn burns(kind: f32) -> bool {
    return kind == GAS || kind == NAPALM;
}

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn unit_of(h: u32) -> f32 {
    return f32(h >> 8u) / 16777216.0;
}

fn share_of(w: vec2<f32>) -> f32 {
    return fract(w.y) / SHARE_SCALE;
}

fn film_of(kind: f32) -> f32 {
    return select(step.film, PASTE_FILM, kind == PUKE || kind == BLOODY_PUKE);
}

fn packed(depth: f32, kind: f32, share: f32, fire: vec2<f32>, dither: f32) -> vec2<f32> {
    let q = clamp(floor(fire + dither), vec2(0.0), vec2(LEVELS));
    let code = (q.x * 32.0 + q.y) * KINDS;
    if depth <= EMPTY {
        return vec2(0.0, code);
    }
    return vec2(depth, code + kind + saturate(share) * SHARE_SCALE);
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
    var fire = fire_of(w);
    let middle = vec2(f32(at.x), f32(at.y)) + 0.5;
    for (var k = 0u; k < slot.sources; k++) {
        let source = sources[slot.first_source + k];
        let rel = middle - vec2(source.x, source.y);
        let held = (source.adds & ADDS) == 0u;
        var outline = source.radius;
        if held {
            let far = source.radius * SHAPE_REACH + 0.5;
            if dot(rel, rel) > far * far {
                continue;
            }
            outline = outline_of(source, rel);
        }
        let cover = cover_of(outline, rel);
        if cover <= 0.0 {
            continue;
        }
        var added = 0.0;
        if held {
            let reach = length(rel) / max(outline, 1.0);
            added = max(source.amount * cover * sqrt(saturate(1.0 - reach * reach)) - depth, 0.0);
        } else {
            added = source.amount * cover / (3.14159265 * max(source.radius * source.radius, 1.0) * AREA);
        }
        if added > 0.0 {
            let blood = select(0.0, 1.0, source.kind == BLOOD);
            share = (share * depth + blood * added) / (depth + added);
            if added > depth {
                kind = source.kind;
            }
            depth += added;
        }
        if source.kind == URINE || source.kind == BLOODY_URINE {
            if fire.x > 0.0 {
                kind = source.kind;
            }
            fire.x = 0.0;
        } else if (source.adds & BURNING) != 0u && cover > 0.5 && burns(kind) && depth > FUEL {
            fire.x = LEVELS;
        }
    }
    water_out[at.index] = packed(depth, kind, share, fire, 0.0);
}

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
}

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
    var hottest = 0.0;
    for (var k = 0; k < 4; k++) {
        let j = texel_of(at.slot, at.x + WAYS[k].x, at.y + WAYS[k].y);
        if j < 0 {
            continue;
        }
        let other = water_in[j];
        hottest = max(hottest, fire_of(other).x);
        let came = outflow(u32(j))[k ^ 1] * step.seconds / AREA;
        if came <= 0.0 {
            continue;
        }
        depth += came;
        blood += came * share_of(other);
        if came > most {
            most = came;
            kind = kind_of(other);
        }
    }
    let share = blood / max(depth, EMPTY);
    depth = max(depth - step.drying * step.seconds, 0.0);
    var fire = fire_of(w);
    let fuel = burns(kind) && depth > FUEL;
    let grain = 0.6 + 0.8 * unit_of(pcg(at.index));
    if fuel && (fire.x > 0.0 || hottest >= LEVELS) {
        fire.x = min(fire.x + LEVELS * SPREAD * grain * step.seconds, LEVELS);
        depth = max(depth - select(BURN, BURN * NAPALM_BURN, kind == NAPALM) * step.seconds * fire.x / LEVELS, 0.0);
    } else if fire.x > 0.0 {
        fire.x -= LEVELS * DIE * step.seconds;
    }
    fire.y += LEVELS * step.seconds * (CHAR * min(fire.x, LEVELS) / LEVELS - 1.0 / SCORCH_LIFE);
    water_out[at.index] = packed(depth, kind, share, fire, unit_of(pcg(at.index ^ pcg(step.seed))));
}

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
    let fire = fire_of(w);
    let alight = fire.x + fire.y > 0.0;
    let shown = select(min(speed, FIRE_BASE - 1.0), FIRE_BASE + fire.x * 32.0 + fire.y, alight);
    var near = w.x > 0.0 || alight;
    for (var dy = -2; dy <= 2 && !near; dy++) {
        for (var dx = -2; dx <= 2 && !near; dx++) {
            let k = texel_of(slot, x + dx, y + dy);
            near = k >= 0 && (water_in[k].x > 0.0 || water_in[k].y >= KINDS);
        }
    }
    textureStore(atlas, into, vec4(w.x, kind_of(w) + fract(w.y), floors[j], select(-1.0, shown, near)));
}
