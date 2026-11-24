// Skinned actors. Retail: lit like D3D mesh lighting, per pixel: ambient plus hardware lights, clamped to 1, doubled in
// display space. Modern: the world's shared light where the view has the sun, without generated relief.

import bevy_pbr::render::{
    pbr_functions::alpha_discard,
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
};
import bevy_pbr::render::mesh_view_bindings::view;
import package::p2_render::dynamic_light::dynamic_light;
@if(VISIBILITY_RANGE_DITHER)
import bevy_pbr::render::pbr_functions::visibility_range_dither;
import package::p2_render::projector::{actor_target, cluster_ranges, decal_list, pool_mirror, project, splat_pool};
import package::p2_render::color::to_linear;
import package::p2_render::modern::{
    Around, actor_highlight, actor_sun, allows_gloss, ambient_occlusion, is_modern, lit_surface, sky_from_ambient, surface_of,
};
import package::p2_render::modern_base::to_eye;
import package::p2_render::sun_shadow::{ROOFED_SKY, SKY_FACING_FADE, SUN_FACING_FADE, ShadowLookup, sun_found, sun_index, world_lookup};
import package::p2_render::lamp_shadow::{add_lamp, lamp_shown, no_lamps};
import package::p2_render::surface::{add_dynamic, material_sun_visibility, modern_light, shade_surface};
import package::p2_render::layer_bindings::{apply_layers, lit_color, post_lighting};
import package::p2_render::layer_types::{ActorShading, HardwareLight, Lighting, ShaderLayers};
import package::p2_render::tone_curve::pbr_neutral;
@if(BINDLESS)
import package::p2_render::layer_bindings::indices;

@if(BINDLESS)
@group(3) @binding(107) var<storage> shading_array: array<ActorShading>;
@else
@group(3) @binding(100) var<uniform> shading_data: ActorShading;

// The world's lights handed to an overlay-drawn actor under modern, as `actor_light/view_model.rs` packs them.
const SUN_KIND: u32 = 2u;
const LAMP_KIND: u32 = 3u;
const FILL_KIND: u32 = 4u;
// Drawn by an overlay camera straight over the scene, as `OverlayMark` in `actor_light.rs` numbers it.
const OVERLAY_WORLD_LIT: u32 = 2u;
// In world units: as the weapon's bounds reach this far into a wall, its light turns wholly to the light at the eye.
const POKE_SPAN: f32 = 16.0;
// In world units: a pulled point stops this much nearer the eye, so it never sits on a wall's face.
const FREE_EYE_MARGIN: f32 = 0.5;

// Pulled toward the eye to no farther than the level leaves free, so a barrel through a wall takes this side's light.
fn within_free(position: vec3<f32>, free: f32) -> vec3<f32> {
    let eye = view.world_position.xyz;
    let from_eye = position - eye;
    let distance = max(length(from_eye), 1e-4);
    return eye + from_eye * (max(min(distance, free) - FREE_EYE_MARGIN, 0.0) / distance);
}

// The open sky above the eye, picked on the CPU for the whole weapon.
fn eye_sky(lighting: Lighting) -> f32 {
    for (var i = 0u; i < lighting.count; i++) {
        if lighting.lights[i].kind == SUN_KIND {
            return lighting.lights[i].attenuation.x;
        }
    }
    return 1.0;
}

fn overlay_surface(color: vec3<f32>, position: vec3<f32>, normal: vec3<f32>, ao: f32, gloss: bool, lighting: Lighting) -> vec3<f32> {
    let surface = surface_of(0.0);
    let eye = to_eye(position);
    var around = Around(vec3(0.0), vec3(0.0, 1.0, 0.0), lighting.ambient, vec3(0.0), vec3(0.0), vec3(0.0));
    var seen = vec3(0.0, 1.0, 1.0);
    var lamps = no_lamps();
    var fill_count = 0.0;
    for (var i = 0u; i < lighting.count; i++) {
        let light = lighting.lights[i];
        let way = -light.direction;
        if light.kind == SUN_KIND {
            let facing = dot(normal, way);
            around.sun_color = light.color.rgb;
            around.to_sun = way;
            seen = vec3(light.color.a * saturate(facing / SUN_FACING_FADE), mix(1.0, light.color.a, saturate(facing / SKY_FACING_FADE)), mix(ROOFED_SKY, 1.0, light.attenuation.x));
        } else if light.kind == FILL_KIND {
            around.sky += light.color.rgb;
            fill_count += 1.0;
            around.fills += sqrt(light.color.rgb) * saturate(dot(normal, way));
        } else if light.kind == LAMP_KIND {
            let to_light = light.position.xyz - position;
            let d2 = dot(to_light, to_light);
            let x2 = d2 / (light.position.w * light.position.w);
            let distance_to = sqrt(max(d2, 1e-4));
            let to_lamp = to_light / distance_to;
            let facing = dot(normal, to_lamp);
            if x2 >= 1.0 || facing <= 0.0 {
                continue;
            }
            var cone = 1.0;
            if light.attenuation.y > 0.5 {
                let share = saturate((dot(light.direction, -to_lamp) - light.attenuation.x) / max(1.0 - light.attenuation.x, 1e-4));
                cone = share * share;
            }
            let a = saturate(surface.x * surface.x + light.attenuation.z / (2.0 * distance_to));
            add_lamp(&lamps, lamp_shown(light.color.rgb, x2, cone), light.color.rgb, x2, cone, saturate(facing / SUN_FACING_FADE), facing, to_lamp, normal, eye, gloss, a);
        }
    }
    around.sky /= max(fill_count, 1.0);
    if fill_count == 0.0 && any(around.sun_color > vec3(0.0)) {
        sky_from_ambient(&around, normal);
    }
    return lit_surface(color, position, normal, seen, ao, gloss, lamps, surface, around);
}

fn light_at(light: HardwareLight, position: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let kind = light.kind;
    // A modern overlay light fading out after the switch turned off.
    if kind >= LAMP_KIND {
        return vec3(0.0);
    }
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
@if(VISIBILITY_RANGE_DITHER)
    visibility_range_dither(in.position, in.visibility_range_dither);
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
    let flag = (*shading).layers.detail.z;
    let modern = is_modern(flag);
    let ranges = cluster_ranges(pbr_input);
    let decals = decal_list(in, pbr_input, ranges, true);
    var out: FragmentOutput;
    var lit = vec3(0.0);
    let overlay = (*shading).overlay != 0u;
    let world_lit = (*shading).overlay == OVERLAY_WORLD_LIT;
    let sun_id = sun_index();
    let sun_on = sun_found(sun_id);
    // Wounds stand as blood under the sun; elsewhere, and on a darken skin which takes no pool, `project` pastes them.
    var pooled = false;
    var mirror = 0.0;
    if modern && (sun_on || overlay) {
        let darken = (*shading).layers.blend.x != 0.0;
        let dynamic = dynamic_light(in.world_position, normal, ranges);
        // The share of the light picked at the eye: all of it for an overlay that does not take the world's.
        var eye_share = 1.0;
        if sun_on && (!overlay || world_lit) {
            pooled = !darken;
            let wounds = splat_pool(pbr_input, decals, normal, actor_target(in.instance_index));
@if(POOL_MIRROR)
            mirror = select(0.0, pool_mirror(wounds), pooled);
            // An overlay has no prepass, so neither SSAO nor the screen's contact and lamp shadows reach it.
            var lit_input = pbr_input;
            eye_share = 0.0;
            var lookup = world_lookup();
            if world_lit {
                lit_input.world_position = vec4(within_free(in.world_position.xyz, (*shading).free), 1.0);
                eye_share = saturate(((*shading).reach - (*shading).free) / POKE_SPAN);
                // By whole screen pixel: the weapon is locked to the view, so the grain stays on it and never crawls.
                lookup = ShadowLookup(0.0, eye_sky(*lighting), floor(in.position.xy));
            }
            var incoming = modern_light(in, lit_input, sun_id, normal, normal, flag, (*shading).layers.blend, darken, wounds, lookup);
            if world_lit {
                incoming.glow = vec3((*shading).glow);
            }
            lit = shade_surface(incoming, (*shading).layers, base, normal, darken, false, dynamic).rgb;
        }
        if eye_share > 0.0 {
            let gloss = allows_gloss(flag) && !darken;
            let surface = overlay_surface(select(base.rgb, vec3(1.0), darken), in.world_position.xyz, normal, ambient_occlusion(pbr_input.frag_coord.xy), gloss, *lighting);
            lit = mix(lit, add_dynamic((*shading).layers, surface, base.rgb, darken, dynamic), eye_share);
        }
    } else {
        var light = (*lighting).ambient;
        var shine = vec3(0.0);
        let glossy = allows_gloss(flag);
        var eye = vec3(0.0);
        if glossy {
            eye = to_eye(in.world_position.xyz);
        }
        var sun = 1.0;
        if modern {
            light *= ambient_occlusion(pbr_input.frag_coord.xy);
            sun = material_sun_visibility(in, sun_id, in.world_position, normal, pbr_input.frag_coord.xy);
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
        light += dynamic_light(in.world_position, normal, ranges);
        // The driver clamps the lit colour to 1; the texture stage doubles it in display space: as a linear factor.
        let factor = to_linear(2.0 * clamp(light, vec3(0.0), vec3(1.0)));
        lit = base.rgb * factor + 2.0 * shine;
        if (*shading).layers.blend.x != 0.0 {
            lit = lit_color((*shading).layers, base.rgb, factor);
        }
    }
    let layered = apply_layers(in, vec4(lit, base.a), base_alpha, pbr_input, (*shading).layers);
    out.color = post_lighting(pbr_input, project(pbr_input, decals, base.rgb, layered, actor_target(in.instance_index), pooled));
    // The overlay camera has no reflection pass to read the record back.
@if(POOL_MIRROR) {
    if !overlay {
        out.color.a = 1.0 + mirror;
    }
}
    // The overlay camera draws after the frame's tone pass, so its actors take the same curve here.
    if modern && overlay {
        out.color = vec4(pbr_neutral(out.color.rgb), out.color.a);
    }
    return out;
}
