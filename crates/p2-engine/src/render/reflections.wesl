// Screen-space reflections over the finished frame. The normal prepass's alpha holds a reflecting class in its two
// bits: 0 tile or glass, 1 metal, 2 wood, 3 (the default prepass's 1) nothing.

import package::p2_engine::screen_pass::view_at;

struct Reflections {
    clip_from_view: mat4x4<f32>,
    view_from_clip: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    // x strength, y stride in pixels per step, z steps.
    look: vec4<f32>,
    fallback: vec4<f32>,
}

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var scene_sampler: sampler;
@group(0) @binding(2) var depth: texture_depth_2d;
@group(0) @binding(3) var normals: texture_2d<f32>;
@group(0) @binding(4) var<uniform> reflections: Reflections;

const PIXELS_PER_STEP: f32 = 6.0;
const REFINE_STEPS: i32 = 5;
const EDGE_FADE: f32 = 0.08;
// How far behind a surface a ray still counts as hitting it, in steps.
const THICKNESS: f32 = 1.5;
const ROUGH_FROM: f32 = 0.2;
const ROUGH_TO: f32 = 0.5;
const BLUR_PIXELS: f32 = 12.0;

struct FullscreenOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// x roughness, y reflectance, z 1 on a metal. Matches `material_class.rs`.
fn surface_of_code(code: u32) -> vec3<f32> {
    switch code {
        case 0u: {
            return vec3(0.15, 0.05, 0.0);
        }
        case 1u: {
            return vec3(0.35, 1.0, 1.0);
        }
        case 2u: {
            return vec3(0.6, 0.04, 0.0);
        }
        default: {
            return vec3(1.0, 0.0, 0.0);
        }
    }
}

fn noise(at: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(at, vec2(0.06711056, 0.00583715))));
}

fn uv_of(p: vec3<f32>) -> vec2<f32> {
    let c = reflections.clip_from_view * vec4(p, 1.0);
    let ndc = c.xy / c.w;
    return vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
}

fn depth_at(uv: vec2<f32>, size: vec2<f32>) -> f32 {
    let texel = clamp(vec2<i32>(uv * size), vec2(0), vec2<i32>(size) - 1);
    return textureLoad(depth, texel, 0);
}

// The sky stores depth 0: no ray is ever behind it.
fn surface_z(uv: vec2<f32>, size: vec2<f32>) -> f32 {
    let z = depth_at(uv, size);
    if z <= 0.0 {
        return -1e9;
    }
    return view_at(reflections.view_from_clip, uv, z).z;
}

// The hit's uv and its weight, 0 on a miss.
fn march(origin: vec3<f32>, way: vec3<f32>, size: vec2<f32>, pixel: vec2<f32>) -> vec3<f32> {
    let start = uv_of(origin);
    let pixels_per_unit = max(length((uv_of(origin + way) - start) * size), 1e-3);
    let stride = PIXELS_PER_STEP * reflections.look.y / pixels_per_unit;
    let steps = i32(reflections.look.z);
    var reach = 1e9;
    if way.z > 0.0 {
        reach = max(-origin.z - 1.0, 0.0) / way.z;
    }
    var last = 0.0;
    var t = stride * (0.5 + noise(pixel));
    for (var i = 0; i < steps; i++) {
        if t > reach {
            break;
        }
        let at = origin + way * t;
        let uv = uv_of(at);
        if any(uv < vec2(0.0)) || any(uv > vec2(1.0)) {
            break;
        }
        let behind = surface_z(uv, size) - at.z;
        if behind > 0.0 && behind < stride * THICKNESS {
            var near = last;
            var far = t;
            for (var j = 0; j < REFINE_STEPS; j++) {
                let mid = 0.5 * (near + far);
                let m = origin + way * mid;
                if surface_z(uv_of(m), size) > m.z {
                    far = mid;
                } else {
                    near = mid;
                }
            }
            let found = uv_of(origin + way * far);
            let edge = min(min(found.x, 1.0 - found.x), min(found.y, 1.0 - found.y));
            return vec3(found, smoothstep(0.0, EDGE_FADE, edge) * (1.0 - f32(i) / f32(steps)));
        }
        last = t;
        t += stride;
    }
    return vec3(0.0);
}

fn blurred(uv: vec2<f32>, rough: f32, size: vec2<f32>) -> vec3<f32> {
    let spread = rough * BLUR_PIXELS / size;
    var sum = textureSampleLevel(scene, scene_sampler, uv, 0.0).rgb;
    sum += textureSampleLevel(scene, scene_sampler, uv + vec2(spread.x, 0.0), 0.0).rgb;
    sum += textureSampleLevel(scene, scene_sampler, uv - vec2(spread.x, 0.0), 0.0).rgb;
    sum += textureSampleLevel(scene, scene_sampler, uv + vec2(0.0, spread.y), 0.0).rgb;
    sum += textureSampleLevel(scene, scene_sampler, uv - vec2(0.0, spread.y), 0.0).rgb;
    return sum / 5.0;
}

@fragment
fn fragment(in: FullscreenOutput) -> @location(0) vec4<f32> {
    let here = textureSampleLevel(scene, scene_sampler, in.uv, 0.0);
    let size = vec2<f32>(textureDimensions(depth));
    let packed = textureLoad(normals, clamp(vec2<i32>(in.position.xy), vec2(0), vec2<i32>(size) - 1), 0);
    let surface = surface_of_code(u32(round(packed.a * 3.0)));
    let glossy = 1.0 - smoothstep(ROUGH_FROM, ROUGH_TO, surface.x);
    let z = depth_at(in.uv, size);
    if glossy <= 0.0 || z <= 0.0 {
        return here;
    }
    let origin = view_at(reflections.view_from_clip, in.uv, z);
    let normal = normalize((reflections.view_from_world * vec4(packed.xyz * 2.0 - 1.0, 0.0)).xyz);
    let eye = normalize(-origin);
    var hit = vec3(0.0);
    if reflections.look.z > 0.0 {
        hit = march(origin, reflect(-eye, normal), size, in.position.xy);
    }
    // A ray that fades out at the screen's edge or meets nothing shows the zone's ambient in its place.
    var found = reflections.fallback.rgb;
    if hit.z > 0.0 {
        found = mix(found, blurred(hit.xy, surface.x, size), hit.z);
    }
    let fresnel = surface.y + (1.0 - surface.y) * pow(1.0 - saturate(dot(normal, eye)), 5.0);
    let tint = select(vec3(1.0), here.rgb / max(max(here.r, max(here.g, here.b)), 1e-3), surface.z > 0.5);
    return vec4(here.rgb + found * tint * (fresnel * glossy * reflections.look.x), here.a);
}
