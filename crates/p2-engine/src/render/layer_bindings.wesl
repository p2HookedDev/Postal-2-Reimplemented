// Lit materials' layer bindings: data at 100 (bindless array 107), detail 101/102, specular cube
// 103/104, 2D specular 105/106, bindless index table 108. Each shader declares its own data.

import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings::view,
    pbr_functions::main_pass_post_lighting_processing,
    pbr_types,
};
import package::p2_engine::shader_layers::{detail, glow, specular};
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d, bindless_textures_cube};
@if(BINDLESS)
import bevy_pbr::render::mesh_bindings::mesh;

struct ShaderLayers {
    detail: vec4<f32>,
    specular: vec4<f32>,
    specular_u: vec4<f32>,
    specular_v: vec4<f32>,
}

@if(BINDLESS) {
struct LayerIndices {
    data: u32,
    detail: u32,
    detail_sampler: u32,
    specular: u32,
    specular_sampler: u32,
    flat_specular: u32,
    flat_specular_sampler: u32,
    @if(TERRAIN_LAYER)
    unused_107: u32,
    @if(TERRAIN_LAYER)
    unused_108: u32,
    @if(TERRAIN_LAYER)
    alpha_map: u32,
    @if(TERRAIN_LAYER)
    alpha_map_sampler: u32,
}
@group(3) @binding(108) var<storage> layer_indices: array<LayerIndices>;
}
@else {
@group(3) @binding(101) var detail_texture: texture_2d<f32>;
@group(3) @binding(102) var detail_sampler: sampler;
@group(3) @binding(103) var specular_texture: texture_cube<f32>;
@group(3) @binding(104) var specular_sampler: sampler;
@group(3) @binding(105) var flat_specular_texture: texture_2d<f32>;
@group(3) @binding(106) var flat_specular_sampler: sampler;
}

@if(BINDLESS)
fn indices(in: VertexOutput) -> LayerIndices {
    return layer_indices[mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu];
}

fn premultiplied(flags: u32) -> bool {
    return (flags & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS) == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_PREMULTIPLIED;
}

// `color` is the linear lit colour. Adds the glow, detail × 2 and the specular, masked by
// `base_alpha` taken before `alpha_discard`. The multi-pass form draws as one premultiplied output.
fn apply_layers(in: VertexOutput, color: vec4<f32>, base_alpha: f32, pbr_input: pbr_types::PbrInput, layers: ShaderLayers) -> vec4<f32> {
    let emissive = glow(in, pbr_input.material.flags);
    let mode = u32(layers.specular.x + 0.5);
    let multi_pass = premultiplied(pbr_input.material.flags);
    var out = color.rgb;
    if !multi_pass {
        out += emissive.rgb;
    }
@if(VERTEX_UVS_A) {
    if layers.detail.x != 0.0 && layers.detail.w == 0.0 {
        let uv = (pbr_input.material.uv_transform * vec3(in.uv, 1.0)).xy * layers.detail.x;
@if(BINDLESS)
        let index = indices(in);
@if(BINDLESS)
        let texel = textureSampleBias(bindless_textures_2d[index.detail], bindless_samplers_filtering[index.detail_sampler], uv, layers.detail.y);
@else
        let texel = textureSampleBias(detail_texture, detail_sampler, uv, layers.detail.y);
        out = detail(out, texel.rgb);
    }
}
    var alpha = color.a;
    if multi_pass {
        let k = select(emissive.a * color.a, 0.0, mode == 4u);
        out = color.a * (out * (1.0 - k) + emissive.rgb);
        alpha = 1.0 - (1.0 - color.a) * (1.0 - k);
    }
    if mode != 0u {
        let normal = normalize(pbr_input.world_normal);
        let to_eye = normalize(view.world_position.xyz - pbr_input.world_position.xyz);
        let reflected = reflect(-to_eye, normal);
        var direction = reflected.xzy;
        if layers.specular.y < 0.5 {
            let camera = (view.view_from_world * vec4(reflected, 0.0)).xyz;
            direction = vec3(camera.x, camera.y, -camera.z);
        }
        var mask = 1.0;
        switch mode {
            case 2u: { mask = base_alpha; }
            case 3u: { mask = layers.specular.z; }
            case 4u: { mask = emissive.a; }
            default: {}
        }
@if(BINDLESS)
        let index = indices(in);
        var texel: vec4<f32>;
        if layers.specular.w < 0.5 {
@if(BINDLESS)
            texel = textureSampleLevel(bindless_textures_cube[index.specular], bindless_samplers_filtering[index.specular_sampler], direction, 0.0);
@else
            texel = textureSampleLevel(specular_texture, specular_sampler, direction, 0.0);
        } else {
            var uv = direction.xy;
@if(VERTEX_UVS_A)
            if layers.specular.w > 1.5 {
                uv = (pbr_input.material.uv_transform * vec3(in.uv, 1.0)).xy;
            }
            uv = vec2(dot(layers.specular_u.xyz, vec3(uv, 1.0)), dot(layers.specular_v.xyz, vec3(uv, 1.0)));
@if(BINDLESS)
            texel = textureSampleBias(bindless_textures_2d[index.flat_specular], bindless_samplers_filtering[index.flat_specular_sampler], uv, view.mip_bias);
@else
            texel = textureSampleBias(flat_specular_texture, flat_specular_sampler, uv, view.mip_bias);
        }
        if mode == 5u {
            out *= texel.rgb;
        } else {
            out = specular(out, texel.rgb, mask);
        }
    }
    return vec4(out, alpha);
}

// Bevy's fog (`main_pass_post_lighting_processing`) on `color`. Bevy mixes towards the whole fog colour; a premultiplied
// surface covers only its alpha's share of it, so the rest comes off (fog is affine in the colour; no tone mapping in
// the shader). The specular fades with the fog as towards black (unverified).
fn post_lighting(pbr_input: pbr_types::PbrInput, color: vec4<f32>) -> vec4<f32> {
    let out = main_pass_post_lighting_processing(pbr_input, color);
    if !premultiplied(pbr_input.material.flags) {
        return out;
    }
    let fog = main_pass_post_lighting_processing(pbr_input, vec4(0.0, 0.0, 0.0, color.a)).rgb;
    return vec4(out.rgb - (1.0 - color.a) * fog, out.a);
}
