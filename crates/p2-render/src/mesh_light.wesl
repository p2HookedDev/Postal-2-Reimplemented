import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
    },
};
import package::p2_render::projector::{cluster_ranges, mesh_target, pool_at, project, wet_color};
import package::p2_render::dynamic_light::dynamic_light;
@if(VISIBILITY_RANGE_DITHER)
import bevy_pbr::render::pbr_functions::visibility_range_dither;
import package::p2_render::modern::{is_modern, is_see_through};
import package::p2_render::relief::base_relief;
import package::p2_render::sun_shadow::{sun_found, sun_index};
import package::p2_render::surface::{modern_light, shade_surface};
import package::p2_render::color::{to_display, to_linear};
import package::p2_render::layer_bindings::{apply_layers, lit_color, post_lighting};
import package::p2_render::material_layers::material_layers;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
@if(VISIBILITY_RANGE_DITHER)
    visibility_range_dither(in.position, in.visibility_range_dither);
    var plain = in;
@if(VERTEX_COLORS)
    plain.color = vec4(1.0);
    var pbr_input = pbr_input_from_standard_material(plain, is_front);
    let ranges = cluster_ranges(pbr_input);
    var color = pbr_input.material.base_color;
    let texture = color.rgb;
    let layers = material_layers(in);
@if(VERTEX_COLORS) {
    color.a *= in.color.a;
    var light = in.color.rgb;
    let normal = normalize(pbr_input.world_normal);
    let sun = sun_index();
    let darken = layers.blend.x != 0.0;
    let pool = pool_at(pbr_input, ranges, normal);
    if is_modern(layers.detail.z) && sun_found(sun) {
        let bumped = base_relief(in, pbr_input, normal, layers.detail.z);
        let incoming = modern_light(in, pbr_input, sun, normal, bumped, layers.detail.z, layers.blend, darken, pool);
        color = shade_surface(incoming, layers, color, bumped, darken, is_see_through(pbr_input.material.flags), dynamic_light(pbr_input.world_position, normal, ranges));
    } else {
        let dynamic = dynamic_light(pbr_input.world_position, normal, ranges);
        if any(dynamic > vec3(0.0)) {
            let baked = to_display(light) / 2.0;
            light = to_linear(2.0 * min(baked + dynamic, vec3(1.0)));
        }
        color = vec4(lit_color(layers, select(wet_color(color.rgb, pool), color.rgb, darken), light), color.a);
    }
}
    color = apply_layers(in, color, color.a, pbr_input, layers);
    pbr_input.material.base_color = alpha_discard(pbr_input.material.flags, pbr_input.material.alpha_cutoff, color);
    var out: FragmentOutput;
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, texture, pbr_input.material.base_color, mesh_target(in.instance_index)));
    return out;
}
