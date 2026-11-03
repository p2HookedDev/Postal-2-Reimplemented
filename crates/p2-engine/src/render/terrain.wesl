// A terrain sector's layers in one opaque pass: per layer its texture at its own UV, decals at alpha 1, the baked
// lighting (display space 0..1) as `Modulate2X` clamped per layer, then mixed over the layers below in linear space. Fog
// once on the result (fog mixes towards a constant, so once on the mix equals the mix of fogged layers).

import bevy_pbr::{
    render::{
        pbr_fragment::pbr_input_from_standard_material,
        pbr_functions::main_pass_post_lighting_processing,
        forward_io::{VertexOutput, FragmentOutput},
    },
};
import package::p2_engine::projector::{apply_decals, cluster_ranges, project, TARGET_TERRAIN};
import package::p2_engine::color::{to_display, to_linear};
import package::p2_engine::modern::{
    LUMINANCE, MODERN_ON, NO_GLOSS, NO_RELIEF, allowed_relief, ambient_occlusion, key_shading,
    masked_sun_visibility, no_relief, relief_from, sun_highlight, sun_shadowed,
};
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::mesh_bindings::mesh;

const MAX_LAYERS: u32 = 8u;

struct TerrainParams {
    u: array<vec4<f32>, MAX_LAYERS>,
    v: array<vec4<f32>, MAX_LAYERS>,
    count: vec4<u32>,
}

@if(BINDLESS) {
@group(3) @binding(133) var<storage> params_array: array<TerrainParams>;
@group(3) @binding(134) var<storage> terrain_indices: array<array<u32, 33>>;
}
@else {
@group(3) @binding(100) var<uniform> params_data: TerrainParams;
@group(3) @binding(101) var texture_0: texture_2d<f32>;
@group(3) @binding(102) var texture_0_sampler: sampler;
@group(3) @binding(103) var texture_1: texture_2d<f32>;
@group(3) @binding(104) var texture_1_sampler: sampler;
@group(3) @binding(105) var texture_2: texture_2d<f32>;
@group(3) @binding(106) var texture_2_sampler: sampler;
@group(3) @binding(107) var texture_3: texture_2d<f32>;
@group(3) @binding(108) var texture_3_sampler: sampler;
@group(3) @binding(109) var texture_4: texture_2d<f32>;
@group(3) @binding(110) var texture_4_sampler: sampler;
@group(3) @binding(111) var texture_5: texture_2d<f32>;
@group(3) @binding(112) var texture_5_sampler: sampler;
@group(3) @binding(113) var texture_6: texture_2d<f32>;
@group(3) @binding(114) var texture_6_sampler: sampler;
@group(3) @binding(115) var texture_7: texture_2d<f32>;
@group(3) @binding(116) var texture_7_sampler: sampler;
@group(3) @binding(119) var alpha_map_1: texture_2d<f32>;
@group(3) @binding(120) var alpha_map_1_sampler: sampler;
@group(3) @binding(121) var alpha_map_2: texture_2d<f32>;
@group(3) @binding(122) var alpha_map_2_sampler: sampler;
@group(3) @binding(123) var alpha_map_3: texture_2d<f32>;
@group(3) @binding(124) var alpha_map_3_sampler: sampler;
@group(3) @binding(125) var alpha_map_4: texture_2d<f32>;
@group(3) @binding(126) var alpha_map_4_sampler: sampler;
@group(3) @binding(127) var alpha_map_5: texture_2d<f32>;
@group(3) @binding(128) var alpha_map_5_sampler: sampler;
@group(3) @binding(129) var alpha_map_6: texture_2d<f32>;
@group(3) @binding(130) var alpha_map_6_sampler: sampler;
@group(3) @binding(131) var alpha_map_7: texture_2d<f32>;
@group(3) @binding(132) var alpha_map_7_sampler: sampler;
}

@if(BINDLESS)
fn material_slot(in: VertexOutput) -> u32 {
    return mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
}

@if(BINDLESS)
fn params(in: VertexOutput) -> TerrainParams {
    return params_array[terrain_indices[material_slot(in)][0]];
}
@if(!BINDLESS)
fn params(in: VertexOutput) -> TerrainParams {
    return params_data;
}

@if(BINDLESS)
fn layer_texture(in: VertexOutput, k: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec4<f32> {
    let s = material_slot(in);
    return textureSampleGrad(
        bindless_textures_2d[terrain_indices[s][1u + 2u * k]],
        bindless_samplers_filtering[terrain_indices[s][2u + 2u * k]],
        uv, gx, gy
    );
}
@if(BINDLESS)
fn layer_size(in: VertexOutput, k: u32) -> vec2<f32> {
    return vec2<f32>(textureDimensions(bindless_textures_2d[terrain_indices[material_slot(in)][1u + 2u * k]], 0));
}
@if(BINDLESS)
fn layer_alpha(in: VertexOutput, k: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> f32 {
    let s = material_slot(in);
    return textureSampleGrad(
        bindless_textures_2d[terrain_indices[s][17u + 2u * k]],
        bindless_samplers_filtering[terrain_indices[s][18u + 2u * k]],
        uv, gx, gy
    ).r;
}
@if(!BINDLESS)
fn layer_texture(in: VertexOutput, k: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec4<f32> {
    var texel: vec4<f32>;
    switch k {
        case 0u: { texel = textureSampleGrad(texture_0, texture_0_sampler, uv, gx, gy); }
        case 1u: { texel = textureSampleGrad(texture_1, texture_1_sampler, uv, gx, gy); }
        case 2u: { texel = textureSampleGrad(texture_2, texture_2_sampler, uv, gx, gy); }
        case 3u: { texel = textureSampleGrad(texture_3, texture_3_sampler, uv, gx, gy); }
        case 4u: { texel = textureSampleGrad(texture_4, texture_4_sampler, uv, gx, gy); }
        case 5u: { texel = textureSampleGrad(texture_5, texture_5_sampler, uv, gx, gy); }
        case 6u: { texel = textureSampleGrad(texture_6, texture_6_sampler, uv, gx, gy); }
        default: { texel = textureSampleGrad(texture_7, texture_7_sampler, uv, gx, gy); }
    }
    return texel;
}
@if(!BINDLESS)
fn layer_size(in: VertexOutput, k: u32) -> vec2<f32> {
    var size: vec2<u32>;
    switch k {
        case 0u: { size = textureDimensions(texture_0, 0); }
        case 1u: { size = textureDimensions(texture_1, 0); }
        case 2u: { size = textureDimensions(texture_2, 0); }
        case 3u: { size = textureDimensions(texture_3, 0); }
        case 4u: { size = textureDimensions(texture_4, 0); }
        case 5u: { size = textureDimensions(texture_5, 0); }
        case 6u: { size = textureDimensions(texture_6, 0); }
        default: { size = textureDimensions(texture_7, 0); }
    }
    return vec2<f32>(size);
}
@if(!BINDLESS)
fn layer_alpha(in: VertexOutput, k: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> f32 {
    var texel: vec4<f32>;
    switch k {
        case 1u: { texel = textureSampleGrad(alpha_map_1, alpha_map_1_sampler, uv, gx, gy); }
        case 2u: { texel = textureSampleGrad(alpha_map_2, alpha_map_2_sampler, uv, gx, gy); }
        case 3u: { texel = textureSampleGrad(alpha_map_3, alpha_map_3_sampler, uv, gx, gy); }
        case 4u: { texel = textureSampleGrad(alpha_map_4, alpha_map_4_sampler, uv, gx, gy); }
        case 5u: { texel = textureSampleGrad(alpha_map_5, alpha_map_5_sampler, uv, gx, gy); }
        case 6u: { texel = textureSampleGrad(alpha_map_6, alpha_map_6_sampler, uv, gx, gy); }
        default: { texel = textureSampleGrad(alpha_map_7, alpha_map_7_sampler, uv, gx, gy); }
    }
    return texel.r;
}

@fragment
fn fragment(vertex_output: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // The vertex colour is the lighting, applied per layer below (no lighting: the texture as is, 2 × 0.5); its alpha
    // the sun mask (1 where the bake's sun reached; modern graphics only).
    var input = vertex_output;
    var light = vec3(0.5);
    var mask = 0.0;
    @if(VERTEX_COLORS) {
        light = input.color.rgb;
        mask = input.color.a;
        input.color = vec4(1.0);
    }
    var pbr_input = pbr_input_from_standard_material(input, is_front);

    let position = vertex_output.world_position.xyz;
    let position_dx = dpdx(position);
    let position_dy = dpdy(position);
    let alpha_uv = vertex_output.uv_b;
    let alpha_dx = dpdx(alpha_uv);
    let alpha_dy = dpdy(alpha_uv);

    let terrain = params(vertex_output);
    let count = min(terrain.count.x, MAX_LAYERS);
    let modern = (terrain.count.y & MODERN_ON) != 0u;
    let normal = normalize(pbr_input.world_normal);
    var sun_seen = 1.0;
    var linear_light = vec3(0.0);
    if modern {
        sun_seen = masked_sun_visibility(mask, pbr_input.world_position, normal, pbr_input.frag_coord.xy);
        light = sun_shadowed(light, mask, sun_seen, normal);
        light *= ambient_occlusion(pbr_input.frag_coord.xy);
        linear_light = to_linear(2.0 * min(light, vec3(1.0)));
    }
    let ranges = cluster_ranges(pbr_input);
    var shine = vec3(0.0);
    var color = vec3(0.0);
    var texture = vec3(0.0);
    for (var k = 0u; k < count; k += 1u) {
        let u = terrain.u[k];
        let v = terrain.v[k];
        let uv = vec2(dot(u.xyz, position) + u.w, dot(v.xyz, position) + v.w);
        let gx = vec2(dot(u.xyz, position_dx), dot(v.xyz, position_dx));
        let gy = vec2(dot(u.xyz, position_dy), dot(v.xyz, position_dy));
        let texel = layer_texture(vertex_output, k, uv, gx, gy);
        var alpha = 1.0;
        if k > 0u {
            alpha = texel.a * layer_alpha(vertex_output, k, alpha_uv, alpha_dx, alpha_dy);
        }
        if alpha <= 0.0 {
            continue;
        }
        var layer_light = light;
        if modern {
            var relief = no_relief(normal);
            if (terrain.count.y & (NO_RELIEF | NO_GLOSS)) != (NO_RELIEF | NO_GLOSS) {
                let size = layer_size(vertex_output, k);
                let step = max(1.0 / size, max(abs(gx), abs(gy)));
                let l = dot(layer_texture(vertex_output, k, uv - vec2(step.x, 0.0), gx, gy).rgb, LUMINANCE);
                let r = dot(layer_texture(vertex_output, k, uv + vec2(step.x, 0.0), gx, gy).rgb, LUMINANCE);
                let t = dot(layer_texture(vertex_output, k, uv - vec2(0.0, step.y), gx, gy).rgb, LUMINANCE);
                let b = dot(layer_texture(vertex_output, k, uv + vec2(0.0, step.y), gx, gy).rgb, LUMINANCE);
                let slope = vec2(r - l, b - t) / 2.0;
                relief = allowed_relief(
                    relief_from(normal, position, position_dx, position_dy, gx, gy, size, slope, dot(texel.rgb, LUMINANCE)),
                    normal,
                    terrain.count.y,
                );
            }
            let key = key_shading(normal, relief, position);
            layer_light = key.x * light;
            let sun = sun_highlight(relief, mask, sun_seen, position, normal);
            shine = mix(shine, key.y * linear_light + sun, alpha);
        }
        pbr_input.material.base_color = vec4(texel.rgb, 1.0);
        apply_decals(&pbr_input, ranges);
        let lit = min(to_display(pbr_input.material.base_color.rgb) * 2.0 * layer_light, vec3(1.0));
        color = mix(color, to_linear(lit), alpha);
        texture = mix(texture, pbr_input.material.base_color.rgb, alpha);
    }
    color += shine;
    let projected = project(pbr_input, ranges, texture, vec4(color, 1.0), TARGET_TERRAIN);
    pbr_input.material.base_color = projected;

    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, projected);
    return out;
}
