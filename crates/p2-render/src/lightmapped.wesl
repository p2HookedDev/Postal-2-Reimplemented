import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
        mesh_view_bindings::{view, clustered_lights},
        clustered_forward as clustering,
    },
};
import package::p2_render::projector::{cluster_ranges, decal_list, pool_at, pool_mirror, project, wet_color, NO_PROJECTORS, TARGET_BSP};
import package::p2_render::dynamic_light::LE_NON_INCIDENCE;
import package::p2_render::layer_bindings::{apply_layers, base_tone, lit_color, post_lighting};
import package::p2_render::material_layers::material_layers;
import package::p2_render::color::{to_display, to_linear};
import package::p2_render::modern::{is_modern, is_see_through, with_light};
import package::p2_render::relief::base_relief;
import package::p2_render::sun_shadow::{sun_found, sun_index, world_lookup};
import package::p2_render::surface::{modern_light, shade_surface};
@if(VISIBILITY_RANGE_DITHER)
import bevy_pbr::render::pbr_functions::visibility_range_dither;

const LE_NONE: u32 = 0u;

// The original light pass of one dynamic light, without texture: colour times sqrt(1 - h / R) saturated, times
// the attenuation texture's falloff (64 texels, edge 30.5 from centre) at the distance from the light's foot
// over its circle's radius in the surface plane. Only for `LE_None` and `LE_NonIncidence` lights.
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
@if(VISIBILITY_RANGE_DITHER)
    visibility_range_dither(vertex_output.position, vertex_output.visibility_range_dither);
    var plain = vertex_output;
@if(VERTEX_COLORS)
    plain.color = vec4(1.0);
    var pbr_input = pbr_input_from_standard_material(plain, is_front);
    let base_alpha = pbr_input.material.base_color.a;
    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color
    );
    let ranges = cluster_ranges(pbr_input);
    let decals = decal_list(plain, pbr_input, ranges, true);

    var out: FragmentOutput;
    let color = pbr_input.material.base_color;
    let layers = material_layers(plain);
    let normal = normalize(pbr_input.world_normal);
    let sun = sun_index();
    let modern = is_modern(layers.detail.z) && sun_found(sun);
    let darken = layers.blend.x != 0.0;
    // Modern glass drops the pool, so the game's blood splats are pasted on it after the light instead.
    let glass = modern && is_see_through(pbr_input.material.flags);
    let pooled = !darken && !glass;
    let pool = pool_at(pbr_input, decals, normal, select(TARGET_BSP, NO_PROJECTORS, glass), base_tone(plain, pbr_input, color.rgb));
    var surface: vec3<f32>;
    var alpha = color.a;
    if modern {
        let bumped = base_relief(plain, pbr_input, normal, layers.detail.z);
        let incoming = modern_light(vertex_output, pbr_input, sun, normal, bumped, normal, layers.detail.z, layers.blend, darken, pool, world_lookup());
        let shaded = shade_surface(incoming, layers, color, bumped, darken, is_see_through(pbr_input.material.flags), vec3(0.0));
        surface = shaded.rgb;
        alpha = shaded.a;
    } else {
        surface = lit_color(layers, select(wet_color(color.rgb, pool), color.rgb, darken), view.exposure * pbr_input.lightmap_light);
    }
    let layered = apply_layers(plain, vec4(surface, alpha), base_alpha, pbr_input, layers);
    var lit = layered.rgb;

    var passes = vec3(0.0);
    for (var i = ranges.first_point_light_index_offset; i < ranges.first_spot_light_index_offset; i++) {
        passes += light_pass(clustering::get_clusterable_object_id(i), pbr_input.world_position.xyz, normal);
    }
    if any(passes > vec3(0.0)) {
        let shown = select(color.rgb, wet_color(color.rgb, pool), pooled);
        let texture = to_display(shown);
        if layers.blend.x != 0.0 {
            // A darken surface stores one minus its colour: light on the colour takes off the stored one.
            lit = to_linear(max(to_display(lit) - (1.0 - texture) * passes, vec3(0.0)));
        } else if modern {
            lit = with_light(lit, shown, passes);
        } else {
            lit = to_linear(min(to_display(lit) + texture * passes, vec3(1.0)));
        }
    }
    out.color = post_lighting(pbr_input, project(pbr_input, decals, color.rgb, vec4(lit, layered.a), TARGET_BSP, pooled));
@if(POOL_MIRROR)
    out.color.a = 1.0 + select(0.0, pool_mirror(pool), pooled);
    return out;
}
