// The liquid stream's surface in screen space. `splat` keeps each pixel's nearest liquid and its bits, `thickness`
// adds up each liquid's optical depth through its particles, `narrow_range` smooths the liquid's depth, and
// `composite` shades it over the frame. All four read the particles as sphere impostors; a lying one is flattened.

import bevy_render::view::View;
import package::p2_render::depth_surface::{normal_at, world_at};

struct Particle {
    // Radius in `w`; 0 or less is a dead particle.
    place: vec4<f32>,
    velocity: vec3<f32>,
    bits: u32,
}

// `LiquidSurface` in `surface.rs`; the ambient is in display light.
struct Surface {
    sun_direction: vec3<f32>,
    seconds: f32,
    sun_color: vec3<f32>,
    scale: u32,
    ambient: vec3<f32>,
    delta: f32,
}

struct Optics {
    // Absorbed per Unreal unit of liquid, per channel.
    sigma: vec3<f32>,
    // How much a thin film colours the specular.
    sheen: f32,
    // The linear colour the liquid scatters back.
    body: vec3<f32>,
    // How much lumps vary the thickness.
    chunks: f32,
    roughness: f32,
    // 1 for a liquid that always burns.
    burns: f32,
}

const KIND_MASK: u32 = 0xFu;
const LANDED: u32 = 16u;
const BURNING: u32 = 32u;
const KINDS: u32 = 9u;

// By `FluidType` byte; row 0 stands for a kind the table does not know. A private var, as only memory may be indexed
// by a value known at run time.
var<private> OPTICS: array<Optics, KINDS> = array<Optics, KINDS>(
    Optics(vec3(0.05, 0.04, 0.03), 0.0, vec3(0.6, 0.6, 0.6), 0.0, 0.1, 0.0),
    // Gas
    Optics(vec3(0.01, 0.012, 0.02), 1.0, vec3(0.9, 0.85, 0.6), 0.0, 0.05, 0.0),
    // Urine
    Optics(vec3(0.03, 0.08, 0.5), 0.0, vec3(0.9, 0.75, 0.2), 0.0, 0.08, 0.0),
    // Blood
    Optics(vec3(0.6, 3.0, 3.2), 0.0, vec3(0.35, 0.01, 0.01), 0.0, 0.12, 0.0),
    // Puke
    Optics(vec3(0.9, 0.7, 1.6), 0.0, vec3(0.45, 0.42, 0.08), 0.8, 0.35, 0.0),
    // BloodyPuke
    Optics(vec3(0.8, 1.6, 2.2), 0.0, vec3(0.42, 0.15, 0.04), 0.6, 0.3, 0.0),
    // Gonorrhea
    Optics(vec3(0.6, 0.4, 0.9), 0.0, vec3(0.55, 0.6, 0.3), 0.3, 0.25, 0.0),
    // BloodyUrine
    Optics(vec3(0.3, 1.2, 1.6), 0.0, vec3(0.6, 0.15, 0.05), 0.0, 0.1, 0.0),
    // Napalm
    Optics(vec3(0.5, 0.8, 1.5), 0.0, vec3(0.5, 0.25, 0.05), 0.0, 0.2, 1.0),
);

// Burning liquid's light, in linear HDR.
const BURN: vec3<f32> = vec3(7.0, 3.0, 0.6);
const F0: f32 = 0.02;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;
// A lying particle's radii against its round radius: wide along the surface it lies on, flat along its normal.
const SPREAD: f32 = 1.4;
const FLAT: f32 = 0.3;
// Lumps in a chunky liquid, per Unreal unit.
const CHUNK_CELL: f32 = 0.6;
// The narrow-range filter: its reach and the depth range around the middle it keeps, in Unreal units, and the most
// texels it reaches and taps on each side.
const SMOOTH_UNITS: f32 = 3.0;
const RANGE_UNITS: f32 = 4.0;
const MOST_REACH: f32 = 12.0;
const TAPS: f32 = 4.0;
// How far behind the scene the smoothed liquid may still be drawn, in Unreal units.
const SCENE_SLACK: f32 = 1.0;
const FILM_SCALE: f32 = 0.35;

@group(0) @binding(0) var<storage, read> particles: array<Particle>;
@group(0) @binding(1) var scene_depth: texture_depth_2d;
@group(0) @binding(2) var<uniform> view: View;
@group(0) @binding(3) var<uniform> surface: Surface;
@group(0) @binding(4) var liquid_depth: texture_depth_2d;
@group(0) @binding(5) var smoothed: texture_2d<f32>;
@group(0) @binding(6) var optical: texture_2d<f32>;
@group(0) @binding(7) var front: texture_2d<u32>;

struct Splat {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) particle: u32,
    @location(1) world: vec3<f32>,
    @location(2) @interpolate(flat) normal: vec3<f32>,
}

// Distances along `dir` from the eye; `far` below `near` misses.
struct Ray {
    dir: vec3<f32>,
    near: f32,
    far: f32,
}

struct SplatOut {
    @builtin(frag_depth) depth: f32,
    @location(0) bits: u32,
}

fn bound_of(p: Particle) -> f32 {
    return p.place.w * select(1.0, max(SPREAD, FLAT), (p.bits & LANDED) != 0u);
}

// The scene's normal under a lying particle, from the depth prepass at the particle's own pixel; world up otherwise.
fn lying_normal(p: Particle) -> vec3<f32> {
    if (p.bits & LANDED) == 0u {
        return vec3(0.0, 1.0, 0.0);
    }
    let clip = view.clip_from_world * vec4(p.place.xyz, 1.0);
    let size = vec2<f32>(textureDimensions(scene_depth));
    let uv = vec2(clip.x, -clip.y) / clip.w * 0.5 + 0.5;
    let pixel = vec2<i32>(clamp(uv * size, vec2(1.0), size - 2.0));
    let surface = world_at(scene_depth, view.world_from_clip, pixel, size);
    return normal_at(scene_depth, view.world_from_clip, view.world_position, pixel, size, surface);
}

// Into the space where the particle is a unit sphere: squashed along `n` while it lies.
fn unit_space(p: Particle, n: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let r = p.place.w;
    if (p.bits & LANDED) == 0u {
        return v / r;
    }
    let along = dot(v, n) * n;
    return (v - along) / (r * SPREAD) + along / (r * FLAT);
}

fn kind_of(bits: u32) -> u32 {
    let kind = bits & KIND_MASK;
    return select(0u, kind, kind < KINDS);
}

fn hash3(p: vec3<f32>) -> f32 {
    var q = fract(p * 0.1031);
    q += dot(q, q.zyx + 31.32);
    return fract((q.x + q.y) * q.z);
}

fn value_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let low = mix(mix(hash3(i), hash3(i + vec3(1.0, 0.0, 0.0)), u.x), mix(hash3(i + vec3(0.0, 1.0, 0.0)), hash3(i + vec3(1.0, 1.0, 0.0)), u.x), u.y);
    let high = mix(mix(hash3(i + vec3(0.0, 0.0, 1.0)), hash3(i + vec3(1.0, 0.0, 1.0)), u.x), mix(hash3(i + vec3(0.0, 1.0, 1.0)), hash3(i + vec3(1.0, 1.0, 1.0)), u.x), u.y);
    return mix(low, high, u.z);
}

// The eye's distance to the scene behind a full-size pixel; the sky is endless.
fn scene_distance(pixel: vec2<i32>) -> f32 {
    let depth = textureLoad(scene_depth, pixel, 0);
    if depth <= 0.0 {
        return 1e30;
    }
    let uv = (vec2<f32>(pixel) + 0.5 - view.viewport.xy) / view.viewport.zw;
    let world = view.world_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    return length(world.xyz / world.w - view.world_position);
}

fn full_pixel(position: vec2<f32>) -> vec2<i32> {
    return vec2<i32>(position * f32(surface.scale) + view.viewport.xy);
}

// Solved from the impostor's own point, close to the particle, so a far particle keeps its precision.
fn ray_through(p: Particle, n: vec3<f32>, world: vec3<f32>) -> Ray {
    let start = length(world - view.world_position);
    let dir = (world - view.world_position) / start;
    let o = unit_space(p, n, world - p.place.xyz);
    let d = unit_space(p, n, dir);
    let a = dot(d, d);
    let b = dot(o, d);
    let disc = b * b - a * (dot(o, o) - 1.0);
    if disc < 0.0 {
        return Ray(dir, 1.0, 0.0);
    }
    let root = sqrt(disc);
    return Ray(dir, start + (-b - root) / a, start + (-b + root) / a);
}

@vertex
fn splat_vertex(@builtin(vertex_index) vertex: u32) -> Splat {
    let index = vertex / 6u;
    let p = particles[index];
    let corner_index = vertex % 6u;
    let corner = vec2(
        select(-1.0, 1.0, corner_index == 1u || corner_index == 2u || corner_index == 4u),
        select(-1.0, 1.0, corner_index == 2u || corner_index == 4u || corner_index == 5u),
    );
    let bound = bound_of(p);
    let to_eye = view.world_position - p.place.xyz;
    let d = length(to_eye);
    var out: Splat;
    out.particle = index;
    if p.place.w <= 0.0 || d <= bound * 1.05 {
        out.position = vec4(2.0, 2.0, 2.0, 1.0);
        out.world = p.place.xyz;
        return out;
    }
    out.normal = lying_normal(p);
    let forward = to_eye / d;
    let side = normalize(cross(view.world_from_view[1].xyz, forward));
    let up = cross(forward, side);
    // On the bounding sphere's near face, as wide as the sphere's outline seen from the eye.
    let half_size = 1.02 * bound * sqrt((d - bound) / (d + bound));
    out.world = p.place.xyz + forward * bound + (side * corner.x + up * corner.y) * half_size;
    out.position = view.clip_from_world * vec4(out.world, 1.0);
    return out;
}

@fragment
fn splat(in: Splat) -> SplatOut {
    let p = particles[in.particle];
    let ray = ray_through(p, in.normal, in.world);
    if ray.far <= ray.near || scene_distance(full_pixel(in.position.xy)) < ray.near {
        discard;
    }
    let clip = view.clip_from_world * vec4(view.world_position + ray.dir * ray.near, 1.0);
    return SplatOut(clip.z / clip.w, p.bits & 0xFFu);
}

@fragment
fn thickness(in: Splat) -> @location(0) vec4<f32> {
    let p = particles[in.particle];
    let ray = ray_through(p, in.normal, in.world);
    let far = min(ray.far, scene_distance(full_pixel(in.position.xy)));
    if far <= ray.near {
        discard;
    }
    let row = OPTICS[kind_of(p.bits)];
    let at = view.world_position + ray.dir * ray.near;
    let lumps = 1.0 + row.chunks * (hash3(floor(at * CHUNK_CELL)) * 2.0 - 1.0);
    let chord = (far - ray.near) * lumps;
    return vec4(row.sigma * chord, chord);
}

fn eye_depth(depth: f32) -> f32 {
    let at = view.view_from_clip * vec4(0.0, 0.0, depth, 1.0);
    return -at.z / at.w;
}

// Truong and Yuksel's narrow-range filter: a neighbour far behind the middle is another layer and left out, one in
// front is clamped to the range, so edges stay sharp and the surface's bumps smooth out.
@fragment
fn narrow_range(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let centre = vec2<i32>(position.xy);
    let depth = textureLoad(liquid_depth, centre, 0);
    if depth <= 0.0 {
        return vec4(0.0);
    }
    let z = eye_depth(depth);
    let texels_per_unit = 0.5 * view.viewport.w / f32(surface.scale) * view.clip_from_view[1][1] / z;
    let reach = clamp(SMOOTH_UNITS * texels_per_unit, 1.0, MOST_REACH);
    let stride = max(1, i32(ceil(reach / TAPS)));
    let span = i32(reach);
    let spread = 0.5 * reach * reach;
    let last = vec2<i32>(textureDimensions(liquid_depth)) - 1;
    var sum = 0.0;
    var weight = 0.0;
    for (var y = -span; y <= span; y += stride) {
        for (var x = -span; x <= span; x += stride) {
            let neighbour = textureLoad(liquid_depth, clamp(centre + vec2(x, y), vec2(0), last), 0);
            if neighbour <= 0.0 {
                continue;
            }
            let zq = eye_depth(neighbour);
            if zq > z + RANGE_UNITS {
                continue;
            }
            let w = exp(-f32(x * x + y * y) / spread);
            sum += max(zq, z - RANGE_UNITS) * w;
            weight += w;
        }
    }
    return vec4(sum / max(weight, 1e-6), 0.0, 0.0, 0.0);
}

fn view_position(texel: vec2<i32>, z: f32) -> vec3<f32> {
    let uv = (vec2<f32>(texel) + 0.5) / vec2<f32>(textureDimensions(smoothed));
    let far = view.view_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 1.0, 1.0);
    let dir = far.xyz / far.w;
    return dir * (z / -dir.z);
}

// The step to the neighbour along `offset` on the side whose depth is nearer the middle's, so an edge does not bend
// the normal.
fn surface_step(texel: vec2<i32>, middle: vec3<f32>, offset: vec2<i32>) -> vec3<f32> {
    let last = vec2<i32>(textureDimensions(smoothed)) - 1;
    let ahead = clamp(texel + offset, vec2(0), last);
    let behind = clamp(texel - offset, vec2(0), last);
    let z_ahead = textureLoad(smoothed, ahead, 0).r;
    let z_behind = textureLoad(smoothed, behind, 0).r;
    let forward = view_position(ahead, z_ahead) - middle;
    let back = middle - view_position(behind, z_behind);
    if z_ahead <= 0.0 {
        return back;
    }
    if z_behind <= 0.0 || abs(forward.z) < abs(back.z) {
        return forward;
    }
    return back;
}

@fragment
fn composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = vec2<i32>((position.xy - view.viewport.xy) / f32(surface.scale));
    let z = textureLoad(smoothed, texel, 0).r;
    let optics = textureLoad(optical, texel, 0);
    if z <= 0.0 || optics.a <= 0.0 {
        discard;
    }
    let scene = textureLoad(scene_depth, vec2<i32>(position.xy), 0);
    if scene > 0.0 && eye_depth(scene) < z - SCENE_SLACK {
        discard;
    }
    let middle = view_position(texel, z);
    var normal_in_view = normalize(cross(surface_step(texel, middle, vec2(0, 1)), surface_step(texel, middle, vec2(1, 0))));
    if dot(normal_in_view, middle) > 0.0 {
        normal_in_view = -normal_in_view;
    }
    let n = normalize((view.world_from_view * vec4(normal_in_view, 0.0)).xyz);
    let world = (view.world_from_view * vec4(middle, 1.0)).xyz;
    let v = normalize(view.world_position - world);
    let l = surface.sun_direction;
    let bits = textureLoad(front, texel, 0).r;
    let row = OPTICS[kind_of(bits)];

    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_v = max(dot(n, v), 1e-4);
    let h = normalize(l + v);
    let n_dot_h = max(dot(n, h), 0.0);
    let a = row.roughness * row.roughness;
    let a2 = a * a;
    let lobe = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    let ggx = a2 / (PI * lobe * lobe);
    let k = 0.5 * a;
    let visibility = 0.25 / ((n_dot_l * (1.0 - k) + k) * (n_dot_v * (1.0 - k) + k));
    let fresnel_sun = F0 + (1.0 - F0) * pow(1.0 - max(dot(v, h), 0.0), 5.0);
    let fresnel_view = F0 + (1.0 - F0) * pow(1.0 - n_dot_v, 5.0);
    let ambient = pow(max(surface.ambient, vec3(0.0)), vec3(2.2));
    var specular = surface.sun_color * (ggx * visibility * fresnel_sun * n_dot_l) + ambient * fresnel_view;
    let film = 0.5 + 0.5 * cos(TAU * (optics.a * FILM_SCALE + (1.0 - n_dot_v) * 0.6 + vec3(0.0, 0.33, 0.67)));
    specular *= mix(vec3(1.0), 2.0 * film, row.sheen);

    let absorbed = 1.0 - exp(-optics.rgb);
    let light = ambient + surface.sun_color * (0.25 + 0.75 * n_dot_l);
    var colour = row.body * light * absorbed * (1.0 - fresnel_view) + specular;
    var alpha = (absorbed.r + absorbed.g + absorbed.b) / 3.0;
    let burning = max(row.burns, select(0.0, 1.0, (bits & BURNING) != 0u));
    if burning > 0.0 {
        let flicker = 0.6 + 0.8 * value_noise(world * 0.08 + vec3(0.0, -2.5 * surface.seconds, 0.0));
        colour += BURN * burning * flicker * clamp(optics.a * 0.5, 0.2, 1.0);
        alpha = max(alpha, 0.5 * burning);
    }
    return vec4(colour, alpha);
}
