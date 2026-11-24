// A stream. A thin jet is an eight-sided shell around its arc, shaded as the round jet inside so it keeps its
// shape from any side. A paste is a torn, lumpy sheet of ropes widened to face the eye. Every shape is fixed to
// the moving liquid, so it runs down the stream with the flow.

import package::p2_render::liquid_look::{AirIn, AirOut, SHAPE_TUBE, TAU, air_shade, hash1, is_puke, noise1, noise2, optics_of, pixel_units, puke_pour, puke_pour_sampler, surface_of, texture_lod, textured_surface};
import bevy_pbr::render::{
    mesh_view_bindings::{view, globals},
    view_transformations::position_world_to_clip,
};

// How far the shell stands out of the round jet: an octagon's sides cut a circle by 1 / cos(TAU / 16).
const TUBE_ROOM: f32 = 1.09;
const BEADS_FROM: f32 = 0.85;
const BEADS_FULL: f32 = 0.9;
const BEAD_RADII: f32 = 9.0;
const BEAD_SHARE: f32 = 0.55;
const BEAD_PEAK: f32 = 1.35;
const NECK: f32 = 0.35;
const BEAD_MEAN: f32 = 0.75;
const RIPPLE: f32 = 0.12;
const RIPPLE_RADII: f32 = 14.0;
const END_ON: f32 = 0.12;

const FLOW_SPEED: f32 = 60.0;
const TEAR_START: f32 = 0.3;
const STRAND_WIDTH: f32 = 0.45;
const DIVERGE: f32 = 3.0;
const WANDER: f32 = 1.5;
const WANDER_RADII: f32 = 30.0;
const HOLES: f32 = 0.6;
const BUMPS: f32 = 0.5;
const ROPES_FROM: f32 = 0.7;
// The game's puke pour strip runs across the stream and repeats every this many radii along it (unverified: its
// orientation in the retail strip).
const POUR_RADII: f32 = 8.0;

fn blurred(speed: f32, units: f32) -> f32 {
    return smoothstep(0.25, 0.5, speed * globals.delta_time / max(units, 1e-4));
}

fn tube_vertex(in: AirIn) -> AirOut {
    let share = in.light.w;
    let r = length(in.way);
    let radial = in.way / max(r, 1e-6);
    let room = TUBE_ROOM * mix(1.0, BEAD_PEAK, smoothstep(BEADS_FROM - 0.1, BEADS_FROM, share));
    let world = in.center + radial * max(r * room, 1.5 * pixel_units(in.center));
    var out: AirOut;
    out.position = position_world_to_clip(world);
    out.world = world;
    out.side = radial;
    out.up = in.axis.xyz;
    out.local = vec2(r, in.corner.y);
    out.light = in.light.rgb;
    out.look = in.look;
    out.half = vec2(share, 0.0);
    out.axis = vec4(in.center, in.axis.w);
    return out;
}

fn tube_fragment(in: AirOut, front: bool) -> vec4<f32> {
    if !front {
        discard;
    }
    let along_axis = normalize(in.up);
    let to_eye = normalize(view.world_position.xyz - in.world);
    let across_eye = to_eye - along_axis * dot(to_eye, along_axis);
    let seen_side = length(across_eye);
    var s = cross(along_axis, to_eye);
    if dot(s, s) < 1e-8 {
        s = in.side;
    }
    s = normalize(s);
    let x = dot(in.world - in.axis.xyz, s);
    let r0 = in.look.x;
    let speed = in.axis.w;
    let share = saturate(in.half.x);
    let flow = in.local.y - globals.time * speed;
    let gap = BEAD_RADII * r0;
    let phase = (fract(flow / gap) - 0.5) / (0.5 * BEAD_SHARE);
    let neck = NECK * (1.0 - smoothstep(0.93, 0.99, share));
    let beaded = mix(max(BEAD_PEAK * sqrt(max(1.0 - phase * phase, 0.0)), neck), BEAD_MEAN, blurred(speed, gap));
    let r = in.local.x * mix(1.0, beaded, smoothstep(BEADS_FROM, BEADS_FULL, share)) * saturate((1.0 - share) * 40.0);
    let pixel = pixel_units(in.world);
    let drawn = max(r, pixel);
    // A jet thinner than a pixel is drawn one pixel wide, as faint as its width.
    let cover = saturate((drawn - abs(x)) / pixel) * min(1.0, r / pixel);
    if cover <= 0.0 {
        discard;
    }
    let a = clamp(x / drawn, -1.0, 1.0);
    let h = sqrt(1.0 - a * a);
    let facing = select(to_eye, across_eye / max(seen_side, 1e-6), seen_side > 1e-4);
    let wave = RIPPLE_RADII * r0;
    let ripple = RIPPLE * (1.0 - blurred(speed, wave)) * (0.6 * sin(TAU * flow / wave) + 0.4 * sin(TAU * flow / (0.43 * wave) + 1.7));
    let n = normalize(s * a + facing * h + along_axis * (ripple * h));
    let thickness = 2.0 * r * h / max(seen_side, END_ON);
    let surface = surface_of(in.look.y, vec2(x, flow) / max(r0, 1e-4));
    return air_shade(in.world, n, thickness, in.look, in.light, in.position, cover, in.look.w, surface);
}

fn strand_radius(radius: f32, strand: f32) -> f32 {
    return select(radius * STRAND_WIDTH * (0.75 + 0.5 * hash1(strand)), radius, strand < 0.5);
}

fn strand_swell(grown: f32, strand: f32) -> f32 {
    return select(sqrt(grown), grown, strand < 0.5);
}

fn widest(ragged: f32) -> f32 {
    return 1.15 * (1.0 + 0.8 * ragged);
}

fn sheet_vertex(in: AirIn) -> AirOut {
    let strand = abs(in.corner.x) - 1.0;
    let edge = sign(in.corner.x);
    let share = in.light.w;
    let to_eye = normalize(view.world_position.xyz - in.center);
    let length_of_way = length(in.way);
    let grown = select(1.0, length_of_way, length_of_way > 1e-6);
    let way = in.way / grown;
    var side = cross(way, to_eye);
    if dot(side, side) < 1e-8 {
        side = cross(vec3(0.0, 1.0, 0.0), to_eye);
    }
    side = normalize(side);
    let flow = in.corner.y - globals.time * FLOW_SPEED;
    let lean = select(-1.0, 1.0, (u32(strand) & 1u) == 1u) * (0.6 + 0.8 * hash1(strand + 0.5));
    let wander = noise1(flow / (in.look.x * WANDER_RADII) + strand * 5.0) - 0.5;
    let drift = select(0.0, in.look.x * grown * (DIVERGE * lean * share * share + WANDER * wander * share), strand > 0.5);
    let center = in.center + side * drift;
    let half = max(strand_radius(in.look.x, strand) * strand_swell(grown, strand) * widest(optics_of(in.look.y).ragged), pixel_units(center));
    let world = center + side * (edge * half);
    var out: AirOut;
    out.position = position_world_to_clip(world);
    out.world = world;
    out.side = side;
    out.up = way;
    out.local = vec2(edge * half, in.corner.y);
    out.light = in.light.rgb;
    out.look = in.look;
    out.half = vec2(share, grown);
    out.seed = strand;
    return out;
}

fn sheet_fragment(in: AirOut) -> vec4<f32> {
    let ragged = optics_of(in.look.y).ragged;
    let strand = in.seed;
    let share = saturate(in.half.x);
    let radius = strand_radius(in.look.x, strand);
    let flow = in.local.y - globals.time * FLOW_SPEED;
    let along = flow / radius + strand * 37.0;
    let x = in.local.x;
    let across = x / radius;
    let right = select(0.0, 1.0, x > 0.0);
    let wobble = 0.65 * noise1(along * 0.3 + right * 19.0) + 0.35 * noise1(along * 1.1 + right * 7.0);
    let grown = in.half.y;
    let r = radius * strand_swell(grown, strand) * (1.0 + ragged * (wobble - 0.5) * (0.6 + share)) * saturate((1.0 - share) * 40.0);

    let pixel = pixel_units(in.world);
    let drawn = max(r, pixel);
    var cover = saturate((drawn - abs(x)) / pixel) * min(1.0, r / pixel);
    cover *= select(1.0, smoothstep(ROPES_FROM, 1.0, grown), strand > 0.5);
    let tear = 0.65 * noise2(vec2(across * 2.2, along * 0.22)) + 0.35 * noise2(vec2(across * 5.0, along * 0.6) + 3.7);
    let eat = HOLES * (0.4 + 0.6 * ragged) * smoothstep(TEAR_START, 1.0, share);
    let edge = saturate(abs(x) / max(r, 1e-4));
    let threshold = eat * (0.4 + 0.9 * edge * edge);
    cover *= smoothstep(threshold - 0.12, threshold - 0.02, tear);
    var surface = surface_of(in.look.y, vec2(across, along));
    if is_puke(in.look.y) && textureDimensions(puke_pour).x > 1u {
        let side = vec2<f32>(textureDimensions(puke_pour));
        let lod = max(texture_lod(side.x, 2.0 * radius, pixel), texture_lod(side.y, POUR_RADII * radius, pixel));
        let texel = textureSampleLevel(puke_pour, puke_pour_sampler, vec2(saturate(x / max(r, 1e-4) * 0.5 + 0.5), fract(along / POUR_RADII)), lod);
        cover *= smoothstep(0.05, 0.5, texel.a);
        surface = textured_surface(surface, in.look.y, texel.rgb, 1.0);
    }
    if cover <= 0.0 {
        discard;
    }
    let a = clamp(x / drawn, -1.0, 1.0);
    let h = sqrt(1.0 - a * a);
    let to_eye = normalize(view.world_position.xyz - in.world);
    let bumped = vec2(across * 1.6, along * 0.8);
    let bump = vec2(noise2(bumped + 5.3), noise2(bumped + 9.1)) - 0.5;
    let n = normalize(in.side * a + to_eye * h + (in.side * bump.x + in.up * bump.y) * (BUMPS * (0.3 + ragged) * h));
    return air_shade(in.world, n, 2.0 * r * h, in.look, in.light, in.position, cover, in.look.w, surface);
}

@vertex
fn vertex(in: AirIn) -> AirOut {
    if abs(in.look.z - SHAPE_TUBE) < 0.5 {
        return tube_vertex(in);
    }
    return sheet_vertex(in);
}

@fragment
fn fragment(in: AirOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    if abs(in.look.z - SHAPE_TUBE) < 0.5 {
        return tube_fragment(in, front);
    }
    return sheet_fragment(in);
}
