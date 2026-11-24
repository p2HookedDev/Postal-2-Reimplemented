import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
        mesh_view_bindings::view,
    },
};
import package::p2_render::projector::{cluster_ranges, pool_at, project, wet_color, TARGET_TERRAIN};
import package::p2_render::layer_bindings::{apply_layers, post_lighting};
import package::p2_render::material_layers::material_layers;
import package::p2_render::dynamic_light::dynamic_light;
import package::p2_render::modern::is_modern;
import package::p2_render::relief::base_relief;
import package::p2_render::sun_shadow::{sun_found, sun_index};
import package::p2_render::surface::{modern_light, retail_lit, shade_surface};
@if(BINDLESS)
import package::p2_render::layer_bindings::indices;
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};

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

    let layers = material_layers(vertex_output);
    var surface: vec3<f32>;
    let sun = sun_index();
    let normal = normalize(pbr_input.world_normal);
    let pool = pool_at(pbr_input, ranges, normal);
    if is_modern(layers.detail.z) && sun_found(sun) {
        let bumped = base_relief(vertex_output, pbr_input, normal, layers.detail.z);
        // A terrain layer has no darken or see-through case.
        let incoming = modern_light(vertex_output, pbr_input, sun, normal, bumped, layers.detail.z, layers.blend, false, pool);
        surface = shade_surface(incoming, layers, pbr_input.material.base_color, bumped, false, false, dynamic_light(pbr_input.world_position, normal, ranges)).rgb;
    } else {
        surface = retail_lit(wet_color(pbr_input.material.base_color.rgb, pool), light);
    }
    var out: FragmentOutput;
    let layered = apply_layers(vertex_output, vec4(surface, alpha), base_alpha, pbr_input, layers);
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, pbr_input.material.base_color.rgb, layered, TARGET_TERRAIN));
    return out;
}
