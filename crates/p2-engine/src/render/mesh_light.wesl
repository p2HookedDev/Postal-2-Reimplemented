import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
    },
};
import package::p2_engine::projector::{apply_decals, cluster_ranges, mesh_target, project};
import package::p2_engine::dynamic_light::{dynamic_light, dynamic_light_relief};
import package::p2_engine::modern::{ambient_occlusion, base_relief, is_modern, key_shading, to_eye};
import package::p2_engine::color::{to_display, to_linear};
import package::p2_engine::layer_bindings::{ShaderLayers, apply_layers, post_lighting};
@if(BINDLESS)
import package::p2_engine::layer_bindings::indices;

@if(BINDLESS)
@group(3) @binding(107) var<storage> layers_array: array<ShaderLayers>;
@else
@group(3) @binding(100) var<uniform> layers_data: ShaderLayers;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var plain = in;
@if(VERTEX_COLORS)
    plain.color = vec4(1.0);
    var pbr_input = pbr_input_from_standard_material(plain, is_front);
    let ranges = cluster_ranges(pbr_input);
    apply_decals(&pbr_input, ranges);
    var color = pbr_input.material.base_color;
    let texture = color.rgb;
@if(BINDLESS)
    let layers = layers_array[indices(in).data];
@else
    let layers = layers_data;
@if(VERTEX_COLORS) {
    color.a *= in.color.a;
    var light = in.color.rgb;
    let normal = normalize(pbr_input.world_normal);
    if is_modern(layers.detail.z) {
        let position = pbr_input.world_position;
        let relief = base_relief(in, pbr_input, normal, layers.specular.x, layers.detail.z);
        let key = key_shading(normal, relief, position.xyz);
        let lights = dynamic_light_relief(position, relief, to_eye(position.xyz), ranges);
        light *= ambient_occlusion(pbr_input.frag_coord.xy);
        let baked = key.x * to_display(light) / 2.0;
        let lit = to_linear(2.0 * min(baked + lights.diffuse, vec3(1.0)));
        color = vec4(color.rgb * lit + key.y * light + lights.specular, color.a);
    } else {
        let dynamic = dynamic_light(pbr_input.world_position, normal, ranges);
        if any(dynamic > vec3(0.0)) {
            let baked = to_display(light) / 2.0;
            light = to_linear(2.0 * min(baked + dynamic, vec3(1.0)));
        }
        color = vec4(color.rgb * light, color.a);
    }
}
    color = apply_layers(in, color, color.a, pbr_input, layers);
    pbr_input.material.base_color = alpha_discard(pbr_input.material.flags, pbr_input.material.alpha_cutoff, color);
    var out: FragmentOutput;
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, texture, pbr_input.material.base_color, mesh_target(in.instance_index)));
    return out;
}
