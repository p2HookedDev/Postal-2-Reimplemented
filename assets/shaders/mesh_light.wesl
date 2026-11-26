import bevy_pbr::{
    render::{
        pbr_functions::alpha_discard,
        pbr_fragment::pbr_input_from_standard_material,
        forward_io::{VertexOutput, FragmentOutput},
    },
};
import package::p2_render::projector::{cluster_ranges, decal_list, mesh_target, pool_at, pool_mirror, project, wet_color, wet_normal, NO_PROJECTORS};
import package::p2_render::dynamic_light::dynamic_light;
@if(VISIBILITY_RANGE_DITHER)
import bevy_pbr::render::pbr_functions::visibility_range_dither;
import package::p2_render::modern::{is_modern, is_see_through};
import package::p2_render::relief::base_relief;
import package::p2_render::sun_shadow::{sun_found, sun_index, world_lookup};
import package::p2_render::surface::{modern_light, pooled_unlit, shade_surface};
import package::p2_render::color::{to_display, to_linear};
import package::p2_render::layer_bindings::{apply_layers, base_tone, lit_color, post_lighting};
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
    let decals = decal_list(in, pbr_input, ranges, true);
    var color = pbr_input.material.base_color;
    let texture = color.rgb;
    let tone = base_tone(in, pbr_input, texture);
    let layers = material_layers(in);
    var pooled = layers.blend.x == 0.0;
    var mirror = 0.0;
@if(VERTEX_COLORS) {
    color.a *= in.color.a;
    var light = in.color.rgb;
    let normal = normalize(pbr_input.world_normal);
    let sun = sun_index();
    let darken = layers.blend.x != 0.0;
    // Modern glass drops the pool, so the game's blood splats are pasted on it after the light instead.
    let glass = is_modern(layers.detail.z) && sun_found(sun) && is_see_through(pbr_input.material.flags);
    pooled = pooled && !glass;
    let pool = pool_at(pbr_input, decals, normal, select(mesh_target(in.instance_index), NO_PROJECTORS, glass), tone);
@if(POOL_MIRROR)
    mirror = select(0.0, pool_mirror(pool), pooled);
    if is_modern(layers.detail.z) && sun_found(sun) {
        let bumped = base_relief(in, pbr_input, normal, layers.detail.z);
        let incoming = modern_light(in, pbr_input, sun, normal, bumped, normal, layers.detail.z, layers.blend, darken, pool, world_lookup());
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
@else {
    if pooled {
        let normal = normalize(pbr_input.world_normal);
        let pool = pool_at(pbr_input, decals, normal, mesh_target(in.instance_index), tone);
        color = vec4(wet_color(color.rgb, pool), color.a);
@if(POOL_MIRROR)
        mirror = pool_mirror(pool);
        let sun = sun_index();
        if (pool.wet > 0.0 || any(pool.glow > vec3(0.0))) && is_modern(layers.detail.z) && sun_found(sun) {
            let incoming = modern_light(in, pbr_input, sun, normal, normal, normal, layers.detail.z, layers.blend, layers.blend.x != 0.0, pool, world_lookup());
            color = vec4(pooled_unlit(color.rgb, wet_normal(normal, pool), incoming), color.a);
        }
    }
}
    color = apply_layers(in, color, color.a, pbr_input, layers);
    pbr_input.material.base_color = alpha_discard(pbr_input.material.flags, pbr_input.material.alpha_cutoff, color);
    var out: FragmentOutput;
    out.color = post_lighting(pbr_input, project(pbr_input, decals, texture, pbr_input.material.base_color, mesh_target(in.instance_index), pooled));
@if(POOL_MIRROR)
    out.color.a = 1.0 + mirror;
    return out;
}
