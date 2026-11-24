// Bevy's material prepass, unchanged but for the normal's alpha: it holds the surface's reflecting class for the
// screen-space reflections, 0 tile or glass, 1/3 metal, 2/3 wood, 1 nothing. Matches `reflections.wesl`.

import bevy_pbr::{
    render::{
        pbr_prepass_functions,
        pbr_bindings,
        pbr_types,
        pbr_functions,
        pbr_functions::SampleBias,
        mesh_bindings::mesh,
        mesh_view_bindings::view,
    },
    prepass::io as prepass_io,
};

import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};

@if(BINDLESS)
import bevy_pbr::render::pbr_bindings::material_indices;
import package::p2_render::layer_types::{ActorShading, LayerIndices, ShaderLayers};

@if(BINDLESS)
@group(3) @binding(108) var<storage> layer_indices: array<LayerIndices>;
@if(ACTOR_SHADING && BINDLESS)
@group(3) @binding(107) var<storage> shading_array: array<ActorShading>;
@if(ACTOR_SHADING && !BINDLESS)
@group(3) @binding(100) var<uniform> shading_data: ActorShading;
@if(!ACTOR_SHADING && BINDLESS)
@group(3) @binding(107) var<storage> layers_array: array<ShaderLayers>;
@if(!ACTOR_SHADING && !BINDLESS)
@group(3) @binding(100) var<uniform> layers_data: ShaderLayers;

fn blend_of(in: prepass_io::VertexOutput) -> vec4<f32> {
@if(BINDLESS)
    let data = layer_indices[mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu].data;
@if(ACTOR_SHADING && BINDLESS)
    return shading_array[data].layers.blend;
@if(ACTOR_SHADING && !BINDLESS)
    return shading_data.layers.blend;
@if(!ACTOR_SHADING && BINDLESS)
    return layers_array[data].blend;
@if(!ACTOR_SHADING && !BINDLESS)
    return layers_data.blend;
}

// `blend.y` is the class index of `material_class.rs`; a zero `blend.z` is a material without a class.
fn class_code(blend: vec4<f32>) -> f32 {
    if blend.z <= 0.0 {
        return 1.0;
    }
    switch u32(round(blend.y)) {
        case 2u: {
            return 0.0;
        }
        case 1u: {
            return 1.0 / 3.0;
        }
        case 3u: {
            return 2.0 / 3.0;
        }
        default: {
            return 1.0;
        }
    }
}

@if(PREPASS_FRAGMENT)
@fragment
fn fragment(in: prepass_io::VertexOutput, @builtin(front_facing) is_front: bool) -> prepass_io::FragmentOutput {
@if(BINDLESS)
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
@if(BINDLESS)
    let flags = pbr_bindings::material_array[material_indices[slot].material].flags;
@if(BINDLESS)
    let uv_transform = pbr_bindings::material_array[material_indices[slot].material].uv_transform;
@if(!BINDLESS)
    let flags = pbr_bindings::material.flags;
@if(!BINDLESS)
    let uv_transform = pbr_bindings::material.uv_transform;

@if(VISIBILITY_RANGE_DITHER)
    pbr_functions::visibility_range_dither(in.position, in.visibility_range_dither);

    pbr_prepass_functions::prepass_sample_color_and_alpha_discard(in);

    var out: prepass_io::FragmentOutput;

@if(UNCLIPPED_DEPTH_ORTHO_EMULATION)
    out.frag_depth = in.unclipped_depth;

@if(NORMAL_PREPASS)
    if (flags & pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        let double_sided = (flags & pbr_types::STANDARD_MATERIAL_FLAGS_DOUBLE_SIDED_BIT) != 0u;
        let mesh_flags = mesh[in.instance_index].flags;
        let is_logically_front = pbr_functions::winding_corrected_front_facing(mesh_flags, is_front);
        let world_normal = pbr_functions::prepare_world_normal(in.world_normal, double_sided, is_logically_front);
        var normal = world_normal;

@if(VERTEX_UVS) {
@if(VERTEX_TANGENTS) {
@if(STANDARD_MATERIAL_NORMAL_MAP) {
@if(STANDARD_MATERIAL_NORMAL_MAP_UV_B)
        let uv = (uv_transform * vec3(in.uv_b, 1.0)).xy;
@else
        let uv = (uv_transform * vec3(in.uv, 1.0)).xy;
        var bias: SampleBias;
        bias.mip_bias = view.mip_bias;
@if(BINDLESS)
        let texture = bindless_textures_2d[material_indices[slot].normal_map_texture];
@else
        let texture = pbr_bindings::normal_map_texture;
@if(BINDLESS)
        let sampler_ = bindless_samplers_filtering[material_indices[slot].normal_map_sampler];
@else
        let sampler_ = pbr_bindings::normal_map_sampler;
        let Nt = textureSampleBias(texture, sampler_, uv, bias.mip_bias).rgb;
        let TBN = pbr_functions::calculate_tbn_mikktspace(normal, in.world_tangent);
        normal = pbr_functions::apply_normal_mapping(flags, TBN, double_sided, is_logically_front, Nt);
}
}
}

        out.normal = vec4(normal * 0.5 + vec3(0.5), class_code(blend_of(in)));
    } else {
        out.normal = vec4(in.world_normal * 0.5 + vec3(0.5), 1.0);
    }

@if(MOTION_VECTOR_PREPASS)
    out.motion_vector = pbr_prepass_functions::calculate_motion_vector(in.world_position, in.previous_world_position);

    return out;
}
@else
@fragment
fn fragment(in: prepass_io::VertexOutput) {
    pbr_prepass_functions::prepass_sample_color_and_alpha_discard(in);
}
