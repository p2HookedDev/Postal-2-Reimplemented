// Drops in the air as quads facing the eye: a bead, a falling drop as an uneven capsule stretched by its motion over
// the frame, a hanging drip with its neck, and the ring a hard landing leaves, lying on the surface.

import package::p2_render::liquid_look::{AirIn, AirOut, SHAPE_CROWN, SHAPE_HANG, air_shade, pixel_units};
import bevy_pbr::render::{
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
};

// The longest streak in radii; a falling drop's tail and a drip's neck against its radius; a drip's length in radii
// when it lets go.
const MOST_STREAK: f32 = 4.0;
const TAIL: f32 = 0.7;
const NECK: f32 = 0.35;
const HANG_STRETCH: f32 = 0.6;
// A crown's ring reaches this many radii, this wide, its normal tilted out this much.
const CROWN_SPREAD: f32 = 4.0;
const CROWN_WIDTH: f32 = 0.6;
const CROWN_TILT: f32 = 0.6;
// Unreal units a crown floats over its surface.
const CROWN_LIFT: f32 = 0.05;

fn crown_ring(radius: f32, age: f32) -> f32 {
    return radius * (1.0 + (CROWN_SPREAD - 1.0) * age);
}

@vertex
fn vertex(in: AirIn) -> AirOut {
    let radius = max(in.look.x, pixel_units(in.center));
    var out: AirOut;
    var world: vec3<f32>;
    if in.look.z == SHAPE_CROWN {
        let n = normalize(in.way);
        let t = normalize(select(cross(n, vec3(1.0, 0.0, 0.0)), cross(n, vec3(0.0, 0.0, 1.0)), abs(n.x) > 0.9));
        let reach = crown_ring(radius, in.look.w) + radius * CROWN_WIDTH;
        out.local = in.corner * reach;
        world = in.center + n * CROWN_LIFT + (t * out.local.x + cross(n, t) * out.local.y);
        out.side = t;
        out.up = n;
    } else {
        // The capsule's axis runs from its tail to its front: back along a fall, up to where a drip hangs from.
        var axis = in.way;
        if in.look.z == SHAPE_HANG {
            axis = vec3(0.0, 1.0, 0.0) * (in.look.x * HANG_STRETCH * min(in.look.w, 1.0));
        }
        let streak = length(axis);
        axis *= min(streak, MOST_STREAK * in.look.x) / max(streak, 1e-6);
        let to_eye = normalize(view.world_position.xyz - in.center);
        let flat = axis - to_eye * dot(axis, to_eye);
        let up = normalize(select(flat, cross(to_eye, vec3(1.0, 0.0, 0.0)), dot(flat, flat) < 1e-8));
        let side = normalize(cross(up, to_eye));
        let half_length = length(flat) * 0.5;
        out.half = vec2(radius, half_length);
        out.local = vec2(in.corner.x * radius, in.corner.y * (half_length + radius));
        world = in.center - axis * 0.5 + side * out.local.x + up * out.local.y;
        out.side = side;
        out.up = up;
    }
    out.position = position_world_to_clip(world);
    out.world = world;
    out.light = in.light.rgb;
    out.look = in.look;
    return out;
}

@fragment
fn fragment(in: AirOut) -> @location(0) vec4<f32> {
    let pixel = pixel_units(in.world);
    let radius = max(in.look.x, pixel);
    // A drop smaller than a pixel is drawn one pixel wide, as faint as its area, so far drops do not flicker.
    let area = min(1.0, in.look.x / pixel);
    let to_eye = normalize(view.world_position.xyz - in.world);
    var cover: f32;
    var n: vec3<f32>;
    var thickness: f32;
    if in.look.z == SHAPE_CROWN {
        let width = max(radius * CROWN_WIDTH * (1.0 - in.look.w), pixel);
        let d = length(in.local);
        let off = (d - crown_ring(radius, in.look.w)) / width;
        cover = saturate(1.0 - abs(off)) * (1.0 - in.look.w);
        let radial = (in.side * in.local.x + cross(in.up, in.side) * in.local.y) / max(d, 1e-4);
        n = normalize(in.up + radial * (off * CROWN_TILT));
        thickness = width * cover;
    } else {
        let s = in.local.y;
        let u = clamp(s / max(in.half.y + radius, 1e-4), -1.0, 1.0);
        var profile = mix(TAIL, 1.0, u * 0.5 + 0.5);
        if in.look.z == SHAPE_HANG {
            profile = mix(1.0, NECK, smoothstep(0.0, 1.0, u));
        }
        let r = radius * profile;
        let cap = max(abs(s) - in.half.y, 0.0);
        let width = sqrt(max(r * r - cap * cap, 0.0));
        let x = in.local.x;
        cover = saturate((width - abs(x)) / pixel);
        let z = sqrt(max(width * width - x * x, 0.0));
        n = normalize(in.side * x + in.up * (sign(s) * cap) + to_eye * max(z, 1e-4));
        thickness = 2.0 * z;
    }
    cover *= area * area;
    if cover <= 0.0 {
        discard;
    }
    return air_shade(in.world, n, thickness, in.look, in.light, in.position, cover, 0.0);
}
