// Screen-space bounce light. `gather` runs at half size: each pixel walks a few screen directions through the depth
// prepass, and a bitmask over the hemisphere's angles in each direction keeps a surface hidden behind a nearer one
// from adding its light. `composite` runs at full size and blurs that along a cross, weighted by like depth and facing.

import bevy_render::view::View;

struct FullscreenOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// The lit scene for `gather`, the gathered light for `composite`.
@group(0) @binding(0) var lit: texture_2d<f32>;
@group(0) @binding(1) var depth: texture_depth_2d;
@group(0) @binding(2) var normals: texture_2d<f32>;
@group(0) @binding(3) var<uniform> view: View;

const PI: f32 = 3.14159265;
const HALF_PI: f32 = 1.57079633;
const SLICES: u32 = 2u;
const STEPS: u32 = 6u;
const SECTORS: f32 = 32.0;
// In Unreal units (about 2 cm each), tuned by eye later (unverified): how far light bounces, how thick a surface is.
const RADIUS: f32 = 192.0;
const THICKNESS: f32 = 32.0;
const MAX_REACH_PIXELS: f32 = 192.0;
const DEPTH_LIKENESS: f32 = 0.05;
const NORMAL_LIKENESS: f32 = 16.0;

fn view_position(texel: vec2<i32>, z: f32) -> vec3<f32> {
    let uv = (vec2<f32>(texel) + 0.5) / vec2<f32>(textureDimensions(depth));
    let p = view.view_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return p.xyz / p.w;
}

fn world_normal(texel: vec2<i32>) -> vec3<f32> {
    return normalize(textureLoad(normals, texel, 0).xyz * 2.0 - 1.0);
}

fn view_normal(texel: vec2<i32>) -> vec3<f32> {
    return normalize((view.view_from_world * vec4(world_normal(texel), 0.0)).xyz);
}

// A 4 by 4 ordered pattern, 0 to 15, so the pixels of a block walk different directions.
fn pattern(pixel: vec2<u32>) -> u32 {
    let x = pixel.x & 3u;
    let y = pixel.y & 3u;
    let a = x ^ y;
    return ((a & 1u) << 3u) | ((y & 1u) << 2u) | (a & 2u) | ((y & 2u) >> 1u);
}

// The sectors from `lo` to `hi`, fractions of the hemisphere's half circle around the normal.
fn sector_bits(lo: f32, hi: f32) -> u32 {
    let start = u32(clamp(lo * SECTORS, 0.0, SECTORS));
    let count = min(u32(clamp(ceil((hi - lo) * SECTORS), 0.0, SECTORS)), 32u - start);
    if count == 0u {
        return 0u;
    }
    return (0xffffffffu >> (32u - count)) << start;
}

@fragment
fn gather(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<u32>(in.position.xy);
    let size = vec2<i32>(textureDimensions(depth));
    let texel = min(vec2<i32>(pixel * 2u), size - 1);
    let z = textureLoad(depth, texel, 0);
    if z <= 0.0 {
        return vec4(0.0);
    }
    let here = view_position(texel, z);
    let normal = view_normal(texel);
    let eye = normalize(-here);
    let reach = min(RADIUS * view.clip_from_view[1][1] * 0.5 * f32(size.y) / -here.z, MAX_REACH_PIXELS);
    if reach < 2.0 {
        return vec4(0.0);
    }
    let turn = pattern(pixel);
    let jitter = f32((turn * 7u + 5u) & 15u) / 16.0 + 1.0 / 32.0;
    var light = vec3(0.0);
    for (var s = 0u; s < SLICES; s++) {
        let angle = (f32(s) + f32(turn) / 16.0) * PI / f32(SLICES);
        let along = vec2(cos(angle), sin(angle));
        let way = vec3(along.x, -along.y, 0.0);
        let axis = normalize(cross(way, eye));
        let projected = normal - axis * dot(normal, axis);
        let ortho = way - eye * dot(way, eye);
        let tilt = sign(dot(ortho, projected)) * acos(saturate(dot(projected, eye) / max(length(projected), 1e-4)));
        var occluded = 0u;
        for (var k = 0u; k < STEPS; k++) {
            let t = (f32(k) + jitter) / f32(STEPS);
            let pixels = max(reach * t * t, 1.5);
            for (var side = -1.0; side <= 1.0; side += 2.0) {
                let at = texel + vec2<i32>(round(along * side * pixels));
                if any(at < vec2(0)) || any(at >= size) {
                    continue;
                }
                let at_z = textureLoad(depth, at, 0);
                if at_z <= 0.0 {
                    continue;
                }
                let delta = view_position(at, at_z) - here;
                let gap = length(delta);
                if gap > RADIUS || gap < 1e-3 {
                    continue;
                }
                let toward = delta / gap;
                let front = side * acos(clamp(dot(toward, eye), -1.0, 1.0));
                let back = side * acos(clamp(dot(normalize(delta - eye * THICKNESS), eye), -1.0, 1.0));
                let a = (front - tilt + HALF_PI) / PI;
                let b = (back - tilt + HALF_PI) / PI;
                let bits = sector_bits(saturate(min(a, b)), saturate(max(a, b)));
                let fresh = bits & ~occluded;
                occluded |= bits;
                if fresh == 0u {
                    continue;
                }
                let facing = saturate(dot(view_normal(at), -toward)) * saturate(dot(normal, toward));
                light += textureLoad(lit, at, 0).rgb * (f32(countOneBits(fresh)) / SECTORS * facing);
            }
        }
    }
    return vec4(max(light / f32(SLICES), vec3(0.0)), 1.0);
}

fn tap(centre: vec2<i32>, offset: vec2<i32>, here_z: f32, normal: vec3<f32>) -> vec4<f32> {
    let at = clamp(centre + offset, vec2(0), vec2<i32>(textureDimensions(lit)) - 1);
    let full = min(at * 2, vec2<i32>(textureDimensions(depth)) - 1);
    let z = textureLoad(depth, full, 0);
    if z <= 0.0 {
        return vec4(0.0);
    }
    let like_depth = exp(-abs(view_position(full, z).z - here_z) / (DEPTH_LIKENESS * abs(here_z)));
    let like_facing = pow(max(dot(normal, world_normal(full)), 1e-4), NORMAL_LIKENESS);
    let weight = like_depth * like_facing;
    return vec4(textureLoad(lit, at, 0).rgb * weight, weight);
}

// Alpha 0: the blend keeps the frame's own alpha.
@fragment
fn composite(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let texel = min(vec2<i32>(in.position.xy), vec2<i32>(textureDimensions(depth)) - 1);
    let z = textureLoad(depth, texel, 0);
    if z <= 0.0 {
        return vec4(0.0);
    }
    let here_z = view_position(texel, z).z;
    let normal = world_normal(texel);
    let centre = texel / 2;
    var sum = tap(centre, vec2(0), here_z, normal);
    for (var i = 1; i <= 2; i++) {
        sum += tap(centre, vec2(i, 0), here_z, normal) + tap(centre, vec2(-i, 0), here_z, normal);
        sum += tap(centre, vec2(0, i), here_z, normal) + tap(centre, vec2(0, -i), here_z, normal);
    }
    if sum.w < 1e-4 {
        return vec4(0.0);
    }
    return vec4(sum.rgb / sum.w, 0.0);
}
