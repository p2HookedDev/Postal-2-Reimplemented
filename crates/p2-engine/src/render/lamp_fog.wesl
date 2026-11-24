// Lamp-lit fog. `march` runs at half size: each view ray is marched through the stretch the picked lamps reach, from an
// offset that differs over every 4 by 4 pixels. `composite` averages one such block, each texel weighted by how near
// its depth lies to the pixel's, and adds the light to the frame.

import package::p2_engine::screen_pass::view_at;

struct FogLamp {
    // xyz the place in view space, w the fog's reach.
    place: vec4<f32>,
    // rgb linear light, w a spot's cone cosine, below -1 for a point lamp.
    color: vec4<f32>,
    along: vec4<f32>,
}

struct LampFog {
    view_from_clip: mat4x4<f32>,
    lamps: array<FogLamp, 4>,
    // x density a unit, y lamps, z steps, w the reach into the sky.
    look: vec4<f32>,
}

@group(0) @binding(0) var depth: texture_depth_2d;
@group(0) @binding(1) var<uniform> fog: LampFog;
@group(0) @binding(2) var marched: texture_2d<f32>;

// A tap whose depth is off by this share of the pixel's, squared, counts half.
const DEPTH_EASE: f32 = 0.0001;

struct FullscreenOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

fn texel_of(uv: vec2<f32>) -> vec2<i32> {
    let size = vec2<i32>(textureDimensions(depth));
    return clamp(vec2<i32>(uv * vec2<f32>(size)), vec2(0), size - 1);
}

// To the surface, or the reach into the sky, which stores depth 0.
fn ray_length(uv: vec2<f32>) -> f32 {
    let z = textureLoad(depth, texel_of(uv), 0);
    if z <= 0.0 {
        return fog.look.w;
    }
    return min(length(view_at(fog.view_from_clip, uv, z)), fog.look.w);
}

// 0 to 15, each once over a 4 by 4 block, in ordered dither order.
fn dither(pixel: vec2<u32>) -> f32 {
    let x = pixel.x & 3u;
    let y = pixel.y & 3u;
    let mixed = x ^ y;
    return f32(((mixed & 1u) << 3u) | ((y & 1u) << 2u) | (mixed & 2u) | ((y & 2u) >> 1u));
}

// As `lamp_falloff` in `lamps.rs`.
fn lamp_glow(lamp: FogLamp, p: vec3<f32>) -> vec3<f32> {
    let to_point = p - lamp.place.xyz;
    let d2 = dot(to_point, to_point);
    let reach = lamp.place.w;
    if d2 >= reach * reach {
        return vec3(0.0);
    }
    let d = sqrt(d2);
    let x = d / reach;
    var cone = 1.0;
    if lamp.color.w >= -1.0 {
        let share = saturate((dot(lamp.along.xyz, to_point / max(d, 1e-4)) - lamp.color.w) / max(1.0 - lamp.color.w, 1e-4));
        cone = share * share;
    }
    return lamp.color.rgb * ((1.0 - x * x * (3.0 - 2.0 * x)) * cone);
}

@fragment
fn march(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let way = normalize(view_at(fog.view_from_clip, in.uv, 1.0));
    let whole = ray_length(in.uv);
    let count = u32(fog.look.y);
    var start = whole;
    var end = 0.0;
    for (var i = 0u; i < count; i++) {
        let lamp = fog.lamps[i];
        let along = dot(way, lamp.place.xyz);
        let gap = along * along - dot(lamp.place.xyz, lamp.place.xyz) + lamp.place.w * lamp.place.w;
        if gap > 0.0 {
            let chord = sqrt(gap);
            start = min(start, max(along - chord, 0.0));
            end = max(end, min(along + chord, whole));
        }
    }
    var sum = vec3(0.0);
    if end > start {
        let steps = u32(fog.look.z);
        let stride = (end - start) / f32(steps);
        var t = start + stride * (dither(vec2<u32>(in.position.xy)) + 0.5) / 16.0;
        for (var s = 0u; s < steps; s++) {
            let p = way * t;
            for (var i = 0u; i < count; i++) {
                sum += lamp_glow(fog.lamps[i], p);
            }
            t += stride;
        }
        sum *= stride * fog.look.x;
    }
    return vec4(sum, whole);
}

@fragment
fn composite(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let here = ray_length(in.uv);
    let small = vec2<i32>(textureDimensions(marched));
    let corner = vec2<i32>(in.uv * vec2<f32>(small)) - 2;
    var sum = vec3(0.0);
    var weight = 0.0;
    for (var y = 0; y < 4; y++) {
        for (var x = 0; x < 4; x++) {
            let tap = textureLoad(marched, clamp(corner + vec2(x, y), vec2(0), small - 1), 0);
            let apart = (tap.a - here) / max(here, 1.0);
            let w = 1.0 / (DEPTH_EASE + apart * apart);
            sum += tap.rgb * w;
            weight += w;
        }
    }
    return vec4(sum / weight, 0.0);
}
