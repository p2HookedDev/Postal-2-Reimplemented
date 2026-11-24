// Drops in the air as quads facing the eye: a falling drop as a teardrop stretched along its motion. A ragged liquid's
// edge wobbles by the drop's own seed, so no two read as the same dot.

import package::p2_render::liquid_look::{AirIn, AirOut, TAU, air_shade, is_puke, noise1, optics_of, pixel_units, puke_splat, puke_splat_sampler, surface_of, texture_lod, textured_surface};
import bevy_pbr::render::{
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
};

// A falling drop's streak, longest and shortest, in radii, and how its tail tapers.
const MOST_STREAK: f32 = 4.0;
const LEAST_STREAK: f32 = 0.8;
const TAIL_POW: f32 = 0.6;
// How much a ragged liquid's edge wobbles, and the width the quad leaves for it, against the radius.
const WOBBLE: f32 = 0.4;
const ROOM: f32 = 1.25;

// `drop_record` in `liquids/air_draw.rs`. The first record is a header holding only `count`.
struct DropRecord {
    center: vec3<f32>,
    count: u32,
    way: vec3<f32>,
    seed: f32,
    light: vec4<f32>,
    look: vec4<f32>,
}

@group(3) @binding(15) var<storage, read> drops: array<DropRecord>;

// A grid vertex: its quad and its corner.
struct GridIn {
    @location(0) position: vec3<f32>,
}

@vertex
fn vertex(grid: GridIn) -> AirOut {
    let quad = u32(grid.position.x + 0.5);
    let corner = u32(grid.position.y + 0.5);
    if quad >= drops[0].count {
        var out: AirOut;
        out.position = vec4(2.0, 2.0, 2.0, 1.0);
        return out;
    }
    let record = drops[quad + 1u];
    let corners = vec2(select(-1.0, 1.0, corner == 1u || corner == 2u), select(-1.0, 1.0, corner >= 2u));
    let in = AirIn(record.center, record.way, corners, vec4(record.light.rgb, record.seed), record.look, vec4(0.0));
    let radius = max(in.look.x, pixel_units(in.center));
    // The axis runs from the tail to the front, back along the fall.
    let speed = length(in.way);
    let toward = select(vec3(0.0, -1.0, 0.0), in.way / max(speed, 1e-6), speed > 1e-6);
    let axis = toward * clamp(speed, LEAST_STREAK * in.look.x, MOST_STREAK * in.look.x);
    let to_eye = normalize(view.world_position.xyz - in.center);
    let flat = axis - to_eye * dot(axis, to_eye);
    let up = normalize(select(flat, cross(to_eye, vec3(1.0, 0.0, 0.0)), dot(flat, flat) < 1e-8));
    let side = normalize(cross(up, to_eye));
    let half_length = length(flat) * 0.5;
    var out: AirOut;
    out.half = vec2(radius, half_length);
    // Half a pixel more each way, so the edge's ramp centred on the edge is never clipped.
    let edge = 0.5 * pixel_units(in.center);
    out.local = vec2(in.corner.x * (radius * ROOM + edge), in.corner.y * (half_length + radius + edge));
    let world = in.center - axis * 0.5 + side * out.local.x + up * out.local.y;
    out.side = side;
    out.up = up;
    out.position = position_world_to_clip(world);
    out.world = world;
    out.light = in.light.rgb;
    out.look = in.look;
    out.seed = in.light.w;
    return out;
}

@fragment
fn fragment(in: AirOut) -> @location(0) vec4<f32> {
    let pixel = pixel_units(in.world);
    let radius = max(in.look.x, pixel);
    // A drop smaller than a pixel is drawn one pixel in radius, as faint as its real area, so far drops do not flicker.
    let area = min(1.0, in.look.x / pixel);
    let to_eye = normalize(view.world_position.xyz - in.world);
    let seed = in.seed;
    let s = in.local.y;
    let x = in.local.x;
    var w: f32;
    // Minus the width times its slope along the axis: how far the normal tilts toward the front.
    var lean: f32;
    if s >= in.half.y {
        let ahead = s - in.half.y;
        w = sqrt(max(radius * radius - ahead * ahead, 0.0));
        lean = ahead;
    } else {
        let span = 2.0 * in.half.y + radius;
        let t = max((s + in.half.y + radius) / span, 1e-4);
        w = radius * pow(t, TAIL_POW);
        lean = -radius * radius * TAIL_POW * pow(t, 2.0 * TAIL_POW - 1.0) / span;
    }
    let right = select(0.0, 9.0, x > 0.0);
    w *= 1.0 + optics_of(in.look.y).ragged * WOBBLE * (noise1(s / radius * 1.3 + seed * 41.0 + right) - 0.5);
    // The ramp is centred on the edge, so a drawn disc adds up to its whole area.
    let cover = saturate((w - abs(x)) / pixel + 0.5) * area * area;
    if cover <= 0.0 {
        discard;
    }
    let z = sqrt(max(w * w - x * x, 0.0));
    let n = normalize(in.side * x + in.up * lean + to_eye * max(z, 1e-4));
    var surface = surface_of(in.look.y, in.local / radius + vec2(seed * 31.0));
    // A puke drop is coloured by the game's splat sprite laid over its quad, turned by its seed.
    if is_puke(in.look.y) && textureDimensions(puke_splat).x > 1u {
        let extent = vec2(radius * ROOM, in.half.y + radius);
        let turn = seed * TAU;
        let p = in.local / (2.0 * extent);
        let uv = vec2(cos(turn) * p.x - sin(turn) * p.y, sin(turn) * p.x + cos(turn) * p.y) + 0.5;
        let side = vec2<f32>(textureDimensions(puke_splat));
        let lod = texture_lod(max(side.x, side.y), 2.0 * min(extent.x, extent.y), pixel);
        let texel = textureSampleLevel(puke_splat, puke_splat_sampler, fract(uv), lod);
        surface = textured_surface(surface, in.look.y, texel.rgb, smoothstep(0.0, 0.3, texel.a));
    }
    return air_shade(in.world, n, 2.0 * z, in.look, in.light, in.position, cover, 0.0, surface);
}
