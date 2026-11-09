import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
        mesh_view_bindings::{view, clustered_lights},
        clustered_forward as clustering,
    },
};
import package::p2_engine::projector::{apply_decals, cluster_ranges, project, TARGET_BSP};
import package::p2_engine::dynamic_light::LE_NON_INCIDENCE;
import package::p2_engine::layer_bindings::{ShaderLayers, apply_layers, post_lighting};
import package::p2_engine::color::{to_display, to_linear};
import package::p2_engine::modern::{
    base_relief, highlight, is_modern, key_shading, masked_sun_visibility, no_relief, relief_light, static_occlusion, sun_highlight,
    sun_shadowed_linear,
    to_eye,
};
@if(BINDLESS)
import package::p2_engine::layer_bindings::indices;
@if(LIGHTMAP)
import bevy_pbr::render::mesh_bindings::mesh;
@if(LIGHTMAP && MULTIPLE_LIGHTMAPS_IN_ARRAY)
import bevy_pbr::lightmap::{lightmaps_textures, lightmaps_samplers};
@if(LIGHTMAP && !MULTIPLE_LIGHTMAPS_IN_ARRAY)
import bevy_pbr::lightmap::{lightmaps_texture, lightmaps_sampler};

@if(BINDLESS)
@group(3) @binding(107) var<storage> layers_array: array<ShaderLayers>;
@else
@group(3) @binding(100) var<uniform> layers_data: ShaderLayers;

const LE_NONE: u32 = 0u;

// The retail light pass of one dynamic light, without the texture: colour ×
// sqrt(1 − h / R) saturated, times the attenuation texture's falloff (64 texels, edge at 30.5 from the centre) at the
// distance from the light's foot in the surface's plane, over the radius of the circle its sphere cuts there. Only
// lights with `LE_None` (0) or `LE_NonIncidence` (13), which bsp_light.rs puts in the point light's radius.
fn light_pass(light_id: u32, position: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let light = clustered_lights.data[light_id];
    let effect = u32(light.position_radius.w + 0.5);
    if effect != LE_NONE && effect != LE_NON_INCIDENCE {
        return vec3(0.0);
    }
    let radius = light.range;
    let to_point = position - light.position_radius.xyz;
    let h = dot(to_point, normal);
    if abs(h) >= radius {
        return vec3(0.0);
    }
    let color = min(light.color_inverse_square_range.rgb * sqrt(1.0 - abs(h) / radius), vec3(1.0));
    let circle = sqrt(radius * radius - h * h);
    let rho = length(to_point - h * normal);
    return color * max(1.0 - (32.0 / 30.5) * rho / circle, 0.0);
}

fn sun_mask(in: VertexOutput) -> f32 {
@if(LIGHTMAP) {
    let packed_uv_rect = mesh[in.instance_index].lightmap_uv_rect;
    let uv_rect = vec4(unpack2x16unorm(packed_uv_rect.x), unpack2x16unorm(packed_uv_rect.y));
    let uv = mix(uv_rect.xy, uv_rect.zw, in.uv_b);
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot >> 16u;
@if(MULTIPLE_LIGHTMAPS_IN_ARRAY)
    return textureSampleLevel(lightmaps_textures[slot], lightmaps_samplers[slot], uv, 0.0).a;
@else
    return textureSampleLevel(lightmaps_texture, lightmaps_sampler, uv, 0.0).a;
}
@else {
    return 0.0;
}
}

@fragment
fn fragment(vertex_output: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(vertex_output, is_front);
    let base_alpha = pbr_input.material.base_color.a;
    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color
    );
    let ranges = cluster_ranges(pbr_input);
    apply_decals(&pbr_input, ranges);

    var out: FragmentOutput;
    let color = pbr_input.material.base_color;
@if(BINDLESS)
    let layers = layers_array[indices(vertex_output).data];
@else
    let layers = layers_data;
    let normal = normalize(pbr_input.world_normal);
    let modern = is_modern(layers.detail.z);
    var relief = no_relief(normal);
    var light_map = view.exposure * pbr_input.lightmap_light;
    var surface = light_map * color.rgb;
    var eye = vec3(0.0);
    if modern {
        eye = to_eye(pbr_input.world_position.xyz);
        let mask = sun_mask(vertex_output);
        let visibility = masked_sun_visibility(mask, pbr_input.world_position, normal, pbr_input.frag_coord.xy);
        light_map = sun_shadowed_linear(light_map, mask, visibility, normal);
        relief = base_relief(vertex_output, pbr_input, normal, layers.specular.x, layers.detail.z);
        let key = key_shading(normal, relief, pbr_input.world_position.xyz);
        let sun = sun_highlight(relief, mask, visibility, pbr_input.world_position.xyz, normal);
        surface = static_occlusion(pbr_input.world_position.xyz, normal, pbr_input.frag_coord.xy) * light_map * (key.x * color.rgb + key.y) + sun;
    }
    let layered = apply_layers(vertex_output, vec4(surface, color.a), base_alpha, pbr_input, layers);
    var lit = layered.rgb;

    var passes = vec3(0.0);
    var shine = vec3(0.0);
    for (var i = ranges.first_point_light_index_offset; i < ranges.first_spot_light_index_offset; i++) {
        let id = clustering::get_clusterable_object_id(i);
        var one = light_pass(id, pbr_input.world_position.xyz, normal);
        if modern {
            let to_light = clustered_lights.data[id].position_radius.xyz - pbr_input.world_position.xyz;
            let direction = to_light / max(length(to_light), 1e-6);
            one *= relief_light(normal, relief.normal, direction);
            shine += one * highlight(relief, direction, eye);
        }
        passes += one;
    }
    if any(passes > vec3(0.0)) {
        let texture = to_display(color.rgb);
        let display = to_display(lit) + texture * passes + shine;
        lit = to_linear(min(display, vec3(1.0)));
    }
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, color.rgb, vec4(lit, layered.a), TARGET_BSP));
    return out;
}
