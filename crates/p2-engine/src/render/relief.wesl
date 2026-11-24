import package::p2_engine::modern_base::{LUMINANCE, NO_RELIEF, modern_bits};
import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings::view,
    pbr_bindings,
    pbr_types,
};
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};

const RELIEF_STRENGTH: f32 = 3.0;
const RELIEF_MAX_TILT: f32 = 0.45;
const RELIEF_NEAR: f32 = 1024.0;
const RELIEF_FAR: f32 = 4096.0;
const RELIEF_MINIFIED_FROM: f32 = 1.0;
const RELIEF_MINIFIED_TO: f32 = 3.0;
const RELIEF_SKEW_FROM: f32 = 0.05;
const RELIEF_SKEW_TO: f32 = 0.2;

fn relief_from(
    normal: vec3<f32>,
    position: vec3<f32>,
    dp_dx: vec3<f32>,
    dp_dy: vec3<f32>,
    duv_dx: vec2<f32>,
    duv_dy: vec2<f32>,
    size: vec2<f32>,
    slope: vec2<f32>,
) -> vec3<f32> {
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
    return normalize(normal - tilt);
}

// Past these a slope shows nothing, so its texture taps are saved.
fn relief_shows(position: vec3<f32>, duv_dx: vec2<f32>, duv_dy: vec2<f32>, size: vec2<f32>) -> bool {
    let texels = max(length(duv_dx * size), length(duv_dy * size));
    return texels < RELIEF_MINIFIED_TO && distance(view.world_position.xyz, position) < RELIEF_FAR;
}

fn base_relief(in: VertexOutput, pbr_input: pbr_types::PbrInput, normal: vec3<f32>, flag: f32) -> vec3<f32> {
@if(VERTEX_UVS_A) {
    if (modern_bits(flag) & NO_RELIEF) != 0u || (pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_BASE_COLOR_TEXTURE_BIT) == 0u {
        return normal;
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
    let position = pbr_input.world_position.xyz;
    let position_dx = dpdx(position);
    let position_dy = dpdy(position);
    if !relief_shows(position, gx, gy, size) {
        return normal;
    }
    let step = max(1.0 / size, max(abs(gx), abs(gy)));
    let b = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(0.0, -1.0), gx, gy).rgb, LUMINANCE);
    let d = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(-1.0, 0.0), gx, gy).rgb, LUMINANCE);
    let f = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(1.0, 0.0), gx, gy).rgb, LUMINANCE);
    let h = dot(textureSampleGrad(texture, sampler_, uv + step * vec2(0.0, 1.0), gx, gy).rgb, LUMINANCE);
    let slope = vec2(f - d, h - b) / 2.0;
    return relief_from(normal, position, position_dx, position_dy, gx, gy, size, slope);
}
@else {
    return normal;
}
}

struct Relief {
    normal: vec3<f32>,
    gloss: f32,
    shininess: f32,
}

fn highlight(relief: Relief, to_light: vec3<f32>, eye: vec3<f32>) -> f32 {
    let facing = dot(relief.normal, to_light);
    if facing <= 0.0 || relief.gloss <= 0.0 {
        return 0.0;
    }
    let halfway = normalize(to_light + eye);
    return relief.gloss * pow(max(dot(relief.normal, halfway), 0.0), relief.shininess) * saturate(4.0 * facing);
}
