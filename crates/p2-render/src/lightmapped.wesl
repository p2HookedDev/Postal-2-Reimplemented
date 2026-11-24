import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
        mesh_view_bindings::{view, clustered_lights},
        clustered_forward as clustering,
    },
};
import package::p2_render::projector::{cluster_ranges, project, TARGET_BSP};
import package::p2_render::dynamic_light::LE_NON_INCIDENCE;
import package::p2_render::layer_bindings::{apply_layers, lit_color, post_lighting};
import package::p2_render::material_layers::material_layers;
import package::p2_render::color::{to_display, to_linear};
import package::p2_render::modern::{is_modern, is_see_through, with_light};
import package::p2_render::relief::base_relief;
import package::p2_render::sun_shadow::{sun_found, sun_index};
import package::p2_render::surface::{modern_light, shade_surface};

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

    var out: FragmentOutput;
    let color = pbr_input.material.base_color;
    let layers = material_layers(vertex_output);
    let normal = normalize(pbr_input.world_normal);
    let sun = sun_index();
    let modern = is_modern(layers.detail.z) && sun_found(sun);
    var surface: vec3<f32>;
    var alpha = color.a;
    if modern {
        let bumped = base_relief(vertex_output, pbr_input, normal, layers.detail.z);
        let darken = layers.blend.x != 0.0;
        let incoming = modern_light(vertex_output, pbr_input, sun, normal, bumped, layers.detail.z, layers.blend, darken);
        let shaded = shade_surface(incoming, layers, color, bumped, darken, is_see_through(pbr_input.material.flags), vec3(0.0));
        surface = shaded.rgb;
        alpha = shaded.a;
    } else {
        surface = lit_color(layers, color.rgb, view.exposure * pbr_input.lightmap_light);
    }
    let layered = apply_layers(vertex_output, vec4(surface, alpha), base_alpha, pbr_input, layers);
    var lit = layered.rgb;

    var passes = vec3(0.0);
    for (var i = ranges.first_point_light_index_offset; i < ranges.first_spot_light_index_offset; i++) {
        passes += light_pass(clustering::get_clusterable_object_id(i), pbr_input.world_position.xyz, normal);
    }
    if any(passes > vec3(0.0)) {
        let texture = to_display(color.rgb);
        if layers.blend.x != 0.0 {
            // A darken surface stores one minus its colour: light on the colour takes off the stored one.
            lit = to_linear(max(to_display(lit) - (1.0 - texture) * passes, vec3(0.0)));
        } else if modern {
            lit = with_light(lit, color.rgb, passes);
        } else {
            lit = to_linear(min(to_display(lit) + texture * passes, vec3(1.0)));
        }
    }
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, color.rgb, vec4(lit, layered.a), TARGET_BSP));
    return out;
}
