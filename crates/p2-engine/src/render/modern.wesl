// Modern graphics shading, read only while the switch is on. Sun shadows are a mask over the bake:
// the sun's map holds dynamic objects only, and where it hides the sun the baked term comes out.

import package::p2_engine::color::{to_display, to_linear};
import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings::{view, lights},
    mesh_view_types::{DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT, DIRECTIONAL_LIGHT_FLAGS_CONTACT_SHADOWS_ENABLED_BIT},
    pbr_bindings,
    pbr_types,
    shadows,
};
@if(SCREEN_SPACE_AMBIENT_OCCLUSION)
import bevy_pbr::render::mesh_view_bindings::screen_space_ambient_occlusion_texture;
@if(CONTACT_SHADOWS)
import bevy_pbr::render::mesh_view_bindings::contact_shadows_settings;
@if(CONTACT_SHADOWS && DEPTH_PREPASS)
import bevy_pbr::render::pbr_functions::calculate_contact_shadow;
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};

const RELIEF_STRENGTH: f32 = 2.0;
const RELIEF_MAX_TILT: f32 = 0.35;
const RELIEF_NEAR: f32 = 512.0;
const RELIEF_FAR: f32 = 2048.0;
const RELIEF_MINIFIED_FROM: f32 = 1.0;
const RELIEF_MINIFIED_TO: f32 = 3.0;
const RELIEF_SKEW_FROM: f32 = 0.05;
const RELIEF_SKEW_TO: f32 = 0.2;
const RELIEF_LIGHT_MIN_COS: f32 = 0.25;
const RELIEF_LIGHT_RANGE: f32 = 0.3;
const KEY_DIRECTION: vec3<f32> = vec3(0.458088, 0.814379, 0.356291);
const KEY_SPECULAR: f32 = 0.5;
const SUN_SPECULAR: f32 = 2.0;
const SPECULAR_STRENGTH: f32 = 0.35;
const GLOSS_FROM: f32 = 0.25;
const GLOSS_FULL: f32 = 0.65;
const ROUGH_FAR: f32 = 0.5;
const ACTOR_GLOSS: f32 = 0.1;
// Skinned actors' sun shadow fades in over N · L from 0 to this (the interpolated normal's): near the terminator a
// low-poly mesh's flat faces shadow themselves where its smooth normal still faces the sun, which showed its triangles.
const ACTOR_TERMINATOR_COS: f32 = 0.35;
const ACTOR_SHININESS: f32 = 20.0;
const AO_STRENGTH: f32 = 0.8;
const SHININESS_SMOOTH: f32 = 48.0;
const SHININESS_ROUGH: f32 = 10.0;
const ROUGH_SLOPE: f32 = 0.15;
const LUMINANCE: vec3<f32> = vec3(0.2126, 0.7152, 0.0722);

struct Relief {
    normal: vec3<f32>,
    gloss: f32,
    shininess: f32,
}

fn no_relief(normal: vec3<f32>) -> Relief {
    return Relief(normal, 0.0, SHININESS_ROUGH);
}

const MODERN_ON: u32 = 1u;
const NO_RELIEF: u32 = 2u;
const NO_GLOSS: u32 = 4u;

fn modern_bits(flag: f32) -> u32 {
    return u32(max(flag, 0.0) + 0.5);
}

fn is_modern(flag: f32) -> bool {
    return (modern_bits(flag) & MODERN_ON) != 0u;
}

fn allows_gloss(flag: f32) -> bool {
    let bits = modern_bits(flag);
    return (bits & MODERN_ON) != 0u && (bits & NO_GLOSS) == 0u;
}

fn actor_highlight(diffuse: vec3<f32>, normal: vec3<f32>, to_light: vec3<f32>, eye: vec3<f32>) -> vec3<f32> {
    let halfway = normalize(to_light + eye);
    return ACTOR_GLOSS * diffuse * pow(max(dot(normal, halfway), 0.0), ACTOR_SHININESS);
}

fn actor_sun(sun: f32, facing: f32) -> f32 {
    return mix(1.0, sun, smoothstep(0.0, ACTOR_TERMINATOR_COS, facing));
}

fn allowed_relief(relief: Relief, normal: vec3<f32>, bits: u32) -> Relief {
    var allowed = relief;
    if (bits & NO_RELIEF) != 0u {
        allowed.normal = normal;
    }
    if (bits & NO_GLOSS) != 0u {
        allowed.gloss = 0.0;
    }
    return allowed;
}

fn relief_from(
    normal: vec3<f32>,
    position: vec3<f32>,
    dp_dx: vec3<f32>,
    dp_dy: vec3<f32>,
    duv_dx: vec2<f32>,
    duv_dy: vec2<f32>,
    size: vec2<f32>,
    slope: vec2<f32>,
    luminance: f32,
) -> Relief {
    let dp_dy_perp = cross(dp_dy, normal);
    let dp_dx_perp = cross(normal, dp_dx);
    let flip = select(-1.0, 1.0, dot(dp_dx, dp_dy_perp) >= 0.0);
    let tangent = dp_dy_perp * duv_dx.x + dp_dx_perp * duv_dy.x;
    let bitangent = dp_dy_perp * duv_dx.y + dp_dx_perp * duv_dy.y;
    let scale = flip * inverseSqrt(max(max(dot(tangent, tangent), dot(bitangent, bitangent)), 1e-20));
    let skew = abs(duv_dx.x * duv_dy.y - duv_dx.y * duv_dy.x) / max(length(duv_dx) * length(duv_dy), 1e-20);
    let texels = max(length(duv_dx * size), length(duv_dy * size));
    let fade = (1.0 - smoothstep(RELIEF_NEAR, RELIEF_FAR, distance(view.world_position.xyz, position)))
        * (1.0 - smoothstep(RELIEF_MINIFIED_FROM, RELIEF_MINIFIED_TO, texels))
        * smoothstep(RELIEF_SKEW_FROM, RELIEF_SKEW_TO, skew);
    var tilt = RELIEF_STRENGTH * fade * scale * (slope.x * tangent + slope.y * bitangent);
    let amount = length(tilt);
    if amount > RELIEF_MAX_TILT {
        tilt *= RELIEF_MAX_TILT / amount;
    }
    let display = pow(max(luminance, 0.0), 1.0 / 2.2);
    let rough = mix(ROUGH_FAR, saturate(length(slope) / ROUGH_SLOPE), fade);
    let gloss = SPECULAR_STRENGTH * smoothstep(GLOSS_FROM, GLOSS_FULL, display) * (1.0 - rough);
    let shininess = mix(SHININESS_SMOOTH, SHININESS_ROUGH, rough);
    return Relief(normalize(normal - tilt), gloss, shininess);
}

fn base_relief(in: VertexOutput, pbr_input: pbr_types::PbrInput, normal: vec3<f32>, layer_specular: f32, flag: f32) -> Relief {
@if(VERTEX_UVS_A) {
    let bits = modern_bits(flag);
    let neither = (bits & (NO_RELIEF | NO_GLOSS)) == (NO_RELIEF | NO_GLOSS);
    if neither || (pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT) == 0u {
        return no_relief(normal);
    }
@if(BINDLESS)
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
@if(BINDLESS)
    let texture = bindless_textures_2d[material_indices[slot].base_color_texture];
@else
    let texture = pbr_bindings::base_color_texture;
@if(BINDLESS)
    let sampler_ = bindless_samplers_filtering[material_indices[slot].base_color_sampler];
@else
    let sampler_ = pbr_bindings::base_color_sampler;
    let uv = (pbr_input.material.uv_transform * vec3(in.uv, 1.0)).xy;
    let gx = dpdx(uv);
    let gy = dpdy(uv);
    let size = vec2<f32>(textureDimensions(texture, 0));
    let step = max(1.0 / size, max(abs(gx), abs(gy)));
    let a = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(-1.0, -1.0), gx, gy).rgb, LUMINANCE);
    let b = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(0.0, -1.0), gx, gy).rgb, LUMINANCE);
    let c = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(1.0, -1.0), gx, gy).rgb, LUMINANCE);
    let d = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(-1.0, 0.0), gx, gy).rgb, LUMINANCE);
    let f = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(1.0, 0.0), gx, gy).rgb, LUMINANCE);
    let g = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(-1.0, 1.0), gx, gy).rgb, LUMINANCE);
    let h = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(0.0, 1.0), gx, gy).rgb, LUMINANCE);
    let i = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(1.0, 1.0), gx, gy).rgb, LUMINANCE);
    let slope = vec2((c + 2.0 * f + i) - (a + 2.0 * d + g), (g + 2.0 * h + i) - (a + 2.0 * b + c)) / 8.0;
    let mean = (a + b + c + d + f + g + h + i) / 8.0;
    let position = pbr_input.world_position.xyz;
    var relief = relief_from(normal, position, dpdx(position), dpdy(position), gx, gy, size, slope, mean);
    if layer_specular > 0.5 {
        relief.gloss = 0.0;
    }
    return allowed_relief(relief, normal, bits);
}
@else {
    return no_relief(normal);
}
}

// The first-order change of N · L over the relief relative to the flat surface's, so flat texels keep the retail light
// and a texture's slopes cancel on average. Lights behind the surface (BSP passes take those) are left as they are.
fn relief_light(normal: vec3<f32>, bumped: vec3<f32>, to_light: vec3<f32>) -> f32 {
    let flat_cos = dot(normal, to_light);
    if flat_cos <= 0.0 {
        return 1.0;
    }
    let up = max(dot(bumped, normal), 1e-3);
    let lean = (bumped - up * normal) / up;
    let share = 1.0 + dot(lean, to_light) / max(flat_cos, RELIEF_LIGHT_MIN_COS);
    return clamp(share, 1.0 - RELIEF_LIGHT_RANGE, 1.0 + RELIEF_LIGHT_RANGE);
}

fn key_light(normal: vec3<f32>) -> vec3<f32> {
    var key = KEY_DIRECTION;
    let sun = sun_index();
    if sun < lights.n_directional_lights {
        key = lights.directional_lights[sun].direction_to_light;
    }
    var across = key - dot(key, normal) * normal;
    if dot(across, across) < 1e-6 {
        // The key along the normal: any side will do, but not one along the normal (its cross would be 0, the key NaN
        // and the surface black: a wall facing ±Z under a sun square to it).
        let side = select(vec3(0.0, 0.0, 1.0), vec3(1.0, 0.0, 0.0), abs(normal.z) > 0.9);
        across = cross(normal, side);
    }
    return normalize(normal + normalize(across));
}

fn to_eye(world_position: vec3<f32>) -> vec3<f32> {
    let to_camera = view.world_position.xyz - world_position;
    return to_camera / max(length(to_camera), 1e-6);
}

fn highlight(relief: Relief, to_light: vec3<f32>, eye: vec3<f32>) -> f32 {
    let facing = dot(relief.normal, to_light);
    if facing <= 0.0 || relief.gloss <= 0.0 {
        return 0.0;
    }
    let halfway = normalize(to_light + eye);
    return relief.gloss * pow(max(dot(relief.normal, halfway), 0.0), relief.shininess) * saturate(4.0 * facing);
}

// From the shadowing sun's cascaded map alone; 1 without one in this view. Static receivers use this: the contact shadow
// sees the static world in the depth buffer too, and took the sun out of window patches the bake had already shaded.
fn sun_map_visibility(world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    let sun = sun_index();
    if sun >= lights.n_directional_lights {
        return 1.0;
    }
    let view_z = dot(vec4(
        view.view_from_world[0].z,
        view.view_from_world[1].z,
        view.view_from_world[2].z,
        view.view_from_world[3].z
    ), world_position);
    return shadows::fetch_directional_shadow(sun, world_position, normal, view_z, frag_coord);
}

fn sun_visibility(world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    var visibility = sun_map_visibility(world_position, normal, frag_coord);
    let sun = sun_index();
    @if(CONTACT_SHADOWS && DEPTH_PREPASS)
    if visibility > 0.0 && sun < lights.n_directional_lights && contact_shadows_settings.linear_steps > 0u
        && (lights.directional_lights[sun].flags & DIRECTIONAL_LIGHT_FLAGS_CONTACT_SHADOWS_ENABLED_BIT) != 0u {
        visibility *= calculate_contact_shadow(
            world_position.xyz,
            frag_coord,
            lights.directional_lights[sun].direction_to_light,
            contact_shadows_settings.linear_steps
        );
    }
    return visibility;
}

fn masked_sun_visibility(mask: f32, world_position: vec4<f32>, normal: vec3<f32>, frag_coord: vec2<f32>) -> f32 {
    if mask <= 0.0 {
        return 1.0;
    }
    return sun_map_visibility(world_position, normal, frag_coord);
}

// `light`: display space before `Modulate2X`. Light − sun term × mask × (1 − `visibility`), at least 0; the sun term is
// the sun's colour × max(N · L, 0), so a blocked lumel keeps exactly its baked light without the sun.
fn sun_shadowed(light: vec3<f32>, mask: f32, visibility: f32, normal: vec3<f32>) -> vec3<f32> {
    let blocked = mask * (1.0 - visibility);
    if blocked <= 0.0 {
        return light;
    }
    return without_sun(light, blocked, normal);
}

fn sun_shadowed_linear(light: vec3<f32>, mask: f32, visibility: f32, normal: vec3<f32>) -> vec3<f32> {
    let blocked = mask * (1.0 - visibility);
    if blocked <= 0.0 {
        return light;
    }
    return to_linear(2.0 * without_sun(to_display(light) / 2.0, blocked, normal));
}

fn without_sun(light: vec3<f32>, blocked: f32, normal: vec3<f32>) -> vec3<f32> {
    let sun = lights.directional_lights[sun_index()];
    let facing = max(dot(normal, sun.direction_to_light), 0.0);
    return max(light - sun.color.rgb * facing * blocked, vec3(0.0));
}

fn key_shading(normal: vec3<f32>, relief: Relief, world_position: vec3<f32>) -> vec2<f32> {
    let to_light = key_light(normal);
    return vec2(relief_light(normal, relief.normal, to_light), KEY_SPECULAR * highlight(relief, to_light, to_eye(world_position)));
}

fn sun_index() -> u32 {
    for (var i = 0u; i < lights.n_directional_lights; i += 1u) {
        if (lights.directional_lights[i].flags & DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
            return i;
        }
    }
    return lights.n_directional_lights;
}

fn sun_highlight(relief: Relief, mask: f32, visibility: f32, world_position: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let index = sun_index();
    if mask <= 0.0 || index >= lights.n_directional_lights {
        return vec3(0.0);
    }
    let sun = lights.directional_lights[index];
    if dot(normal, sun.direction_to_light) <= 0.0 {
        return vec3(0.0);
    }
    return SUN_SPECULAR * highlight(relief, sun.direction_to_light, to_eye(world_position)) * sun.color.rgb * visibility;
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
