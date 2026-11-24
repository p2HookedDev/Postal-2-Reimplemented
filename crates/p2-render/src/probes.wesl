import package::p2_render::modern_base::LampLight;
import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_view_bindings::lights,
    pbr_bindings,
};
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};
@if(BINDLESS)
import bevy_pbr::render::{pbr_bindings::material_indices, mesh_bindings::mesh};

// The probe grid rides in the material's metal and roughness texture, Bevy's 1 x 1 default without one: two volumes side
// by side, each a tile per layer of the grid, a tile the grid's columns with a repeat at each side, half as many layers
// as columns. The tile count across and the bounce range match `probes.rs`; the spacing comes in the ambient's alpha.
const PROBE_TILES_WIDE: f32 = 12.0;
const PROBE_BOUNCE_RANGE: f32 = 2.0;
const PROBE_SMALLEST_ATLAS: f32 = 64.0;

struct ProbeAtlas {
    pages: u32,
    sampling: u32,
    size: vec2<f32>,
}

fn probe_atlas(in: VertexOutput) -> ProbeAtlas {
@if(BINDLESS) {
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    let pages = material_indices[slot].metallic_roughness_texture;
    return ProbeAtlas(pages, material_indices[slot].metallic_roughness_sampler, vec2<f32>(textureDimensions(bindless_textures_2d[pages])));
}
@else {
    return ProbeAtlas(0u, 0u, vec2<f32>(textureDimensions(pbr_bindings::metallic_roughness_texture)));
}
}

fn probe_tap(atlas: ProbeAtlas, volume: f32, layer: f32, x: f32, z: f32) -> vec4<f32> {
    let size = atlas.size;
    let tile_side = size.x / (2.0 * PROBE_TILES_WIDE);
    let layers = (tile_side - 2.0) * 0.5;
    let wrapped = layer - layers * floor(layer / layers);
    let tile = vec2(wrapped - PROBE_TILES_WIDE * floor(wrapped / PROBE_TILES_WIDE), floor(wrapped / PROBE_TILES_WIDE)) * tile_side;
    let uv = (tile + vec2(volume * tile_side * PROBE_TILES_WIDE + x + 1.5, z + 1.5)) / size;
@if(BINDLESS) {
    return textureSampleLevel(bindless_textures_2d[atlas.pages], bindless_samplers_filtering[atlas.sampling], uv, 0.0);
}
@else {
    return textureSampleLevel(pbr_bindings::metallic_roughness_texture, pbr_bindings::metallic_roughness_sampler, uv, 0.0);
}
}

// The surface's own side of the grid: half a step out of the wall it lies on.
fn read_probes(in: VertexOutput, position: vec3<f32>, normal: vec3<f32>, sum: ptr<function, LampLight>) {
    let spacing = lights.ambient_color.a;
    let atlas = probe_atlas(in);
    let size = atlas.size;
    if spacing <= 0.0 || size.x < PROBE_SMALLEST_ATLAS {
        return;
    }
    let columns = size.x / (2.0 * PROBE_TILES_WIDE) - 2.0;
    let cell = position / spacing + normal * 0.5;
    let layer = floor(cell.y);
    let x = cell.x - columns * floor(cell.x / columns);
    let z = cell.z - columns * floor(cell.z / columns);
    let up = cell.y - layer;
    let shown = mix(probe_tap(atlas, 0.0, layer, x, z).rgb, probe_tap(atlas, 0.0, layer + 1.0, x, z).rgb, up) * PROBE_BOUNCE_RANGE;
    (*sum).bounce = shown * shown;
    let sky = mix(probe_tap(atlas, 1.0, layer, x, z), probe_tap(atlas, 1.0, layer + 1.0, x, z), up);
    (*sum).sky = vec4(sky.r, sky.gba * 2.0 - 1.0);
}
