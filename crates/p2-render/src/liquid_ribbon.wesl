// A stream's ribbon, widened to face the eye and traced as a round tube across its width: true normals, and the
// thickness light passes through. Its swell and its lumps run down the stream with the flow.

import package::p2_render::liquid_look::{AirIn, AirOut, TAU, air_shade, pixel_units};
import bevy_pbr::render::{
    mesh_view_bindings::{view, globals},
    view_transformations::position_world_to_clip,
};

// The varicose swell's depth and its wavelength in radii, the flow's speed in Unreal units a second, and how much
// lumps vary the thickness.
const SWELL: f32 = 0.18;
const SWELL_RADII: f32 = 9.0;
const FLOW_SPEED: f32 = 60.0;
const LUMPS: f32 = 0.25;

fn hash(x: f32) -> f32 {
    return fract(sin(x * 12.9898) * 43758.5453);
}

fn flow_noise(x: f32) -> f32 {
    let cell = floor(x);
    let t = fract(x);
    return mix(hash(cell), hash(cell + 1.0), t * t * (3.0 - 2.0 * t));
}

@vertex
fn vertex(in: AirIn) -> AirOut {
    let to_eye = normalize(view.world_position.xyz - in.center);
    var side = cross(in.way, to_eye);
    if dot(side, side) < 1e-8 {
        side = cross(vec3(0.0, 1.0, 0.0), to_eye);
    }
    side = normalize(side);
    let half = max(in.look.x * (1.0 + SWELL), pixel_units(in.center));
    let world = in.center + side * (in.corner.x * half);
    var out: AirOut;
    out.position = position_world_to_clip(world);
    out.world = world;
    out.side = side;
    out.up = in.way;
    out.local = vec2(in.corner.x * half, in.corner.y);
    out.light = in.light.rgb;
    out.look = in.look;
    return out;
}

@fragment
fn fragment(in: AirOut) -> @location(0) vec4<f32> {
    let radius = in.look.x;
    let flow = in.local.y - globals.time * FLOW_SPEED;
    let r = radius * (1.0 + SWELL * sin(TAU * flow / (SWELL_RADII * radius)));
    let pixel = pixel_units(in.world);
    let drawn = max(r, pixel);
    let x = in.local.x;
    // A stream thinner than a pixel is drawn one pixel wide, as faint as its width.
    let cover = saturate((drawn - abs(x)) / pixel) * min(1.0, r / pixel);
    if cover <= 0.0 {
        discard;
    }
    let across = clamp(x / drawn, -1.0, 1.0);
    let h = sqrt(1.0 - across * across);
    let to_eye = normalize(view.world_position.xyz - in.world);
    let n = normalize(in.side * across + to_eye * h);
    let thickness = 2.0 * r * h * (1.0 - LUMPS + 2.0 * LUMPS * flow_noise(flow / radius));
    return air_shade(in.world, n, thickness, in.look, in.light, in.position, cover, in.look.w);
}
