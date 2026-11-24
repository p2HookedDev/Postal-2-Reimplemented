// Detail droplets. `simulate` moves every slot of the pool and bounces it off the depth prepass where it is on screen;
// `vertex` and `fragment` draw each live slot with its source's texture: a spinning cell that comes to lie on the
// surface it lands on. A blood hit's streaks are drawn stretched along their motion and die where they land; its
// mist is a soft puff that grows and fades.

import bevy_render::view::View;
import package::p2_render::depth_surface::{eye_distance, normal_at, on_surface, world_at};

struct Droplet {
    // Age in seconds in `w`.
    place: vec4<f32>,
    // Lifetime in seconds in `w`; 0 is an empty slot.
    velocity: vec4<f32>,
    // Half size in world units in `w`.
    colour: vec4<f32>,
    // The texture's rectangle: the corner at -right, -up, then the corner at +right, +up.
    rect: vec4<f32>,
    // Texture slot, what it is (0 a chunk, `STREAK` or `MIST`), its turn in radians, gravity.
    shape: vec4<f32>,
    // The normal of the surface it lies on; zero in the air.
    rest: vec4<f32>,
}

struct Push {
    // Its reach in `w`.
    place: vec4<f32>,
    // The speed it gives at its middle in `x`, none at its reach; the hit's way in `yzw`, zero for none.
    speed: vec4<f32>,
}

const PUSHES: u32 = 8;
const STREAK: u32 = 1u;
const MIST: u32 = 2u;

// `count` slots from `first` on, around the ring, may hold a droplet that lives.
struct Frame {
    seconds: f32,
    count: u32,
    pushes: u32,
    first: u32,
    push: array<Push, PUSHES>,
}

@group(0) @binding(0) var<storage, read_write> pool: array<Droplet>;
@group(0) @binding(1) var depth: texture_depth_2d;
@group(0) @binding(2) var<uniform> view: View;
@group(0) @binding(3) var<uniform> frame: Frame;
@group(0) @binding(4) var<storage, read> drawn: array<Droplet>;
@group(0) @binding(5) var texture_sampler: sampler;
@group(0) @binding(6) var texture_0: texture_2d<f32>;
@group(0) @binding(7) var texture_1: texture_2d<f32>;
@group(0) @binding(8) var texture_2: texture_2d<f32>;
@group(0) @binding(9) var texture_3: texture_2d<f32>;
@group(0) @binding(10) var texture_4: texture_2d<f32>;
@group(0) @binding(11) var texture_5: texture_2d<f32>;
@group(0) @binding(12) var texture_6: texture_2d<f32>;
@group(0) @binding(13) var texture_7: texture_2d<f32>;

const DRAG: f32 = 1.2;
// How far behind a surface a droplet may be and still be taken as having just hit it, in Unreal units.
const THICKNESS: f32 = 16.0;
const REST_SPEED: f32 = 25.0;
// On landing, the share of its speed into the surface a chunk bounces back with and of its speed along it it keeps.
// Matches the chunk's landing noted at `DROPLET_GRAVITY` in `gpu_particles.rs`.
const RESTITUTION: f32 = 0.35;
const KEEP: f32 = 0.7;
// How far the scene may fall away under a lying droplet before it drops again, in Unreal units.
const UNSUPPORTED: f32 = 6.0;
// How far over its surface a lying droplet is held.
const LIFT: f32 = 0.4;
// The most a chunk spins in the air, in radians a second.
const SPIN: f32 = 10.0;
const MIST_DRAG: f32 = 6.0;
// In a droplet's `rest.w`: no explosion or hit pushes it (a blood hit's spray, which would otherwise fly again at every
// shot into the same body).
const UNPUSHED: f32 = 1.0;
// A puff reaches this many times its birth size by half its life.
const MIST_GROWTH: f32 = 4.0;
const MIST_ALPHA: f32 = 0.5;
// A streak's tail reaches back over the way it moved in twice this many seconds.
const STREAK_SHUTTER: f32 = 0.0167;

fn alive(d: Droplet) -> bool {
    return d.velocity.w > 0.0 && d.place.w < d.velocity.w;
}

fn lying(d: Droplet) -> bool {
    return dot(d.rest.xyz, d.rest.xyz) > 0.5;
}

fn spin_of(slot: u32) -> f32 {
    return (fract(sin(f32(slot) * 12.9898) * 43758.5453) * 2.0 - 1.0) * SPIN;
}

@compute @workgroup_size(64)
fn simulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= frame.count {
        return;
    }
    let slot = (frame.first + id.x) % arrayLength(&pool);
    let d = pool[slot];
    if !alive(d) {
        return;
    }
    let dt = frame.seconds;
    let kind = u32(d.shape.y);
    var age = d.place.w + dt;
    // An explosion or a hit near it throws it away from there and as much upward, off what it lay on.
    var pushed = vec3(0.0);
    let pushes = select(min(frame.pushes, PUSHES), 0u, d.rest.w == UNPUSHED);
    for (var i = 0u; i < pushes; i++) {
        let off = d.place.xyz - frame.push[i].place.xyz;
        let far = length(off);
        let reach = frame.push[i].place.w;
        if far < reach {
            let away = select(vec3(0.0), off / far, far > 1e-3);
            pushed += normalize(away + frame.push[i].speed.yzw + vec3(0.0, 1.0, 0.0)) * frame.push[i].speed.x * (1.0 - far / reach);
        }
    }
    let thrown = dot(pushed, pushed) > 0.0;
    let before = d.velocity.xyz + pushed;
    let was_lying = lying(d) && !thrown;
    var v = vec3(0.0);
    var p = d.place.xyz;
    var turn = d.shape.z;
    var rest = select(d.rest, vec4(0.0, 0.0, 0.0, d.rest.w), thrown);
    var life = d.velocity.w;
    // A droplet only comes to lie once it has stopped, so one lying still has no speed to move it.
    if !was_lying {
        v = before * exp(-select(DRAG, MIST_DRAG, kind == MIST) * dt) - vec3(0.0, d.shape.w * dt, 0.0);
        p += v * dt;
        turn += spin_of(slot) * dt;
    }
    let clip = view.clip_from_world * vec4(p, 1.0);
    let ndc = clip.xy / max(clip.w, 1e-4);
    if clip.w > 0.0 && all(abs(ndc) < vec2(1.0)) {
        let size = vec2<f32>(textureDimensions(depth));
        let pixel = clamp(vec2<i32>((ndc * vec2(0.5, -0.5) + 0.5) * size), vec2(1), vec2<i32>(size) - 2);
        // Depth 0 is the sky.
        let solid = textureLoad(depth, pixel, 0) > 0.0;
        let surface = world_at(depth, view.world_from_clip, pixel, size);
        let behind = eye_distance(view.view_from_world, p) - eye_distance(view.view_from_world, surface);
        if was_lying {
            // What it lay on is gone: a door opened, a body moved away.
            if !solid || behind < -UNSUPPORTED {
                rest = vec4(0.0, 0.0, 0.0, d.rest.w);
            }
        } else if solid && kind != MIST {
            if behind > 0.0 && behind < THICKNESS {
                let n = normal_at(depth, view.world_from_clip, view.world_position, pixel, size, surface);
                let into = dot(v, n);
                if into < 0.0 && kind == STREAK {
                    life = age;
                } else if into < 0.0 {
                    let along = (v - into * n) * KEEP;
                    let hop = -into * RESTITUTION;
                    v = along + hop * n;
                    p = on_surface(view.view_from_world, view.world_position, p, surface, n, LIFT);
                    // A blood hit's droplet comes to rest only on a floor: on a wall or ceiling it falls on.
                    if length(v) < REST_SPEED && (n.y > 0.5 || d.rest.w != UNPUSHED) {
                        v = vec3(0.0);
                        rest = vec4(n, d.rest.w);
                    }
                }
            }
        }
    }
    pool[slot] = Droplet(vec4(p, age), vec4(v, life), d.colour, d.rect, vec4(d.shape.xy, turn, d.shape.w), rest);
}

struct DropletOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // The alpha is how much of the texture's own alpha is kept.
    @location(1) colour: vec4<f32>,
    @location(2) @interpolate(flat) slot: u32,
    // The quad's own corner, -1 to 1 each way.
    @location(3) local: vec2<f32>,
    @location(4) @interpolate(flat) kind: u32,
}

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> DropletOutput {
    var out: DropletOutput;
    let d = drawn[index / 6u];
    if !alive(d) {
        out.position = vec4(2.0, 2.0, 2.0, 1.0);
        return out;
    }
    var corners = array<vec2<f32>, 6>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0));
    let corner = corners[index % 6u];
    let age = d.place.w / d.velocity.w;
    let half = d.colour.w;
    let turn = d.shape.z;
    let turned = vec2(corner.x * cos(turn) - corner.y * sin(turn), corner.x * sin(turn) + corner.y * cos(turn));
    var right = view.world_from_view[0].xyz;
    var up = view.world_from_view[1].xyz;
    if lying(d) {
        let n = d.rest.xyz;
        right = normalize(cross(n, select(vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), abs(n.y) > 0.9)));
        up = cross(n, right);
    }
    var offset = (right * turned.x + up * turned.y) * half;
    var alpha = 1.0 - smoothstep(0.85, 1.0, age);
    let kind = u32(d.shape.y);
    if kind == MIST {
        offset *= 1.0 + (MIST_GROWTH - 1.0) * smoothstep(0.0, 0.5, age);
        alpha = MIST_ALPHA * (1.0 - age) * (1.0 - age);
    } else if kind == STREAK {
        // Its motion across the screen; the head is at its place, the tail behind it.
        let forward = view.world_from_view[2].xyz;
        let motion = d.velocity.xyz - forward * dot(d.velocity.xyz, forward);
        let speed = length(motion);
        let along = select(up, motion / max(speed, 1e-4), speed > 1e-3);
        let side = normalize(cross(along, forward));
        let reach = max(half, speed * STREAK_SHUTTER);
        offset = side * corner.x * half + along * (corner.y - 1.0) * reach;
    }
    out.colour = vec4(d.colour.rgb, alpha);
    out.position = view.clip_from_world * vec4(d.place.xyz + offset, 1.0);
    out.local = corner;
    out.kind = kind;
    out.uv = mix(d.rect.xy, d.rect.zw, corner * 0.5 + 0.5);
    out.slot = u32(d.shape.x);
    return out;
}

fn texel(slot: u32, uv: vec2<f32>) -> vec4<f32> {
    switch slot {
        case 0u: { return textureSampleLevel(texture_0, texture_sampler, uv, 0.0); }
        case 1u: { return textureSampleLevel(texture_1, texture_sampler, uv, 0.0); }
        case 2u: { return textureSampleLevel(texture_2, texture_sampler, uv, 0.0); }
        case 3u: { return textureSampleLevel(texture_3, texture_sampler, uv, 0.0); }
        case 4u: { return textureSampleLevel(texture_4, texture_sampler, uv, 0.0); }
        case 5u: { return textureSampleLevel(texture_5, texture_sampler, uv, 0.0); }
        case 6u: { return textureSampleLevel(texture_6, texture_sampler, uv, 0.0); }
        default: { return textureSampleLevel(texture_7, texture_sampler, uv, 0.0); }
    }
}

@fragment
fn fragment(in: DropletOutput) -> @location(0) vec4<f32> {
    // Reversed depth: a smaller value is farther than the scene.
    if in.position.z < textureLoad(depth, vec2<i32>(in.position.xy), 0) {
        discard;
    }
    if in.kind == MIST {
        let a = in.colour.a * (1.0 - smoothstep(0.0, 1.0, dot(in.local, in.local)));
        return vec4(in.colour.rgb * a, a);
    }
    if in.kind == STREAK {
        let a = in.colour.a * (1.0 - in.local.x * in.local.x) * smoothstep(-1.0, 0.6, in.local.y);
        return vec4(in.colour.rgb * a, a);
    }
    let sampled = texel(in.slot, in.uv);
    let alpha = sampled.a * in.colour.a;
    return vec4(sampled.rgb * in.colour.rgb * alpha, alpha);
}
