import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
    },
};
import package::p2_engine::projector::{apply_decals, cluster_ranges, mesh_target, project};
import package::p2_engine::dynamic_light::dynamic_light;
@if(VISIBILITY_RANGE_DITHER)
import bevy_pbr::render::pbr_functions::visibility_range_dither;
import package::p2_engine::modern::{
    allows_gloss, ambient_occlusion, classed_surface, is_modern, is_see_through, modern_glass, modern_surface, with_light,
};
import package::p2_engine::relief::base_relief;
import package::p2_engine::sun_shadow::{has_sun, sun_seen};
import package::p2_engine::lamp_shadow::lamp_light;
import package::p2_engine::color::{to_display, to_linear};
import package::p2_engine::layer_bindings::{ShaderLayers, apply_layers, lit_color, post_lighting};
@if(BINDLESS)
import package::p2_engine::layer_bindings::indices;

@if(BINDLESS)
@group(3) @binding(107) var<storage> layers_array: array<ShaderLayers>;
@else
@group(3) @binding(100) var<uniform> layers_data: ShaderLayers;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
@if(VISIBILITY_RANGE_DITHER)
    visibility_range_dither(in.position, in.visibility_range_dither);
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
    if is_modern(layers.detail.z) && has_sun() {
        let position = pbr_input.world_position.xyz;
        let seen = sun_seen(pbr_input.world_position, normal, pbr_input.frag_coord.xy);
        let bumped = base_relief(in, pbr_input, normal, layers.detail.z);
        let darken = layers.blend.x != 0.0;
        let gloss = allows_gloss(layers.detail.z) && !darken;
        let material_surface = classed_surface(layers.detail.z, layers.blend);
        let lamps = lamp_light(in, position, bumped, pbr_input.frag_coord.xy, ranges, gloss, material_surface, layers.detail.z);
        let occlusion = ambient_occlusion(pbr_input.frag_coord.xy);
        let dynamic = dynamic_light(pbr_input.world_position, normal, ranges);
        if darken {
            let white = modern_surface(vec3(1.0), position, bumped, seen, occlusion, gloss, lamps, material_surface);
            color = vec4(lit_color(layers, color.rgb, with_light(white, vec3(1.0), dynamic)), color.a);
        } else if is_see_through(pbr_input.material.flags) {
            let glass = modern_glass(color, position, bumped, seen, occlusion, gloss, lamps, material_surface);
            color = vec4(with_light(glass.rgb, color.rgb, dynamic), glass.a);
        } else {
            let lit = modern_surface(color.rgb, position, bumped, seen, occlusion, gloss, lamps, material_surface);
            color = vec4(with_light(lit, color.rgb, dynamic), color.a);
        }
    } else {
        let dynamic = dynamic_light(pbr_input.world_position, normal, ranges);
        if any(dynamic > vec3(0.0)) {
            let baked = to_display(light) / 2.0;
            light = to_linear(2.0 * min(baked + dynamic, vec3(1.0)));
        }
        color = vec4(lit_color(layers, color.rgb, light), color.a);
    }
}
    color = apply_layers(in, color, color.a, pbr_input, layers);
    pbr_input.material.base_color = alpha_discard(pbr_input.material.flags, pbr_input.material.alpha_cutoff, color);
    var out: FragmentOutput;
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, texture, pbr_input.material.base_color, mesh_target(in.instance_index)));
    return out;
}
