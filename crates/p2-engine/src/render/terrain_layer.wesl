import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
    },
};
import package::p2_engine::projector::{apply_decals, cluster_ranges, project, TARGET_TERRAIN};
import package::p2_engine::layer_bindings::{ShaderLayers, apply_layers, post_lighting};
import package::p2_engine::color::{to_display, to_linear};
import package::p2_engine::modern::{
    ambient_occlusion, base_relief, is_modern, key_shading, masked_sun_visibility, sun_highlight, sun_shadowed,
};
@if(BINDLESS)
import package::p2_engine::layer_bindings::indices;
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};

@if(BINDLESS)
@group(3) @binding(107) var<storage> layers_array: array<ShaderLayers>;
@else
@group(3) @binding(100) var<uniform> layers_data: ShaderLayers;

@if(!BINDLESS) {
@group(3) @binding(109) var alpha_map: texture_2d<f32>;
@group(3) @binding(110) var alpha_map_sampler: sampler;
}

@fragment
fn fragment(vertex_output: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var input = vertex_output;
    var light = vec3(0.5);
    var mask = 0.0;
    @if(VERTEX_COLORS) {
        light = input.color.rgb;
        mask = input.color.a;
        input.color = vec4(1.0);
    }
    var pbr_input = pbr_input_from_standard_material(input, is_front);
    @if(BINDLESS) {
        let index = indices(vertex_output);
        pbr_input.material.base_color.a *= textureSample(
            bindless_textures_2d[index.alpha_map],
            bindless_samplers_filtering[index.alpha_map_sampler],
            vertex_output.uv_b
        ).r;
    }
    @else {
        pbr_input.material.base_color.a *= textureSample(alpha_map, alpha_map_sampler, vertex_output.uv_b).r;
    }
    let base_alpha = pbr_input.material.base_color.a;
    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color
    );

    let alpha = pbr_input.material.base_color.a;
    let ranges = cluster_ranges(pbr_input);
    apply_decals(&pbr_input, ranges);

@if(BINDLESS)
    let layers = layers_array[indices(vertex_output).data];
@else
    let layers = layers_data;
    var shine = vec3(0.0);
    if is_modern(layers.detail.z) {
        let normal = normalize(pbr_input.world_normal);
        let visibility = masked_sun_visibility(mask, pbr_input.world_position, normal, pbr_input.frag_coord.xy);
        light = sun_shadowed(light, mask, visibility, normal);
        let relief = base_relief(vertex_output, pbr_input, normal, layers.specular.x, layers.detail.z);
        let key = key_shading(normal, relief, pbr_input.world_position.xyz);
        let sun = sun_highlight(relief, mask, visibility, pbr_input.world_position.xyz, normal);
        light *= ambient_occlusion(pbr_input.frag_coord.xy);
        shine = key.y * to_linear(2.0 * min(light, vec3(1.0))) + sun;
        light *= key.x;
    }
    let lit = min(to_display(pbr_input.material.base_color.rgb) * 2.0 * light, vec3(1.0));
    var out: FragmentOutput;
    let layered = apply_layers(vertex_output, vec4(to_linear(lit) + shine, alpha), base_alpha, pbr_input, layers);
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, pbr_input.material.base_color.rgb, layered, TARGET_TERRAIN));
    return out;
}
