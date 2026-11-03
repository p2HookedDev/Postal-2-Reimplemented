// A `Shader`'s layers over the lit colour. Glow: baked into the emissive texture (glow × mask, alpha mask) while the
// base texture holds diffuse × (1 − mask), so lit + glow is the driver's lerp(lit diffuse, glow, mask). With a separate
// opacity the base texture is the plain diffuse and the glow blends by its alpha.

import bevy_pbr::render::{pbr_bindings, pbr_types, forward_io::VertexOutput};
import package::p2_engine::color::{to_display, to_linear};
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};

// The driver's detail stage on linear colours: current × detail × 2 in display space, saturating, is
// min(lit · 2^2.2 · detail, 1) once both are linear (`DUMMY_MODULATE2X`, unverified).
fn detail(lit: vec3<f32>, texel: vec3<f32>) -> vec3<f32> {
    return min(lit * 4.5947934 * texel, vec3(1.0));
}

// The driver's specular stage on linear colours: current + mask × specular in display space, saturating
// (`D3DTOP_ADD` / `MODULATEALPHA_ADDCOLOR` / `MULTIPLYADD` with the texture factor, unverified).
fn specular(lit: vec3<f32>, texel: vec3<f32>, mask: f32) -> vec3<f32> {
    let display = to_display(lit) + mask * to_display(texel);
    return to_linear(min(display, vec3(1.0)));
}

fn glow(in: VertexOutput, flags: u32) -> vec4<f32> {
@if(VERTEX_UVS_A) {
    if (flags & pbr_types::STANDARD_MATERIAL_FLAGS_EMISSIVE_TEXTURE_BIT) == 0u {
        return vec4(0.0);
    }
@if(BINDLESS)
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
@if(BINDLESS)
    let emissive = pbr_bindings::material_array[material_indices[slot].material].emissive.rgb;
@else
    let emissive = pbr_bindings::material.emissive.rgb;
@if(BINDLESS)
    let uv_transform = pbr_bindings::material_array[material_indices[slot].material].uv_transform;
@else
    let uv_transform = pbr_bindings::material.uv_transform;
@if(BINDLESS)
    let texture = bindless_textures_2d[material_indices[slot].emissive_texture];
@else
    let texture = pbr_bindings::emissive_texture;
@if(BINDLESS)
    let sampler_ = bindless_samplers_filtering[material_indices[slot].emissive_sampler];
@else
    let sampler_ = pbr_bindings::emissive_sampler;
    // At the base's UV: the glow is baked to line up with the base's holes, only without UV animation.
    // A modifier on `Diffuse` alone would not move it.
    let texel = textureSample(texture, sampler_, (uv_transform * vec3(in.uv, 1.0)).xy);
    return vec4(emissive * texel.rgb, texel.a);
}
@else {
    return vec4(0.0);
}
}
