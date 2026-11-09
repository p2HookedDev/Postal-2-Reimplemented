// Projector decals (tagged by `projector_tag`) are retail's projector pass over the finished surface,
// unlit in display space; `apply_decals` handles other decals before lighting, `project` after it.

import bevy_pbr::render::{
    pbr_types::{PbrInput, STANDARD_MATERIAL_FLAGS_UNLIT_BIT},
    clustered_forward,
    clustered_forward::ClusterableObjectIndexRanges,
    mesh_functions::get_tag,
    mesh_view_bindings,
    utils::porter_duff_over,
};
import bevy_pbr::decal::clustered::get_view_z;
import package::p2_engine::color::{to_display, to_linear};
@if(CLUSTERED_DECALS_ARE_USABLE)
import bevy_pbr::decal::clustered::{clustered_decal_iterator_new, clustered_decal_iterator_next};

const PROJECTOR: u32 = 0x80000000u;
// `EProjectorBlending` (the material stage, tag bits 8..16).
const PB_MODULATE: u32 = 1u;
const PB_ALPHA_BLEND: u32 = 2u;
const PB_ADD: u32 = 3u;
// `EFrameBufferBlending` (tag bits 0..8).
const FB_OVERWRITE: u32 = 0u;
const FB_MODULATE: u32 = 1u;
const FB_ALPHA_BLEND: u32 = 2u;
const FB_ALPHA_MODULATE: u32 = 3u;
const FB_TRANSLUCENT: u32 = 4u;
const FB_DARKEN: u32 = 5u;
const FB_BRIGHTEN: u32 = 6u;
// The alpha test bit; `AlphaRef` in bits 22..30.
const ALPHA_TEST: u32 = 0x200000u;
// What a surface is, for the projector's targets (tag bits 16..21, `ProjectorActor::targets`): its shader passes one.
const TARGET_BSP: u32 = 0x10000u;
const TARGET_TERRAIN: u32 = 0x20000u;
const TARGET_ACTOR: u32 = 0x80000u;
const TARGET_MESH: u32 = 0xc0000u;
const TARGETS: u32 = 0xf0000u;
const ON_UNLIT: u32 = 0x100000u;
const TAGGED: u32 = 0x40000000u;
// The `MeshTag` bits of a section (map/spawned.rs `projector_mesh_tag`): its actor is in no projector's list, so it
// refuses them; or it is in one, so it also takes a tagged projector. No tag at all takes untagged ones only: merged
// static meshes, whose actors all accept and carry no tag a projector aims at.
const NO_PROJECTORS: u32 = 1u;
const ATTACHED: u32 = 2u;
// A static mesh lit as an actor still takes static mesh projectors.
const STATIC_MESH: u32 = 4u;
const MOVER: u32 = 8u;
// In a projector's tag: it follows a mover and draws on movers only; in a surface: it is a mover.
const ON_MOVER: u32 = 0x80u;

fn section_target(instance_index: u32, kind: u32) -> u32 {
    let tag = get_tag(instance_index);
    if (tag & NO_PROJECTORS) != 0u {
        return 0u;
    }
    var surface = kind;
    if (tag & STATIC_MESH) != 0u {
        surface = TARGET_MESH;
    }
    if (tag & ATTACHED) != 0u {
        surface |= TAGGED;
    }
    if (tag & MOVER) != 0u {
        surface |= ON_MOVER;
    }
    return surface;
}

fn mesh_target(instance_index: u32) -> u32 {
    return section_target(instance_index, TARGET_MESH);
}

fn actor_target(instance_index: u32) -> u32 {
    return section_target(instance_index, TARGET_ACTOR);
}

fn cluster_ranges(pbr_input: PbrInput) -> ClusterableObjectIndexRanges {
    let view_z = get_view_z(pbr_input.world_position.xyz);
    let cluster_index = clustered_forward::view_fragment_cluster_index(pbr_input.frag_coord.xy, view_z, pbr_input.is_orthographic);
    return clustered_forward::unpack_clusterable_object_index_ranges(cluster_index);
}

@if(CLUSTERED_DECALS_ARE_USABLE)
fn decal_texel(index: i32, uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(
        mesh_view_bindings::clustered_decal_textures[index],
        mesh_view_bindings::clustered_decal_sampler,
        uv,
        0.0
    );
}

@if(CLUSTERED_DECALS_ARE_USABLE)
fn faces_projector(offset: i32, normal: vec3<f32>) -> bool {
    let index = clustered_forward::get_clusterable_object_id(u32(offset));
    let m = mesh_view_bindings::clustered_decals.decals[index].local_from_world;
    let direction = -normalize(vec3(m[0].z, m[1].z, m[2].z));
    return dot(normalize(normal), direction) < -0.0001;
}

// Bevy's over-blend of the base colour (decal::clustered `apply_decals`, base colour only: no decal here has other
// textures) for the decals that are not projectors, at alpha 1 with the surface's alpha put back: Bevy's over-blend
// multiplies the colour by the surface's alpha, which a straight alpha blend applies again. `ranges`: `cluster_ranges`.
fn apply_decals(pbr_input: ptr<function, PbrInput>, ranges: ClusterableObjectIndexRanges) {
@if(CLUSTERED_DECALS_ARE_USABLE) {
    var decal_ranges = ranges;
    var iterator = clustered_decal_iterator_new((*pbr_input).world_position.xyz, &decal_ranges);
    let alpha = (*pbr_input).material.base_color.a;
    var base_color = vec4((*pbr_input).material.base_color.rgb, 1.0);
    while clustered_decal_iterator_next(&iterator) {
        if (iterator.tag & PROJECTOR) == 0u && iterator.base_color_texture_index >= 0 {
            base_color = porter_duff_over(base_color, decal_texel(iterator.base_color_texture_index, iterator.uv));
        }
    }
    (*pbr_input).material.base_color = vec4(base_color.rgb, alpha);
}
}

// Over the lit linear `color`, in cluster order; `texture` is read by material blending. BSP takes a
// projector only where it faces it; a `bUnlit` static mesh still takes projectors (unverified gap).
fn project(pbr_input: PbrInput, ranges: ClusterableObjectIndexRanges, texture: vec3<f32>, color: vec4<f32>, surface: u32) -> vec4<f32> {
    var out = color;
@if(CLUSTERED_DECALS_ARE_USABLE) {
    var decal_ranges = ranges;
    var iterator = clustered_decal_iterator_new(pbr_input.world_position.xyz, &decal_ranges);
    var base = vec3(0.0);
    var frame = vec3(0.0);
    var drawn = false;
    while clustered_decal_iterator_next(&iterator) {
        if (iterator.tag & PROJECTOR) == 0u || iterator.base_color_texture_index < 0 || (iterator.tag & surface & TARGETS) == 0u {
            continue;
        }
        // `ProjectTag` filters actors only: BSP and terrain take the projector regardless.
        if (iterator.tag & TAGGED) != 0u && (surface & TARGET_MESH) != 0u && (surface & TAGGED) == 0u {
            continue;
        }
        if (iterator.tag & ON_MOVER) != 0u && (surface & ON_MOVER) == 0u {
            continue;
        }
        // Only BSP's unlit flag means retail's (a surface without a lightmap, mesh_view); terrain and meshes are
        // flagged unlit because their shaders light them themselves, which hid every projector on them.
        if surface == TARGET_BSP && (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) != 0u && (iterator.tag & ON_UNLIT) == 0u {
            continue;
        }
        if surface == TARGET_BSP && !faces_projector(iterator.decal_index_offset, pbr_input.world_normal) {
            continue;
        }
        let texel = decal_texel(iterator.base_color_texture_index, iterator.uv);
        if (iterator.tag & ALPHA_TEST) != 0u && texel.a * 255.0 <= f32((iterator.tag >> 22u) & 0xffu) {
            continue;
        }
        if !drawn {
            base = to_display(texture);
            frame = to_display(color.rgb);
            drawn = true;
        }
        let projected = to_display(texel.rgb);
        let material = (iterator.tag >> 8u) & 0xffu;
        var src = projected;
        if material == PB_MODULATE {
            src = projected * base;
        } else if material == PB_ALPHA_BLEND {
            src = mix(projected, base, texel.a);
        } else if material == PB_ADD {
            src = min(projected + base, vec3(1.0));
        }
        switch iterator.tag & 0x7fu {
            case FB_OVERWRITE: { frame = src; }
            case FB_MODULATE: { frame = 2.0 * src * frame; }
            case FB_ALPHA_BLEND: { frame = mix(frame, src, texel.a); }
            case FB_ALPHA_MODULATE: { frame = src + frame * (1.0 - texel.a); }
            case FB_TRANSLUCENT: { frame = src + frame; }
            case FB_DARKEN: { frame = frame * (1.0 - src); }
            case FB_BRIGHTEN: { frame = src + frame * (1.0 - src); }
            default: {}
        }
        frame = min(frame, vec3(1.0));
    }
    if drawn {
        out = vec4(to_linear(frame), color.a);
    }
}
    return out;
}
