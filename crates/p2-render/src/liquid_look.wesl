// The look of liquid in the air, shared by the ribbon and the drops: each kind's optics, and the light it takes, which
// is the lamps, zone ambient and sun the particles take, never the frame behind it.

import package::p2_render::color::{to_display, to_linear};
import package::p2_render::sun_shadow::{sun_index, sun_map_visibility};
// The levels are the material's bindings 5 and 6 (`P2_PARTICLE_LEVELS`), so the source carries no index.
import package::p2_render::sun_levels::LevelSource;
import bevy_pbr::render::{
    mesh_view_bindings::{view, lights, globals},
    view_transformations::depth_ndc_to_view_z,
};

@if(DEPTH_PREPASS)
import bevy_pbr::render::mesh_view_bindings::depth_prepass_texture;

// `AirMesh` in `liquids/air_draw.rs`.
struct AirIn {
    @location(0) center: vec3<f32>,
    @location(1) way: vec3<f32>,
    @location(2) corner: vec2<f32>,
    @location(3) light: vec4<f32>,
    // Radius, `FluidType` byte, `SHAPE_*`, and the shape's extra word.
    @location(4) look: vec4<f32>,
}

struct AirOut {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) side: vec3<f32>,
    @location(2) up: vec3<f32>,
    // Unreal units across and along the quad from its middle.
    @location(3) local: vec2<f32>,
    @location(4) light: vec3<f32>,
    @location(5) look: vec4<f32>,
    // A capsule's radius and the half length of its straight part.
    @location(6) half: vec2<f32>,
}

struct Optics {
    // Absorbed per Unreal unit of liquid, per channel.
    sigma: vec3<f32>,
    // How much a thin film colours the specular.
    sheen: f32,
    // The linear albedo the liquid scatters back: dark, as a wet liquid's colour is mostly its specular.
    body: vec3<f32>,
    // How much lumps vary the thickness.
    chunks: f32,
    roughness: f32,
    // 1 for a liquid that always burns.
    burns: f32,
}

const SHAPE_RIBBON: f32 = 0.0;
const SHAPE_FALL: f32 = 1.0;
const SHAPE_CROWN: f32 = 2.0;
const SHAPE_HANG: f32 = 3.0;
const KINDS: u32 = 9u;

// By `FluidType` byte; row 0 stands for a kind the table does not know. A private var, as only memory may be indexed
// by a value known at run time.
var<private> OPTICS: array<Optics, KINDS> = array<Optics, KINDS>(
    Optics(vec3(0.05, 0.04, 0.03), 0.0, vec3(0.1, 0.1, 0.1), 0.0, 0.1, 0.0),
    // Gas
    Optics(vec3(0.01, 0.012, 0.02), 1.0, vec3(0.1, 0.1, 0.09), 0.0, 0.05, 0.0),
    // Urine
    Optics(vec3(0.03, 0.08, 0.5), 0.0, vec3(0.037, 0.035, 0.0), 0.0, 0.08, 0.0),
    // Blood
    Optics(vec3(0.6, 3.0, 3.2), 0.0, vec3(0.12, 0.001, 0.0006), 0.0, 0.12, 0.0),
    // Puke
    Optics(vec3(0.9, 0.7, 1.6), 0.0, vec3(0.36, 0.11, 0.008), 0.8, 0.35, 0.0),
    // BloodyPuke
    Optics(vec3(0.8, 1.6, 2.2), 0.0, vec3(0.14, 0.02, 0.003), 0.6, 0.3, 0.0),
    // Gonorrhea
    Optics(vec3(0.6, 0.4, 0.9), 0.0, vec3(0.25, 0.28, 0.06), 0.3, 0.25, 0.0),
    // BloodyUrine
    Optics(vec3(0.3, 1.2, 1.6), 0.0, vec3(0.06, 0.012, 0.001), 0.0, 0.1, 0.0),
    // Napalm
    Optics(vec3(0.5, 0.8, 1.5), 0.0, vec3(0.1, 0.06, 0.01), 0.0, 0.2, 1.0),
);

// Burning liquid's light, in linear HDR.
const BURN: vec3<f32> = vec3(7.0, 3.0, 0.6);
const F0: f32 = 0.02;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;
const FILM_SCALE: f32 = 0.35;
// As a puff of smoke, liquid in the air faces the sun with about half of itself.
const SUN_BODY: f32 = 0.5;
const LIGHT_LIMIT: f32 = 2.0;
const SPECULAR_LIMIT: f32 = 16.0;
// How much of the liquid's own hue its glints take, and how much of the light around it its rim reflects.
const GLINT_HUE: f32 = 0.6;
const RIM: f32 = 0.5;
// Unreal units over which liquid fades into the geometry behind it.
const SOFT_UNITS: f32 = 2.0;

fn optics_of(kind: f32) -> Optics {
    return OPTICS[min(u32(kind + 0.5), KINDS - 1u)];
}

fn pixel_units(world: vec3<f32>) -> f32 {
    let distance = length(world - view.world_position.xyz);
    return 2.0 * distance / (view.clip_from_view[1][1] * view.viewport.w);
}

fn scene_fade(frag_coord: vec4<f32>) -> f32 {
@if(DEPTH_PREPASS) {
    let scene = textureLoad(depth_prepass_texture, vec2<i32>(frag_coord.xy), 0);
    if scene <= 0.0 || frag_coord.z <= 0.0 {
        return 1.0;
    }
    return saturate((depth_ndc_to_view_z(frag_coord.z) - depth_ndc_to_view_z(scene)) / SOFT_UNITS);
}
@else {
    return 1.0;
}
}

// Premultiplied colour of liquid `thickness` units deep at `world` with normal `n`: its body lit by the light at its
// emitter (display brightness) and the sun where it reaches, absorbed over the depth, and the sun's glint.
fn air_shade(world: vec3<f32>, n: vec3<f32>, thickness: f32, look: vec4<f32>, emitter: vec3<f32>, frag_coord: vec4<f32>, cover: f32, burning: f32) -> vec4<f32> {
    let optics = optics_of(look.y);
    let v = normalize(view.world_position.xyz - world);
    var light = emitter;
    var sun_light = vec3(0.0);
    var to_sun = vec3(0.0, 1.0, 0.0);
    let sun = sun_index();
    if sun < lights.n_directional_lights {
        to_sun = lights.directional_lights[sun].direction_to_light;
        let seen = sun_map_visibility(LevelSource(0u, 0u), sun, vec4(world, 1.0), n, frag_coord.xy);
        sun_light = lights.directional_lights[sun].color.rgb * seen;
        light += to_display(sun_light) * SUN_BODY;
    }
    light = min(light, vec3(LIGHT_LIMIT));
    let depth = thickness * (1.0 + optics.chunks * 0.5);
    let transmit = exp(-optics.sigma * depth);
    let alpha = cover * (1.0 - dot(transmit, vec3(1.0 / 3.0)));
    let body = to_linear(to_display(optics.body) * light);

    let n_dot_v = max(dot(n, v), 1e-3);
    let fresnel = F0 + (1.0 - F0) * pow(1.0 - n_dot_v, 5.0);
    let h = normalize(to_sun + v);
    let n_dot_h = max(dot(n, h), 0.0);
    let a2 = pow(max(optics.roughness, 0.03), 4.0);
    let ggx = a2 / (PI * pow(n_dot_h * n_dot_h * (a2 - 1.0) + 1.0, 2.0));
    let lit_side = select(0.0, 1.0, dot(n, to_sun) > 0.0);
    let brightest = max(max(optics.body.r, optics.body.g), max(optics.body.b, 1e-4));
    let hue = mix(vec3(1.0), optics.body / brightest, GLINT_HUE);
    let film = 0.5 + 0.5 * cos(TAU * (depth * FILM_SCALE + (1.0 - n_dot_v) * 0.6 + vec3(0.0, 0.33, 0.67)));
    var specular = sun_light * (ggx * fresnel * lit_side / (4.0 * n_dot_v)) * hue + to_linear(light) * (fresnel * RIM);
    specular = min(specular * mix(vec3(1.0), 2.0 * film, optics.sheen), vec3(SPECULAR_LIMIT));

    let flicker = 0.75 + 0.25 * sin(globals.time * 23.0 + world.x * 0.3 + world.z * 0.2);
    let burn = BURN * (max(burning, optics.burns) * flicker * clamp(depth * 0.5, 0.2, 1.0));
    let fade = scene_fade(frag_coord);
    return vec4((body * alpha + specular * cover + burn * alpha) * fade, alpha * fade);
}
