// Modern shading, read only while the switch is on: real-time light from the map's suns and sky, no baked light.
// Linear light is display light squared. Diffuse lights add in display light as the original's do, keeping the
// map's colours; only highlights add in linear light. The result is linear light, past 1 where bright.

import package::p2_render::color::{to_display, to_linear};
import package::p2_render::modern_base::{DIELECTRIC_F0, LUMINANCE, LampLight, MODERN_ON, NO_GLOSS, fresnel_five, ggx, modern_bits, to_eye};
import package::p2_render::sun_shadow::{ROOFED_SKY, SKY_FACING_FADE, casts_shadow, is_sun, sun_index};
import bevy_pbr::render::{
    mesh_view_bindings::{view, lights},
    pbr_types,
};
@if(SCREEN_SPACE_AMBIENT_OCCLUSION)
import bevy_pbr::render::mesh_view_bindings::screen_space_ambient_occlusion_texture;

const AO_STRENGTH: f32 = 0.8;

const SKY_SHADOWED: f32 = 0.7;
// The sky's reflection in a sun shadow: what cast the shadow also stands in the way of a reflection towards the
// sun's side.
const REFLECTION_SHADOWED: f32 = 0.2;

// Matches `MaterialClass::Default`; terrain and unclassed surfaces take it.
const DEFAULT_ROUGHNESS: f32 = 0.7;
// The sun is a disc of 0.53 degrees, which widens the highlight.
const SUN_DISC: f32 = 0.002325;
const GROUND_ALBEDO: vec3<f32> = vec3(0.17, 0.14, 0.11);
// Retail's lit surface stops at twice its texture.
const DIFFUSE_LIMIT: f32 = 2.0;

const ACTOR_GLOSS: f32 = 0.1;
// Skinned actors' sun shadow fades in over N · L from 0 to this (the interpolated normal's): near the terminator
// a low-poly mesh's flat faces shadow themselves where its smooth normal still faces the sun, which showed its
// triangles.
const ACTOR_TERMINATOR_COS: f32 = 0.35;
const ACTOR_SHININESS: f32 = 20.0;

fn is_modern(flag: f32) -> bool {
    return (modern_bits(flag) & MODERN_ON) != 0u;
}

fn allows_gloss(flag: f32) -> bool {
    let bits = modern_bits(flag);
    return (bits & MODERN_ON) != 0u && (bits & NO_GLOSS) == 0u;
}

// x roughness, y metal, z 1 on a special-lit surface and else its reflectance as a dielectric. `packed` is the
// roughness, 0 for the default surface, 2 further from 0 when special-lit, negative on a metal.
fn surface_of(packed: f32) -> vec3<f32> {
    let size = abs(packed);
    let special = size >= 2.0;
    let rough = select(size, size - 2.0, special);
    return vec3(select(rough, DEFAULT_ROUGHNESS, rough == 0.0), select(0.0, 1.0, packed < 0.0), select(DIELECTRIC_F0, 1.0, special));
}

// The materials level sits above the flag's effect bits. Matches `material_class.rs`.
const MATERIALS_SHIFT: u32 = 3u;
const MATERIALS_TOP: f32 = 10.0;

// `blend` z is the material class's roughness (0 without a class), w its reflectance (1 on a metal). The level
// moves the surface from the default towards its class. A classed surface's reflectance is negated: a class
// guessed from names changes only how it shines, as its texture already holds its paint and shading.
fn classed_surface(flag: f32, blend: vec4<f32>) -> vec3<f32> {
    let level = (modern_bits(flag) >> MATERIALS_SHIFT) & 15u;
    if level == 0u || blend.z <= 0.0 {
        return surface_of(0.0);
    }
    let scale = min(f32(level) / MATERIALS_TOP, 1.0);
    let metal = blend.w >= 1.0;
    return vec3(mix(DEFAULT_ROUGHNESS, blend.z, scale), select(0.0, scale, metal), -mix(DIELECTRIC_F0, select(blend.w, DIELECTRIC_F0, metal), scale));
}

fn ao_with_bounce(ao: f32, albedo: vec3<f32>) -> vec3<f32> {
    let a = 2.0404 * albedo - 0.3324;
    let b = -4.7951 * albedo + 0.6417;
    let c = 2.7552 * albedo + 0.6903;
    return max(vec3(ao), ((ao * a + b) * ao + c) * ao);
}

struct Around {
    sun_color: vec3<f32>,
    to_sun: vec3<f32>,
    ambient: vec3<f32>,
    sky: vec3<f32>,
    fills: vec3<f32>,
    glow: vec3<f32>,
}

fn view_around(normal: vec3<f32>, lamps: LampLight, surface: vec3<f32>) -> Around {
    let special = surface.z > 0.5;
    var around = Around(vec3(0.0), vec3(0.0, 1.0, 0.0), lights.ambient_color.rgb, vec3(0.0), vec3(0.0), vec3(0.0));
    let sun = sun_index();
    if sun < lights.n_directional_lights {
        around.sun_color = select(lights.directional_lights[sun].color.rgb, vec3(0.0), special);
        around.to_sun = lights.directional_lights[sun].direction_to_light;
    }
    var fill_count = 0.0;
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if !casts_shadow(i) && !is_sun(i) && (lights.directional_lights[i].cascades_overlap_proportion > 0.5) == special {
            let fill = lights.directional_lights[i].color.rgb;
            around.sky += fill;
            fill_count += 1.0;
            let way = lights.directional_lights[i].direction_to_light;
            // A special sun is unshadowed. Otherwise a fill reaches only as far as the probe sees sky its way; open sky
            // reads 0.5 + 0.5 up by the probes' weights.
            var sees = 1.0;
            if lamps.sky.x >= 0.0 && !special {
                sees = saturate((lamps.sky.x + dot(lamps.sky.yzw, way)) / max(0.5 + 0.5 * way.y, 0.1));
            }
            around.fills += sqrt(fill) * (saturate(dot(normal, way)) * sees);
        }
    }
    around.sky /= max(fill_count, 1.0);
    if fill_count == 0.0 && !special && any(around.sun_color > vec3(0.0)) {
        sky_from_ambient(&around, normal);
    }
    return around;
}

// A map lit by one sun has no fill to stand for its sky, so its shade took no sky light at all. The zone's
// ambient stands in: whole on a surface facing up, half on a wall, none facing down. A map without a sun has no
// sky to add.
fn sky_from_ambient(around: ptr<function, Around>, normal: vec3<f32>) {
    let ambient = (*around).ambient;
    (*around).sky = ambient * ambient;
    (*around).fills = ambient * (0.5 + 0.5 * normal.y);
}

struct Lit {
    matte: vec3<f32>,
    shine: vec3<f32>,
    reflection: vec3<f32>,
}

fn surface_light(albedo: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>, around: Around) -> Lit {
    let special = surface.z > 0.5;
    let sun_color = around.sun_color;
    let to_sun = around.to_sun;
    let rough = surface.x;
    let metal = surface.y;
    let classed = surface.z < 0.0;
    let dielectric = select(abs(surface.z), DIELECTRIC_F0, special);
    let a = rough * rough;
    let f0 = mix(vec3(dielectric), albedo, metal);
    let eye = to_eye(position);
    let reflected = reflect(-eye, normal);
    // Probes carry what the zone's flat ambient and the sky left under a roof stand in for without them.
    let probed = lamps.sky.x >= 0.0;
    let open = select(seen.z, saturate((seen.z - ROOFED_SKY) / (1.0 - ROOFED_SKY)), probed);
    let sky_seen = select(mix(SKY_SHADOWED, 1.0, seen.y) * open, 1.0, special);
    let zone = select(around.ambient, vec3(0.0), probed) + around.glow;
    let sky = around.sky;
    let fills = around.fills;
    let ground = GROUND_ALBEDO * sun_color;
    let facing = saturate(dot(normal, to_sun));
    // A surface turned from the sun learns nothing from the sun map, so it must not count as under open sky.
    var env = (zone * zone + mix(ground, sky, saturate(reflected.y * 0.5 + 0.5)) * seen.z) * mix(REFLECTION_SHADOWED, 1.0, seen.y * saturate(facing / SKY_FACING_FADE));
    if probed {
        env = zone * zone + sky * saturate(lamps.sky.x + dot(lamps.sky.yzw, reflected)) + lamps.bounce;
    }
    // Added over a dark texture, an untinted reflection greys it. A rough surface's keeps the texture's hue.
    let shown_color = sqrt(albedo);
    let hue = shown_color / max(max(shown_color.r, max(shown_color.g, shown_color.b)), 1e-3);
    env *= mix(hue, vec3(1.0), max(smoothstep(0.4, 0.8, 1.0 - rough), metal));
    // A dark texel is grime for a highlight only. Taking it off the diffuse too would square the texture's contrast.
    let cavity = mix(saturate(dot(sqrt(albedo), LUMINANCE) * 4.0), 1.0, metal);
    let occlusion = ao * lamps.grounded;
    let ambient = (zone + fills * sky_seen) * sqrt(ao_with_bounce(occlusion, albedo));

    let direct = seen.x;
    let nv = saturate(dot(normal, eye));
    let matte_f0 = select(f0, vec3(DIELECTRIC_F0), classed);
    let matte_rough = select(rough, DEFAULT_ROUGHNESS, classed);
    let share = vec3(1.0) - (matte_f0 + (max(vec3(1.0 - matte_rough), matte_f0) - matte_f0) * fresnel_five(nv));
    let direct_light = ambient + sqrt(sun_color) * (facing * direct) + lamps.diffuse;
    // A bounce adds in linear light, so it fills what is dark and leaves retail's colours where the map's lights
    // already reach.
    let shown = min(sqrt(direct_light * direct_light + lamps.bounce * occlusion), vec3(DIFFUSE_LIMIT));
    var lit = Lit(shown * shown * albedo * share * select(1.0 - metal, 1.0, classed), vec3(0.0), vec3(0.0));
    if gloss {
        let spec = sun_color * direct * ggx(normal, to_sun, eye, facing, saturate(a + SUN_DISC), f0);
        let e0 = rough * vec4(-1.0, -0.0275, -0.572, 0.022) + vec4(1.0, 0.0425, 1.04, -0.04);
        let ab = vec2(-1.04, 1.04) * (min(e0.x * e0.x, exp2(-9.28 * nv)) * e0.x + e0.y) + e0.zw;
        let rough_reflection = mix(1.0 - 0.6 * smoothstep(0.5, 1.0, rough), 1.0, metal);
        env *= rough_reflection * (f0 * ab.x + ab.y) * saturate(pow(nv + occlusion, exp2(-16.0 * rough * rough - 1.0)) - 1.0 + occlusion);
        lit.shine = (spec + env + lamps.specular * f0 + lamps.specular_edge * (vec3(1.0) - f0)) * cavity;
        lit.reflection = env * cavity;
    }
    return lit;
}

fn modern_surface(color: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>) -> vec3<f32> {
    return lit_surface(color, position, normal, seen, ao, gloss, lamps, surface, view_around(normal, lamps, surface));
}

// `color` is the texture as the renderer decoded it; the result is what the frame stores, with no tone curve.
fn lit_surface(color: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>, around: Around) -> vec3<f32> {
    let display = to_display(color);
    let lit = surface_light(display * display, position, normal, seen, ao, gloss, lamps, surface, around);
    return to_linear(sqrt(max(lit.matte + lit.shine, vec3(0.0))));
}

// What blending shows of a see-through surface: its highlight and reflection stay whole at any opacity, which
// takes more alpha than the texture's.
fn modern_glass(color: vec4<f32>, position: vec3<f32>, normal: vec3<f32>, seen: vec3<f32>, ao: f32, gloss: bool, lamps: LampLight, surface: vec3<f32>) -> vec4<f32> {
    let display = to_display(color.rgb);
    let lit = surface_light(display * display, position, normal, seen, ao, gloss, lamps, surface, view_around(normal, lamps, surface));
    let matte = to_linear(sqrt(max(lit.matte, vec3(0.0))));
    let highlight = max(to_linear(sqrt(max(lit.matte + lit.shine, vec3(0.0)))) - matte, vec3(0.0));
    let alpha = saturate(color.a + dot(highlight, LUMINANCE));
    return vec4((matte * color.a + highlight) / max(alpha, 1e-4), alpha);
}

// The frame's alpha tells the sky from the world for the sun's rays: a solid surface of the sky stores 0, the
// world 1.
fn sky_view() -> bool {
@if(SKY_VIEW)
    return true;
@else
    return false;
}

fn sky_mask(material_flags: u32, alpha: f32) -> f32 {
    let mode = material_flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS;
    let solid = mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_OPAQUE || mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_MASK;
    return select(alpha, 0.0, solid && sky_view());
}

// Only an alpha-blend pipeline has the def, so the glass path is compiled into no other.
fn is_see_through(material_flags: u32) -> bool {
@if(P2_SEE_THROUGH) {
    return (material_flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS) == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_BLEND;
}
@else {
    return false;
}
}

// Moving lights that are not real-time shadowed yet: added in display space as retail's are.
fn with_light(lit: vec3<f32>, color: vec3<f32>, light: vec3<f32>) -> vec3<f32> {
    if all(light <= vec3(0.0)) {
        return lit;
    }
    return to_linear(to_display(lit) + to_display(color) * light);
}

fn ambient_occlusion(frag_coord: vec2<f32>) -> f32 {
@if(SCREEN_SPACE_AMBIENT_OCCLUSION) {
    let ssao = textureLoad(screen_space_ambient_occlusion_texture, vec2<i32>(frag_coord - view.viewport.xy), 0i).r;
    return mix(1.0, ssao, AO_STRENGTH);
}
@else {
    return 1.0;
}
}

fn actor_highlight(diffuse: vec3<f32>, normal: vec3<f32>, to_light: vec3<f32>, eye: vec3<f32>) -> vec3<f32> {
    let halfway = normalize(to_light + eye);
    return ACTOR_GLOSS * diffuse * pow(max(dot(normal, halfway), 0.0), ACTOR_SHININESS);
}

fn actor_sun(sun: f32, facing: f32) -> f32 {
    return mix(1.0, sun, smoothstep(0.0, ACTOR_TERMINATOR_COS, facing));
}
