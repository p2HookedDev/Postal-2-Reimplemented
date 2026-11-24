// The liquid stream. Each lane of the pool holds one pour's particles in the order they were poured, so a particle's
// neighbours in the stream are the slots beside it. `flow` reads last frame's pool and writes the other one: it flies
// each particle, pulls linked neighbours together, blends their speed, breaks a link stretched too far, and lands,
// slides, spreads and fades particles on the depth prepass.

import bevy_render::view::View;
import package::p2_render::depth_surface::{eye_distance, normal_at, on_surface, world_at};

struct LiquidParticle {
    // Radius in `w`; 0 is an empty slot.
    place: vec4<f32>,
    velocity: vec3<f32>,
    // The kind in the low four bits, then LANDED, BURNING and BROKEN; the age in AGE_UNITS from AGE_SHIFT up.
    bits: u32,
}

// `live` slots from the pool's start are stepped; a lane is `lane` slots.
struct Frame {
    seconds: f32,
    live: u32,
    lane: u32,
    unused: u32,
}

// Lengths in Unreal units, rates per second.
struct Kind {
    radius: f32,
    life: f32,
    // A link pulls back only when longer than `spacing`, and breaks past `snap`.
    spacing: f32,
    snap: f32,
    cohesion: f32,
    viscosity: f32,
    // How fast it stops sliding once on a surface.
    friction: f32,
    // How much wider it lies on a surface than it flies.
    spread: f32,
}

// Indexed by the kind byte; slot 0 is no liquid.
const KINDS: array<Kind, 9> = array<Kind, 9>(
    Kind(0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0),
    // Gas: thin and runny.
    Kind(1.0, 3.0, 4.0, 10.0, 18.0, 2.0, 1.5, 2.2),
    // Urine.
    Kind(0.9, 3.0, 4.0, 11.0, 20.0, 2.0, 2.0, 2.0),
    // Blood: sticky.
    Kind(1.1, 5.0, 5.0, 16.0, 30.0, 6.0, 7.0, 1.8),
    // Puke: thick and lumpy.
    Kind(1.6, 5.0, 6.0, 22.0, 35.0, 10.0, 10.0, 1.6),
    // Bloody puke.
    Kind(1.5, 5.0, 6.0, 21.0, 34.0, 9.0, 9.0, 1.6),
    // Gonorrhea.
    Kind(1.0, 3.5, 5.0, 15.0, 26.0, 4.0, 4.0, 1.9),
    // Bloody urine.
    Kind(0.95, 3.5, 4.5, 12.0, 22.0, 3.0, 3.0, 2.0),
    // Napalm: sticky jelly.
    Kind(1.3, 6.0, 5.5, 18.0, 32.0, 8.0, 9.0, 1.7),
);

const KIND_MASK: u32 = 0xFu;
const LANDED: u32 = 16u;
const BURNING: u32 = 32u;
// The link to the next slot, the one poured after, is broken.
const BROKEN: u32 = 64u;
const AGE_SHIFT: u32 = 16u;
const AGE_UNITS: f32 = 4096.0;
// No kind lives longer, so the pool knows when it is quiet.
const LONGEST_LIFE: f32 = 6.0;
// Slots side by side whose ages differ more were not poured together: a pause in the pour or a lane that wrapped.
const LINK_GAP: f32 = 0.15;
// How far behind a surface a particle may be and still be taken as having just hit it.
const THICKNESS: f32 = 16.0;
// How far the scene may fall away under a lying particle before it runs off the edge.
const UNSUPPORTED: f32 = 6.0;
const LIFT: f32 = 0.3;
// A landing kills the speed into the surface and splashes this share of the speed along it.
const SPLASH_KEEP: f32 = 0.6;
// How fast a landed particle spreads to its lying width.
const SPREAD_RATE: f32 = 6.0;
const FADE_FROM: f32 = 0.75;

@group(0) @binding(0) var<storage, read> before: array<LiquidParticle>;
@group(0) @binding(1) var<storage, read_write> after: array<LiquidParticle>;
@group(0) @binding(2) var depth: texture_depth_2d;
@group(0) @binding(3) var<uniform> view: View;
@group(0) @binding(4) var<uniform> frame: Frame;
// Each lane's gravity in `xyz`.
@group(0) @binding(5) var<storage, read> lanes: array<vec4<f32>>;

fn kind_of(bits: u32) -> Kind {
    var table = KINDS;
    return table[min(bits & KIND_MASK, 8u)];
}

fn age_of(bits: u32) -> f32 {
    return f32(bits >> AGE_SHIFT) / AGE_UNITS;
}

fn with_age(bits: u32, age: f32) -> u32 {
    return (bits & 0xFFFFu) | (u32(clamp(round(age * AGE_UNITS), 0.0, 65535.0)) << AGE_SHIFT);
}

// The slot `by` places on from `slot`, wrapping inside its lane.
fn beside(slot: u32, by: u32) -> u32 {
    let base = slot - slot % frame.lane;
    return base + (slot - base + by) % frame.lane;
}

fn joined(p: LiquidParticle, q: LiquidParticle) -> bool {
    return q.place.w > 0.0 && (q.bits & LANDED) == 0u && (q.bits & KIND_MASK) == (p.bits & KIND_MASK)
        && abs(age_of(q.bits) - age_of(p.bits)) < LINK_GAP;
}

fn flown(q: LiquidParticle, gravity: vec3<f32>, dt: f32) -> vec3<f32> {
    return q.place.xyz + (q.velocity + gravity * dt) * dt;
}

// How far a link moves `here` toward its neighbour `there`; a negative `w` is a link stretched past breaking.
fn pulled(here: vec3<f32>, there: vec3<f32>, k: Kind, share: f32) -> vec4<f32> {
    let off = there - here;
    let far = length(off);
    if far > k.snap {
        return vec4(0.0, 0.0, 0.0, -1.0);
    }
    if far <= k.spacing {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }
    return vec4(off / far * (far - k.spacing) * share, 1.0);
}

@compute @workgroup_size(64)
fn flow(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x;
    if slot >= frame.live {
        return;
    }
    let p = before[slot];
    let dt = frame.seconds;
    // Every stepped slot is written, as the pool written this frame still holds the frame before last.
    if p.place.w <= 0.0 || dt <= 0.0 {
        after[slot] = p;
        return;
    }
    let k = kind_of(p.bits);
    let life = min(k.life, LONGEST_LIFE);
    let age = age_of(p.bits) + dt;
    if age >= life {
        after[slot] = LiquidParticle(vec4(p.place.xyz, 0.0), vec3(0.0), p.bits);
        return;
    }
    var bits = with_age(p.bits, age);
    var landed = (p.bits & LANDED) != 0u;
    var v = p.velocity;
    var x = p.place.xyz;
    if landed {
        v *= exp(-k.friction * dt);
        x += v * dt;
    } else {
        // Both ends of a link measure it from the same flown places, so they agree on whether it breaks.
        let gravity = lanes[slot / frame.lane].xyz;
        let here = flown(p, gravity, dt);
        let share = min(1.0 - exp(-k.cohesion * dt), 0.5);
        var shift = vec3(0.0);
        var pace = vec3(0.0);
        var links = 0.0;
        let newer = before[beside(slot, 1u)];
        if (p.bits & BROKEN) == 0u && joined(p, newer) {
            let link = pulled(here, flown(newer, gravity, dt), k, share);
            if link.w < 0.0 {
                bits |= BROKEN;
            } else {
                shift += link.xyz;
                pace += newer.velocity;
                links += 1.0;
            }
        }
        let older = before[beside(slot, frame.lane - 1u)];
        if (older.bits & BROKEN) == 0u && joined(p, older) {
            let link = pulled(here, flown(older, gravity, dt), k, share);
            if link.w >= 0.0 {
                shift += link.xyz;
                pace += older.velocity;
                links += 1.0;
            }
        }
        x = here + shift;
        v = (x - p.place.xyz) / dt;
        if links > 0.0 {
            v = mix(v, pace / links, 1.0 - exp(-k.viscosity * dt));
        }
    }
    let clip = view.clip_from_world * vec4(x, 1.0);
    let ndc = clip.xy / max(clip.w, 1e-4);
    if clip.w > 0.0 && all(abs(ndc) < vec2(1.0)) {
        let size = vec2<f32>(textureDimensions(depth));
        let pixel = clamp(vec2<i32>((ndc * vec2(0.5, -0.5) + 0.5) * size), vec2(1), vec2<i32>(size) - 2);
        // Depth 0 is the sky.
        let solid = textureLoad(depth, pixel, 0) > 0.0;
        let surface = world_at(depth, view.world_from_clip, pixel, size);
        let behind = eye_distance(view.view_from_world, x) - eye_distance(view.view_from_world, surface);
        if landed {
            if !solid || behind < -UNSUPPORTED {
                landed = false;
            } else {
                x = on_surface(view.view_from_world, view.world_position, x, surface, normal_at(depth, view.world_from_clip, view.world_position, pixel, size, surface), LIFT);
            }
        } else if solid && behind > 0.0 && behind < THICKNESS {
            let n = normal_at(depth, view.world_from_clip, view.world_position, pixel, size, surface);
            let into = dot(v, n);
            if into < 0.0 {
                v = (v - into * n) * SPLASH_KEEP;
                x = on_surface(view.view_from_world, view.world_position, x, surface, n, LIFT);
                landed = true;
            }
        }
    }
    bits = select(bits & ~LANDED, bits | LANDED, landed);
    let fade = 1.0 - smoothstep(FADE_FROM, 1.0, age / life);
    let width = k.radius * select(1.0, k.spread, landed) * fade;
    let radius = select(width, mix(p.place.w, width, 1.0 - exp(-SPREAD_RATE * dt)), landed);
    after[slot] = LiquidParticle(vec4(x, radius), v, bits);
}
