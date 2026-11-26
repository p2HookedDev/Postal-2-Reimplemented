import package::p2_render::color::{BLOOD_BODY, BLOOD_SIGMA, to_display, to_linear};
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
    @location(4) look: vec4<f32>,
    @location(5) axis: vec4<f32>,
}

struct AirOut {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) side: vec3<f32>,
    @location(2) up: vec3<f32>,
    @location(3) local: vec2<f32>,
    @location(4) light: vec3<f32>,
    @location(5) look: vec4<f32>,
    @location(6) half: vec2<f32>,
    @location(7) @interpolate(flat) seed: f32,
    @location(8) axis: vec4<f32>,
}

struct Optics {
    sigma: vec3<f32>,
    sheen: f32,
    body: vec3<f32>,
    chunks: f32,
    roughness: f32,
    burns: f32,
    ragged: f32,
    least: f32,
}

struct Surface {
    tint: vec3<f32>,
    wet: f32,
    lump: f32,
}

const SHAPE_RIBBON: f32 = 0.0;
const SHAPE_FALL: f32 = 1.0;
const SHAPE_TUBE: f32 = 2.0;

@group(3) @binding(11) var puke_pour: texture_2d<f32>;
@group(3) @binding(12) var puke_pour_sampler: sampler;
@group(3) @binding(13) var puke_splat: texture_2d<f32>;
@group(3) @binding(14) var puke_splat_sampler: sampler;
const KINDS: u32 = 9u;
const BLOOD: u32 = 3u;
const PUKE: u32 = 4u;
const BLOODY_PUKE: u32 = 5u;
const BLOODY_SHARE: f32 = 0.55;

// Indexed by `FluidType` byte, row 0 for an unknown kind. A private var, as only memory may be indexed at run time.
var<private> OPTICS: array<Optics, KINDS> = array<Optics, KINDS>(
    Optics(vec3(0.05, 0.04, 0.03), 0.0, vec3(0.1, 0.1, 0.1), 0.0, 0.1, 0.0, 0.4, 0.2),
    // Gas
    Optics(vec3(0.01, 0.012, 0.02), 0.25, vec3(0.1, 0.1, 0.09), 0.0, 0.05, 0.0, 0.3, 0.12),
    // Urine
    Optics(vec3(0.03, 0.08, 0.5), 0.0, vec3(0.037, 0.035, 0.0), 0.0, 0.08, 0.0, 0.35, 0.25),
    // Blood
    Optics(BLOOD_SIGMA, 0.0, BLOOD_BODY, 0.0, 0.1, 0.0, 0.45, 0.6),
    // Puke
    Optics(vec3(3.5, 3.8, 5.0), 0.0, vec3(0.3, 0.2, 0.05), 0.9, 0.45, 0.0, 1.0, 0.8),
    // BloodyPuke
    Optics(vec3(3.0, 5.0, 6.0), 0.0, vec3(0.16, 0.03, 0.006), 0.7, 0.4, 0.0, 0.9, 0.8),
    // Gonorrhea
    Optics(vec3(1.8, 1.4, 2.6), 0.0, vec3(0.25, 0.28, 0.06), 0.4, 0.3, 0.0, 0.7, 0.5),
    // BloodyUrine
    Optics(vec3(0.3, 1.2, 1.6), 0.0, vec3(0.06, 0.012, 0.001), 0.0, 0.1, 0.0, 0.4, 0.35),
    // Napalm
    Optics(vec3(0.5, 0.8, 1.5), 0.0, vec3(0.1, 0.06, 0.01), 0.0, 0.2, 1.0, 0.6, 0.3),
);

const BURN: vec3<f32> = vec3(7.0, 3.0, 0.6);
const F0: f32 = 0.02;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;
const FILM_SCALE: f32 = 0.35;
const SUN_BODY: f32 = 0.5;
const LIGHT_LIMIT: f32 = 2.0;
const SPECULAR_LIMIT: f32 = 16.0;
const GLINT_HUE: f32 = 0.6;
const RIM: f32 = 0.5;
const KEY: vec3<f32> = vec3(0.32, 0.9, 0.3);
const KEY_GLINT: f32 = 0.25;
const BACK_LIGHT: f32 = 0.8;
const BACK_POWER: f32 = 6.0;
const DEEP: f32 = 0.25;
const WET_SHARPEN: f32 = 0.35;
const DRY_SPECULAR: f32 = 0.15;
const BILE: vec3<f32> = vec3(0.8, 1.15, 0.6);
const CHUNK: vec3<f32> = vec3(1.3, 1.2, 1.4);
const FLECK: vec3<f32> = vec3(0.35, 0.22, 0.2);
const MATTE_BELOW: f32 = 0.3;
const SHINY_ABOVE: f32 = 1.0;
const SOFT_UNITS: f32 = 2.0;

fn optics_of(kind: f32) -> Optics {
    return OPTICS[min(u32(kind + 0.5), KINDS - 1u)];
}

fn pcg(n: u32) -> u32 {
    let state = n * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn hash1(x: f32) -> f32 {
    return f32(pcg(bitcast<u32>(x))) * (1.0 / 4294967296.0);
}

fn hash2(p: vec2<f32>) -> f32 {
    return f32(pcg(bitcast<u32>(p.x) ^ pcg(bitcast<u32>(p.y)))) * (1.0 / 4294967296.0);
}

fn noise1(x: f32) -> f32 {
    let cell = floor(x);
    let t = x - cell;
    return mix(hash1(cell), hash1(cell + 1.0), t * t * (3.0 - 2.0 * t));
}

fn noise2(p: vec2<f32>) -> f32 {
    let cell = floor(p);
    let t = p - cell;
    let s = t * t * (3.0 - 2.0 * t);
    let low = mix(hash2(cell), hash2(cell + vec2(1.0, 0.0)), s.x);
    let high = mix(hash2(cell + vec2(0.0, 1.0)), hash2(cell + vec2(1.0, 1.0)), s.x);
    return mix(low, high, s.y);
}

fn surface_of(kind: f32, at: vec2<f32>) -> Surface {
    let chunks = optics_of(kind).chunks;
    if chunks <= 0.0 {
        return Surface(vec3(1.0), 1.0, 0.0);
    }
    let bile = smoothstep(0.35, 0.75, noise2(at * 0.6));
    let lump = smoothstep(0.62, 0.78, noise2(at * 1.3 + 3.3));
    let fleck = smoothstep(0.7, 0.8, noise2(at * 2.7 + 7.1));
    var tint = mix(vec3(1.0), BILE, bile);
    tint = mix(tint, CHUNK, lump);
    tint = mix(tint, FLECK, fleck);
    let wet = smoothstep(0.45, 0.7, noise2(at * 0.45 + 11.0)) * (1.0 - lump);
    return Surface(mix(vec3(1.0), tint, chunks), wet, lump * chunks);
}

fn is_puke(kind: f32) -> bool {
    let k = u32(kind + 0.5);
    return k == PUKE || k == BLOODY_PUKE;
}

fn texture_lod(side: f32, units: f32, pixel: f32) -> f32 {
    return log2(max(pixel * side / max(units, 1e-4), 1.0));
}

fn textured_surface(surface: Surface, kind: f32, colour: vec3<f32>, share: f32) -> Surface {
    let body = optics_of(kind).body;
    let own = select(colour, mix(colour, OPTICS[BLOOD].body, BLOODY_SHARE), u32(kind + 0.5) == BLOODY_PUKE);
    let tint = own / max(body, vec3(1e-4));
    return Surface(mix(surface.tint, tint, saturate(share)), surface.wet, surface.lump);
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

fn ggx(n_dot_h: f32, a2: f32) -> f32 {
    return a2 / (PI * pow(n_dot_h * n_dot_h * (a2 - 1.0) + 1.0, 2.0));
}

fn air_shade(world: vec3<f32>, n: vec3<f32>, thickness: f32, look: vec4<f32>, emitter: vec3<f32>, frag_coord: vec4<f32>, cover: f32, burning: f32, surface: Surface) -> vec4<f32> {
    let optics = optics_of(look.y);
    let turbid = select(0.0, 1.0, u32(look.y + 0.5) == BLOOD);
    let falling = select(0.0, 1.0, abs(look.z - SHAPE_FALL) < 0.5);
    let shine = 1.0 - turbid * falling *(1.0 - smoothstep(MATTE_BELOW, SHINY_ABOVE, look.x));
    let v =normalize(view.world_position.xyz - world);
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
    let depth = thickness * (1.0 + surface.lump);
    let transmit = exp(-optics.sigma * depth);
    let n_dot_v = max(dot(n, v), 1e-3);
    let fresnel = F0 + (1.0 - F0) * pow(1.0 - n_dot_v, 5.0);
    let alpha = cover * max(max(1.0 - dot(transmit, vec3(1.0 / 3.0)), fresnel * shine), optics.least * shine);
    let deep = mix(mix(vec3(DEEP), vec3(1.0), transmit), vec3(1.0), max(saturate(optics.chunks * 1.25), turbid));
    let body = to_linear(to_display(optics.body * surface.tint * deep) * light);

    let a2 = pow(max(optics.roughness * mix(1.0, WET_SHARPEN, surface.wet * optics.chunks), 0.03), 4.0);
    let lit_side = select(0.0, 1.0, dot(n, to_sun) > 0.0);
    let brightest = max(max(optics.body.r, optics.body.g), max(optics.body.b, 1e-4));
    let own = optics.body / brightest;
    let hue = mix(vec3(1.0), own, GLINT_HUE * (1.0 - turbid));
    let film = 0.5 + 0.5 * cos(TAU * (depth * FILM_SCALE + (1.0 - n_dot_v) * 0.6 + vec3(0.0, 0.33, 0.67)));
    let sun_glint = ggx(max(dot(n, normalize(to_sun + v)), 0.0), a2) * lit_side;
    let key_glint = ggx(max(dot(n, normalize(KEY + v)), 0.0), a2) * KEY_GLINT;
    var specular = (sun_light * sun_glint + to_linear(light) * key_glint) * (fresnel / (4.0 * n_dot_v)) * hue + to_linear(light) * (fresnel * RIM * (1.0 - 0.7 * turbid));
    let dry = mix(1.0, mix(DRY_SPECULAR, 1.0, surface.wet), optics.chunks);
    specular = min(specular * mix(vec3(1.0), 2.0 * film, optics.sheen) * dry * shine, vec3(SPECULAR_LIMIT));
    let behind = pow(saturate(dot(-v, to_sun)), BACK_POWER);
    let back = min(sun_light * transmit * own * (behind * BACK_LIGHT), vec3(LIGHT_LIMIT));

    let flicker = 0.75 + 0.25 * sin(globals.time * 23.0 + world.x * 0.3 + world.z * 0.2);
    let burn = BURN * (max(burning, optics.burns) * flicker * clamp(depth * 0.5, 0.2, 1.0));
    let fade = scene_fade(frag_coord);
    return vec4(((body + back + burn) * alpha + specular * cover) * fade, alpha * fade);
}
