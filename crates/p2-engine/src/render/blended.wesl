import bevy_pbr::{
    render::{
        pbr_functions::{alpha_discard, apply_pbr_lighting},
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
        pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
    },
};
import package::p2_engine::projector::{apply_decals, cluster_ranges, mesh_target, project};
import package::p2_engine::layer_bindings::{ShaderLayers, apply_layers, post_lighting};
@if(BINDLESS)
import package::p2_engine::layer_bindings::indices;

@if(BINDLESS)
@group(3) @binding(107) var<storage> layers_array: array<ShaderLayers>;
@else
@group(3) @binding(100) var<uniform> layers_data: ShaderLayers;

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

    var color = pbr_input.material.base_color;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        var lit_input = pbr_input;
        lit_input.material.emissive = vec4(0.0);
        color = apply_pbr_lighting(lit_input);
    }
@if(BINDLESS)
    let layers = layers_array[indices(vertex_output).data];
@else
    let layers = layers_data;
    var out: FragmentOutput;
    let layered = apply_layers(vertex_output, color, base_alpha, pbr_input, layers);
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, pbr_input.material.base_color.rgb, layered, mesh_target(vertex_output.instance_index)));
    return out;
}
