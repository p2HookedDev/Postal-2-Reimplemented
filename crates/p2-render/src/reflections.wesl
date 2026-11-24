// Screen-space reflections over the finished frame. The normal prepass's alpha holds a reflecting class in its two
// bits: 0 tile or glass, 1 metal, 2 wood, 3 (the default prepass's 1) nothing.

import package::p2_render::screen_pass::{gradient_noise, texel_at, view_at};

struct Reflections {
    clip_from_view: mat4x4<f32>,
    view_from_clip: mat4x4<f32>,
    view_from_world: mat4x4<f32>,
    // x strength, y stride in pixels per step, z steps, w the materials level.
    look: vec4<f32>,
    // In view space: xyz the centre and w the radius of each puddle; `puddle_count.x` of them are set.
    puddles: array<vec4<f32>, 16>,
    puddle_count: vec4<f32>,
    // The view's part of the frame as shares of it, corner in xy and size in zw: a viewport uv times zw plus xy is the
    // frame's uv.
    in_frame: vec4<f32>,
    // A frame uv times zw plus xy is the viewport's uv.
    from_frame: vec4<f32>,
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

// The default class's roughness and reflectance, and the materials level that reaches a class. Match `modern.wesl` and `modern_base.wesl`.
const DEFAULT_ROUGHNESS: f32 = 0.85;
const DEFAULT_REFLECTANCE: f32 = 0.04;
const MATERIALS_TOP: f32 = 10.0;

// x roughness, y reflectance, z how far a metal, moved from the default surface by the materials level as the lit
// shader moves it.
fn surface_of_code(code: u32) -> vec3<f32> {
    let scale = min(reflections.look.w / MATERIALS_TOP, 1.0);
    let picked = class_surface(code);
    return vec3(mix(DEFAULT_ROUGHNESS, picked.x, scale), mix(DEFAULT_REFLECTANCE, picked.y, scale), picked.z * scale);
}

// x roughness, y reflectance, z 1 on a metal. Matches `material_class.rs`.
fn class_surface(code: u32) -> vec3<f32> {
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

// A liquid's roughness and reflectance: it mirrors at a low angle and hardly at all from above.
const PUDDLE_SURFACE: vec3<f32> = vec3(0.1, 0.04, 0.0);
// How far off a puddle's plane a pixel still lies in it, in world units.
const PUDDLE_DEPTH: f32 = 4.0;
// A lit pool's roughness and reflectance, under the gloss the lit pass records in the frame's alpha.
const POOL_SURFACE: vec3<f32> = vec3(0.1, 0.02, 0.0);
// Matches projector.wesl.
const BLOOD_SHEEN: vec3<f32> = vec3(1.0, 0.3, 0.3);
const LEAST_RECORD: f32 = 1.0 / 256.0;

// 1 on a puddle, falling to 0 inside the sprite's uneven rim; only ground counts, never a wall that stands in the disc.
fn wetness(at: vec3<f32>, normal: vec3<f32>) -> f32 {
    let ground = smoothstep(0.7, 0.9, dot(normal, normalize(reflections.view_from_world[1].xyz)));
    if ground <= 0.0 {
        return 0.0;
    }
    var wet = 0.0;
    let count = u32(reflections.puddle_count.x);
    for (var i = 0u; i < count; i++) {
        let puddle = reflections.puddles[i];
        let off = at - puddle.xyz;
        let height = dot(off, normal);
        if abs(height) < PUDDLE_DEPTH {
            wet = max(wet, 1.0 - smoothstep(0.55 * puddle.w, 0.8 * puddle.w, length(off - normal * height)));
        }
    }
    return wet * ground;
}

fn uv_of(p: vec3<f32>) -> vec2<f32> {
    let c = reflections.clip_from_view * vec4(p, 1.0);
    let ndc = c.xy / c.w;
    return vec2(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
}

fn depth_at(uv: vec2<f32>, viewport: vec4<f32>, size: vec2<f32>) -> f32 {
    let texel = clamp(texel_at(uv, viewport), vec2(0), vec2<i32>(size) - 1);
    return textureLoad(depth, texel, 0);
}

// The sky stores depth 0: no ray is ever behind it.
fn surface_z(uv: vec2<f32>, viewport: vec4<f32>, size: vec2<f32>) -> f32 {
    let z = depth_at(uv, viewport, size);
    if z <= 0.0 {
        return -1e9;
    }
    return view_at(reflections.view_from_clip, uv, z).z;
}

// The hit's uv and its weight, 0 on a miss.
fn march(origin: vec3<f32>, way: vec3<f32>, viewport: vec4<f32>, size: vec2<f32>, pixel: vec2<f32>) -> vec3<f32> {
    let start = uv_of(origin);
    let pixels_per_unit = max(length((uv_of(origin + way) - start) * viewport.zw), 1e-3);
    let stride = PIXELS_PER_STEP * reflections.look.y / pixels_per_unit;
    let steps = i32(reflections.look.z);
    var reach = 1e9;
    if way.z > 0.0 {
        reach = max(-origin.z - 1.0, 0.0) / way.z;
    }
    var last = 0.0;
    var t = stride * (0.5 + gradient_noise(pixel));
    for (var i = 0; i < steps; i++) {
        if t > reach {
            break;
        }
        let at = origin + way * t;
        let uv = uv_of(at);
        if any(uv < vec2(0.0)) || any(uv > vec2(1.0)) {
            break;
        }
        let behind = surface_z(uv, viewport, size) - at.z;
        if behind > 0.0 && behind < stride * THICKNESS {
            var near = last;
            var far = t;
            for (var j = 0; j < REFINE_STEPS; j++) {
                let mid = 0.5 * (near + far);
                let m = origin + way * mid;
                if surface_z(uv_of(m), viewport, size) > m.z {
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
    var surface = surface_of_code(u32(round(packed.a * 3.0)));
    // The pass covers the whole frame so the bars outside the viewport, whose depth is 0, are copied through.
    let viewport = reflections.in_frame * vec4(size, size);
    let uv = in.uv * reflections.from_frame.zw + reflections.from_frame.xy;
    let z = depth_at(uv, viewport, size);
    if z <= 0.0 {
        return here;
    }
    // A lit pool wrote alpha 1 plus its gloss, negative on blood; the sun rays read alpha later as sky 0, world 1.
    let record = here.a - 1.0;
    let mirror = select(0.0, abs(record), abs(record) >= LEAST_RECORD);
    let unmarked = vec4(here.rgb, 1.0);
    if surface.x >= ROUGH_TO && reflections.puddle_count.x == 0.0 && mirror == 0.0 {
        return unmarked;
    }
    let origin = view_at(reflections.view_from_clip, uv, z);
    // A pool on nearly level ground mirrors as level: the prepass's 10-bit normal of the ground's gentle rolls would
    // step the mirror in bands. A slope keeps its own normal, as the lit pass lights it.
    let ground = packed.xyz * 2.0 - 1.0;
    let level = select(ground, vec3(0.0, 1.0, 0.0), mirror > 0.0 && ground.y > 0.98);
    let normal = normalize((reflections.view_from_world * vec4(level, 0.0)).xyz);
    surface = mix(surface, PUDDLE_SURFACE, wetness(origin, normal));
    var glossy = 1.0 - smoothstep(ROUGH_FROM, ROUGH_TO, surface.x);
    var pool_tint = vec3(1.0);
    if mirror > 0.0 {
        // The liquid comes in by its record, so its rim leaves no ring, and never mirrors less than the floor under it.
        let wet = mix(surface, POOL_SURFACE, mirror);
        surface = vec3(wet.x, max(surface.y, wet.y), wet.z);
        glossy = max(glossy, mirror);
        pool_tint = mix(vec3(1.0), select(vec3(1.0), BLOOD_SHEEN, record < 0.0), mirror);
    }
    if glossy <= 0.0 {
        return unmarked;
    }
    let eye = normalize(-origin);
    // A ray that meets nothing on screen shows nothing: no stand-in light is put in its place.
    let hit = march(origin, reflect(-eye, normal), viewport, size, in.position.xy);
    if hit.z <= 0.0 {
        return unmarked;
    }
    let seen = blurred(hit.xy * reflections.in_frame.zw + reflections.in_frame.xy, surface.x, size);
    let fresnel = surface.y + (1.0 - surface.y) * pow(1.0 - saturate(dot(normal, eye)), 5.0);
    let tint = mix(vec3(1.0), here.rgb / max(max(here.r, max(here.g, here.b)), 1e-3), surface.z) * pool_tint;
    if mirror > 0.0 {
        // A liquid is a mirror over what shows through it: the room takes its Fresnel share, darker or brighter.
        return vec4(mix(here.rgb, seen * tint, saturate(fresnel * mirror * hit.z * reflections.look.x)), 1.0);
    }
    // The lit pass already gave the surface its light: a reflection shows only where it is brighter, and adds none.
    return vec4(max(here.rgb, seen * hit.z * tint * (fresnel * glossy * reflections.look.x)), 1.0);
}
