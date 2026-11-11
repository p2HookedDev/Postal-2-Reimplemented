// Skinned actors lit like D3D mesh lighting, per pixel: ambient plus hardware lights, clamped to 1,
// doubled in display space. No generated relief: its per-triangle frame made polygons visible.

import bevy_pbr::render::{
    pbr_functions::alpha_discard,
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
};
import package::p2_engine::dynamic_light::dynamic_light;
import package::p2_engine::projector::{actor_target, cluster_ranges, project};
import package::p2_engine::color::to_linear;
import package::p2_engine::modern::{actor_highlight, actor_sun, allows_gloss, ambient_occlusion, is_modern, sun_visibility, to_eye};
import package::p2_engine::layer_bindings::{ShaderLayers, apply_layers, lit_color, post_lighting};
@if(BINDLESS)
import package::p2_engine::layer_bindings::indices;

struct HardwareLight {
    position: vec4<f32>,
    direction: vec3<f32>,
    kind: u32,
    color: vec4<f32>,
    attenuation: vec4<f32>,
}

struct Lighting {
    ambient: vec3<f32>,
    count: u32,
    lights: array<HardwareLight, 8>,
}

struct ActorShading {
    lighting: Lighting,
    layers: ShaderLayers,
}

@if(BINDLESS)
@group(3) @binding(107) var<storage> shading_array: array<ActorShading>;
@else
@group(3) @binding(100) var<uniform> shading_data: ActorShading;

fn light_at(light: HardwareLight, position: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let kind = light.kind;
    if kind == 2u {
        return light.color.rgb * max(dot(normal, -light.direction), 0.0);
    }
    let to_light = light.position.xyz - position;
    let distance = length(to_light);
    if distance > light.position.w || distance < 1e-6 {
        return vec3(0.0);
    }
    let l = to_light / distance;
    if kind == 1u && -dot(l, light.direction) <= 0.70710678 {
        return vec3(0.0);
    }
    let falloff = max(light.attenuation.x + light.attenuation.y * distance, 1e-6);
    return light.color.rgb * max(dot(normal, l), 0.0) / falloff;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    // The specular mask (`apply_layers`), before `alpha_discard` sets the alpha to 1 on opaque and masked materials.
    let base_alpha = pbr_input.material.base_color.a;
    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color
    );
@if(BINDLESS)
    let shading = &shading_array[indices(in).data];
@else
    let shading = &shading_data;
    let lighting = &(*shading).lighting;
    let normal = normalize(in.world_normal);
    let count = (*lighting).count;
    let base = pbr_input.material.base_color;
    var out: FragmentOutput;
    var light = (*lighting).ambient;
    var shine = vec3(0.0);
    let glossy = allows_gloss((*shading).layers.detail.z);
    var eye = vec3(0.0);
    if glossy {
        eye = to_eye(in.world_position.xyz);
    }
    var sun = 1.0;
    let modern = is_modern((*shading).layers.detail.z);
    if modern {
        light *= ambient_occlusion(pbr_input.frag_coord.xy);
        sun = sun_visibility(in.world_position, normal, pbr_input.frag_coord.xy);
    }
    for (var i = 0u; i < count; i++) {
        var one = light_at((*lighting).lights[i], in.world_position.xyz, normal);
        let kind = (*lighting).lights[i].kind;
        if kind == 2u {
            one *= actor_sun(sun, dot(normal, -(*lighting).lights[i].direction));
        }
        light += one;
        if glossy && any(one > vec3(0.0)) {
            var to_light = -(*lighting).lights[i].direction;
            if kind != 2u {
                to_light = normalize((*lighting).lights[i].position.xyz - in.world_position.xyz);
            }
            shine += actor_highlight(one, normal, to_light, eye);
        }
    }
    let ranges = cluster_ranges(pbr_input);
    light += dynamic_light(in.world_position, normal, ranges);
    // The driver clamps the lit colour to 1; the texture stage doubles it in display space: as a linear factor.
    let factor = to_linear(2.0 * clamp(light, vec3(0.0), vec3(1.0)));
    var lit = base.rgb * factor + 2.0 * shine;
    if (*shading).layers.blend.x != 0.0 {
        lit = lit_color((*shading).layers, base.rgb, factor);
    }
    let layered = apply_layers(in, vec4(lit, base.a), base_alpha, pbr_input, (*shading).layers);
    out.color = post_lighting(pbr_input, project(pbr_input, ranges, base.rgb, layered, actor_target(in.instance_index)));
    return out;
}
