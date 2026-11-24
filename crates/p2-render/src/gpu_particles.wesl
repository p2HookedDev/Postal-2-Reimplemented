// Detail droplets. `simulate` moves every slot of the pool and bounces it off the depth prepass where it is on screen;
// `vertex` and `fragment` draw each live slot with its source's texture: a spinning cell that comes to lie on the
// surface it lands on, sliding along it first if its kind slides, or a streak along its motion.

import bevy_render::view::View;

struct Droplet {
    // Age in seconds in `w`.
    place: vec4<f32>,
    // Lifetime in seconds in `w`; 0 is an empty slot.
    velocity: vec4<f32>,
    // Half size in world units in `w`.
    colour: vec4<f32>,
    // The texture's rectangle: the corner at -right, -up, then the corner at +right, +up.
    rect: vec4<f32>,
    // Texture slot, length over width (0: a square that spins), its turn in radians, gravity.
    shape: vec4<f32>,
    // The normal of the surface it lies or slides on; zero in the air.
    rest: vec4<f32>,
    // On landing, the share of its speed into the surface it bounces back with and of its speed along it it keeps,
    // then how fast it slows while sliding in Unreal units a second squared (0 never slides).
    skid: vec4<f32>,
}

struct Push {
    // Its reach in `w`.
    place: vec4<f32>,
    // The speed it gives at its middle in `x`, none at its reach.
    speed: vec4<f32>,
}

const PUSHES: u32 = 8;

struct Frame {
    seconds: f32,
    count: u32,
    pushes: u32,
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
// A piece that slides stays on the surface when it would bounce off slower than this, in Unreal units a second.
const HOP_SPEED: f32 = 60.0;
// The sliding speed at which a piece turns at its full spin, in Unreal units a second.
const SKID_SPEED: f32 = 200.0;
// A sliding piece follows its surface only while the surface turns less than this cosine in a step.
const FOLLOW: f32 = 0.7;
const REST_FADE: f32 = 0.35;
// How far the scene may fall away under a lying droplet before it drops again, in Unreal units.
const UNSUPPORTED: f32 = 6.0;
// How far over its surface a lying droplet is held.
const LIFT: f32 = 0.4;
// The most a chunk spins in the air, in radians a second.
const SPIN: f32 = 10.0;
// The speed at which a streak has its full length, in Unreal units a second.
const STREAK_SPEED: f32 = 300.0;
// What a spark burns down to.
const EMBER: vec3<f32> = vec3(1.0, 0.15, 0.05);

fn world_at(pixel: vec2<i32>, size: vec2<f32>) -> vec3<f32> {
    let uv = (vec2<f32>(pixel) + 0.5) / size;
    let clip = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, textureLoad(depth, pixel, 0), 1.0);
    let world = view.world_from_clip * clip;
    return world.xyz / world.w;
}

fn eye_distance(p: vec3<f32>) -> f32 {
    return -(view.view_from_world * vec4(p, 1.0)).z;
}

fn alive(d: Droplet) -> bool {
    return d.velocity.w > 0.0 && d.place.w < d.velocity.w;
}

fn lying(d: Droplet) -> bool {
    return dot(d.rest.xyz, d.rest.xyz) > 0.5;
}

fn spin_of(slot: u32) -> f32 {
    return (fract(sin(f32(slot) * 12.9898) * 43758.5453) * 2.0 - 1.0) * SPIN;
}

fn normal_at(pixel: vec2<i32>, size: vec2<f32>, surface: vec3<f32>) -> vec3<f32> {
    let across = world_at(pixel + vec2(1, 0), size) - world_at(pixel - vec2(1, 0), size);
    let down = world_at(pixel + vec2(0, 1), size) - world_at(pixel - vec2(0, 1), size);
    let facing = cross(across, down);
    let to_eye = normalize(view.world_position - surface);
    let n = select(to_eye, normalize(facing), dot(facing, facing) > 1e-8);
    return select(n, -n, dot(n, to_eye) < 0.0);
}

// The point of the eye's line through `p` that is held over the surface.
fn on_surface(p: vec3<f32>, surface: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    return view.world_position + (p - view.world_position) * (eye_distance(surface) / eye_distance(p)) + n * LIFT;
}

@compute @workgroup_size(64)
fn simulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= frame.count {
        return;
    }
    let d = pool[id.x];
    if !alive(d) {
        return;
    }
    let dt = frame.seconds;
    // An explosion or a hit near it throws it away from there and as much upward, off what it lay on.
    var pushed = vec3(0.0);
    for (var i = 0u; i < min(frame.pushes, PUSHES); i++) {
        let off = d.place.xyz - frame.push[i].place.xyz;
        let far = length(off);
        let reach = frame.push[i].place.w;
        if far < reach {
            let away = select(vec3(0.0), off / far, far > 1e-3);
            pushed += normalize(away + vec3(0.0, 1.0, 0.0)) * frame.push[i].speed.x * (1.0 - far / reach);
        }
    }
    let thrown = dot(pushed, pushed) > 0.0;
    let before = d.velocity.xyz + pushed;
    let was_lying = lying(d) && !thrown;
    var v = vec3(0.0);
    var p = d.place.xyz;
    var turn = d.shape.z;
    var rest = select(d.rest, vec4(0.0), thrown);
    if was_lying {
        let speed = length(before);
        let slowed = max(speed - d.skid.z * dt, 0.0);
        if slowed >= REST_SPEED {
            v = before * (slowed / speed);
            p += v * dt;
            turn += spin_of(id.x) * min(speed / SKID_SPEED, 1.0) * dt;
        }
    } else {
        v = before * exp(-DRAG * dt) - vec3(0.0, d.shape.w * dt, 0.0);
        p += v * dt;
        turn += spin_of(id.x) * dt;
    }
    var life = d.velocity.w;
    let clip = view.clip_from_world * vec4(p, 1.0);
    let ndc = clip.xy / max(clip.w, 1e-4);
    if clip.w > 0.0 && all(abs(ndc) < vec2(1.0)) {
        let size = vec2<f32>(textureDimensions(depth));
        let pixel = clamp(vec2<i32>((ndc * vec2(0.5, -0.5) + 0.5) * size), vec2(1), vec2<i32>(size) - 2);
        // Depth 0 is the sky.
        let solid = textureLoad(depth, pixel, 0) > 0.0;
        let surface = world_at(pixel, size);
        let behind = eye_distance(p) - eye_distance(surface);
        if was_lying {
            // What it lay on is gone: a door opened, a body moved away.
            if !solid || behind < -UNSUPPORTED {
                rest = vec4(0.0);
            } else if dot(v, v) > 0.0 {
                let n = normal_at(pixel, size, surface);
                // A wall or a step up stops a sliding piece where it was rather than let it climb.
                if behind > UNSUPPORTED || dot(n, rest.xyz) < FOLLOW {
                    p = d.place.xyz;
                    v = vec3(0.0);
                } else {
                    rest = vec4(n, 0.0);
                    v -= dot(v, n) * n;
                    p = on_surface(p, surface, n);
                }
            }
        } else if solid {
            if behind > 0.0 && behind < THICKNESS {
                let n = normal_at(pixel, size, surface);
                let into = dot(v, n);
                if into < 0.0 {
                    let along = (v - into * n) * d.skid.y;
                    let hop = -into * d.skid.x;
                    let slides = d.skid.z > 0.0 && hop < HOP_SPEED;
                    v = select(along + hop * n, along, slides);
                    p = on_surface(p, surface, n);
                    if slides {
                        rest = vec4(n, 0.0);
                    }
                    if length(v) < REST_SPEED {
                        v = vec3(0.0);
                        if d.shape.y > 0.0 {
                            life = min(life, d.place.w + REST_FADE);
                        } else {
                            rest = vec4(n, 0.0);
                        }
                    }
                }
            }
        }
    }
    pool[id.x] = Droplet(vec4(p, d.place.w + dt), vec4(v, life), d.colour, d.rect, vec4(d.shape.xy, turn, d.shape.w), rest, d.skid);
}

struct DropletOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // The alpha is how much of the texture's own alpha is kept; below 0 the texel is added to the frame.
    @location(1) colour: vec4<f32>,
    @location(2) @interpolate(flat) slot: u32,
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
    var offset: vec3<f32>;
    if d.shape.y > 0.0 {
        let speed = length(d.velocity.xyz);
        let to_eye = normalize(view.world_position - d.place.xyz);
        let along = select(view.world_from_view[1].xyz, d.velocity.xyz / max(speed, 1e-3), speed > 1.0);
        let across = cross(along, to_eye);
        let side = select(view.world_from_view[0].xyz, normalize(across), dot(across, across) > 1e-6);
        let reach = half * max(1.0, d.shape.y * min(speed / STREAK_SPEED, 1.0));
        offset = side * corner.x * half + along * corner.y * reach;
        out.colour = vec4(d.colour.rgb * mix(vec3(1.0), EMBER, age) * (1.0 - smoothstep(0.5, 1.0, age)), -1.0);
    } else {
        let turn = d.shape.z;
        let turned = vec2(corner.x * cos(turn) - corner.y * sin(turn), corner.x * sin(turn) + corner.y * cos(turn));
        var right = view.world_from_view[0].xyz;
        var up = view.world_from_view[1].xyz;
        if lying(d) {
            let n = d.rest.xyz;
            right = normalize(cross(n, select(vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), abs(n.y) > 0.9)));
            up = cross(n, right);
        }
        offset = (right * turned.x + up * turned.y) * half;
        out.colour = vec4(d.colour.rgb, 1.0 - smoothstep(0.85, 1.0, age));
    }
    out.position = view.clip_from_world * vec4(d.place.xyz + offset, 1.0);
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
    let sampled = texel(in.slot, in.uv);
    if in.colour.a < 0.0 {
        return vec4(sampled.rgb * in.colour.rgb, 0.0);
    }
    let alpha = sampled.a * in.colour.a;
    return vec4(sampled.rgb * in.colour.rgb * alpha, alpha);
}
