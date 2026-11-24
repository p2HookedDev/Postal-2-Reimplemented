import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
        mesh_view_bindings::view,
    },
};
import package::p2_engine::projector::{apply_decals, cluster_ranges, project, TARGET_TERRAIN};
import package::p2_engine::layer_bindings::{ShaderLayers, apply_layers, post_lighting};
import package::p2_engine::color::{to_display, to_linear};
import package::p2_engine::dynamic_light::dynamic_light;
import package::p2_engine::modern::{allows_gloss, ambient_occlusion, is_modern, modern_surface, surface_of, with_light};
import package::p2_engine::relief::base_relief;
import package::p2_engine::sun_shadow::{has_sun, sun_seen};
import package::p2_engine::lamp_shadow::lamp_light;
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
    @if(VERTEX_COLORS) {
        light = input.color.rgb;
        input.color = vec4(1.0);
    }
    var pbr_input = pbr_input_from_standard_material(input, is_front);
    @if(BINDLESS) {
        let index = indices(vertex_output);
        pbr_input.material.base_color.a *= textureSampleBias(
            bindless_textures_2d[index.alpha_map],
            bindless_samplers_filtering[index.alpha_map_sampler],
            vertex_output.uv_b,
            view.mip_bias
        ).r;
    }
    @else {
        pbr_input.material.base_color.a *= textureSampleBias(alpha_map, alpha_map_sampler, vertex_output.uv_b, view.mip_bias).r;
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
    var surface = to_linear(min(to_display(pbr_input.material.base_color.rgb) * 2.0 * light, vec3(1.0)));
    if is_modern(layers.detail.z) && has_sun() {
        let normal = normalize(pbr_input.world_normal);
        let position = pbr_input.world_position.xyz;
        let seen = sun_seen(pbr_input.world_position, normal, pbr_input.frag_coord.xy);
        let bumped = base_relief(vertex_output, pbr_input, normal, layers.detail.z);
        let gloss = allows_gloss(layers.detail.z);
        let lamps = lamp_light(vertex_output, position, bumped, pbr_input.frag_coord.xy, ranges, gloss, surface_of(0.0));
        surface = modern_surface(pbr_input.material.base_color.rgb, position, bumped, seen, ambient_occlusion(pbr_input.frag_coord.xy), gloss, lamps, surface_of(0.0));
        surface = with_light(surface, pbr_input.material.base_color.rgb, dynamic_light(pbr_input.world_position, normal, ranges));
    }
    var out: FragmentOutput;
    let layered = apply_layers(vertex_output, vec4(surface, alpha), base_alpha, pbr_input, layers);
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, pbr_input.material.base_color.rgb, layered, TARGET_TERRAIN));
    return out;
}
