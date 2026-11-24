// Fire as a volume. `march` runs at half size: a ray from the eye crosses each fire's box, stops at the depth
// prepass, and gathers flame light through noise that rises with time. `composite` runs at full size and adds a
// bilinear sample of that to the frame, as retail adds its flame sprites; the fire's smoke stays sprites.

import bevy_render::view::View;
import package::p2_render::screen_pass::gradient_noise;

struct FullscreenOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

const MAX_VOLUMES: u32 = 8u;

struct Volume {
    centre: vec4<f32>,
    extent: vec4<f32>,
    tint: vec4<f32>,
}

struct Fires {
    volumes: array<Volume, MAX_VOLUMES>,
    count: u32,
    steps: u32,
    octaves: u32,
    seconds: f32,
}

@group(0) @binding(0) var marched: texture_2d<f32>;
@group(0) @binding(1) var depth: texture_depth_2d;
@group(0) @binding(2) var<uniform> view: View;
@group(0) @binding(3) var<uniform> fires: Fires;

// Per Unreal unit (about 2 cm), tuned by eye later (unverified).
const EXTINCTION: f32 = 0.03;
const EMISSION: f32 = 0.08;
// Noise cells a second.
const RISE: f32 = 1.5;
const STRETCH: vec3<f32> = vec3(1.0, 0.5, 1.0);

fn hash(p: vec3<f32>) -> f32 {
    let q = fract(p * 0.3183099 + vec3(0.71, 0.113, 0.419)) * 17.0;
    return fract(q.x * q.y * q.z * (q.x + q.y + q.z));
}

fn value_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let near = mix(
        mix(hash(i), hash(i + vec3(1.0, 0.0, 0.0)), u.x),
        mix(hash(i + vec3(0.0, 1.0, 0.0)), hash(i + vec3(1.0, 1.0, 0.0)), u.x),
        u.y,
    );
    let far = mix(
        mix(hash(i + vec3(0.0, 0.0, 1.0)), hash(i + vec3(1.0, 0.0, 1.0)), u.x),
        mix(hash(i + vec3(0.0, 1.0, 1.0)), hash(i + vec3(1.0, 1.0, 1.0)), u.x),
        u.y,
    );
    return mix(near, far, u.z);
}

fn fbm(p: vec3<f32>) -> f32 {
    var sum = 0.0;
    var amplitude = 0.5;
    var total = 0.0;
    var q = p;
    for (var i = 0u; i < fires.octaves; i++) {
        sum += amplitude * value_noise(q);
        total += amplitude;
        q = q * 2.03 + vec3(17.1, 3.7, 9.2);
        amplitude *= 0.5;
    }
    return sum / max(total, 1e-4);
}

fn flame(v: Volume, p: vec3<f32>) -> vec2<f32> {
    let local = (p - v.centre.xyz) / max(v.extent.xyz, vec3(1.0));
    let height = saturate(local.y * 0.5 + 0.5);
    let core = saturate(1.0 - length(local.xz) / mix(1.0, 0.3, height));
    let cells = 1.5 / max(min(v.extent.x, v.extent.z), 8.0);
    let n = fbm(p * cells * STRETCH - vec3(0.0, fires.seconds * RISE, 0.0));
    let density = saturate(core * 1.5 - 0.35 + (n - 0.5) * 2.0) * saturate((1.0 - height) * 3.0) * v.tint.w;
    let heat = saturate(density * 1.6 * (1.0 - height * 0.8));
    return vec2(density, heat);
}

fn black_body(heat: f32) -> vec3<f32> {
    let t = saturate(heat) * 3.0;
    let low = mix(vec3(0.6, 0.05, 0.0), vec3(1.0, 0.35, 0.03), saturate(t));
    let mid = mix(low, vec3(1.0, 0.75, 0.25), saturate(t - 1.0));
    return mix(mid, vec3(1.0, 0.95, 0.8), saturate(t - 2.0));
}

@fragment
fn march(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(depth));
    let texel = min(vec2<i32>(in.position.xy) * 2 + vec2<i32>(view.viewport.xy), size - 1);
    let ndc = vec2(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let eye = view.world_position;
    let near = view.world_from_clip * vec4(ndc, 1.0, 1.0);
    let toward = normalize(near.xyz / near.w - eye);
    let z = textureLoad(depth, texel, 0);
    var scene = 1e9;
    if z > 0.0 {
        let at = view.world_from_clip * vec4(ndc, z, 1.0);
        scene = distance(at.xyz / at.w, eye);
    }
    let inverse = select(1.0 / toward, vec3(1e9), abs(toward) < vec3(1e-6));
    let jitter = gradient_noise(in.position.xy);
    var light = vec3(0.0);
    var clear = 1.0;
    for (var i = 0u; i < min(fires.count, MAX_VOLUMES); i++) {
        let v = fires.volumes[i];
        let t0 = (v.centre.xyz - v.extent.xyz - eye) * inverse;
        let t1 = (v.centre.xyz + v.extent.xyz - eye) * inverse;
        let lo = min(t0, t1);
        let hi = max(t0, t1);
        let start = max(max(max(lo.x, lo.y), lo.z), 0.0);
        let end = min(min(min(hi.x, hi.y), hi.z), scene);
        if end <= start {
            continue;
        }
        let stride = (end - start) / f32(max(fires.steps, 1u));
        for (var k = 0u; k < fires.steps; k++) {
            let fire = flame(v, eye + toward * (start + (f32(k) + jitter) * stride));
            if fire.x <= 0.0 {
                continue;
            }
            let absorbed = 1.0 - exp(-fire.x * EXTINCTION * stride);
            let glow = black_body(fire.y) * v.tint.rgb * (fire.y * fire.y * EMISSION * fire.x * stride);
            light += clear * glow;
            clear *= 1.0 - absorbed;
        }
        if clear < 0.01 {
            break;
        }
    }
    return vec4(light, 0.0);
}

@fragment
fn composite(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let last = vec2<i32>(textureDimensions(marched)) - 1;
    let at = (in.position.xy - view.viewport.xy) * 0.5 - 0.5;
    let base = vec2<i32>(floor(at));
    let f = at - floor(at);
    let a = textureLoad(marched, clamp(base, vec2(0), last), 0);
    let b = textureLoad(marched, clamp(base + vec2(1, 0), vec2(0), last), 0);
    let c = textureLoad(marched, clamp(base + vec2(0, 1), vec2(0), last), 0);
    let d = textureLoad(marched, clamp(base + vec2(1, 1), vec2(0), last), 0);
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}
