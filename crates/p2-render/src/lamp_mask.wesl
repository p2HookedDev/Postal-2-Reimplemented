// Sun shadows for each world camera pixel, cast after its depth prepass with the same code the lit surfaces run,
// which then read these in place of their own.

import bevy_pbr::render::mesh_view_bindings::view;
import package::p2_render::lamp_shadow::{SCREEN_SUN_FROM, SCREEN_SUN_PIXELS};
import package::p2_render::sun_levels::{LevelSource, share_records};
import package::p2_render::sun_shadow::{sun_held, sun_index, world_lookup};

@group(1) @binding(0) var depth: texture_depth_2d;
@group(1) @binding(1) var normals: texture_2d<f32>;
@group(1) @binding(6) var mask: texture_storage_2d<rgba32float, write>;

// Matches `lamps/shadow_mask.rs`.
const GROUP_SIDE: u32 = 8u;

var<workgroup> suns: array<array<u32, GROUP_SIDE>, GROUP_SIDE>;

@compute @workgroup_size(GROUP_SIDE, GROUP_SIDE, 1)
fn shade(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>, @builtin(local_invocation_index) invocation: u32) {
    share_records(LevelSource(0u, 0u), invocation, GROUP_SIDE * GROUP_SIDE);
    workgroupBarrier();
    let size = textureDimensions(depth);
    // The header, which `shadow_mask.rs` marks unwritten before the pass: 1, then two columns of the view's matrix,
    // so another view keeps its own shadows.
    if all(id.xy == vec2(0u)) {
        textureStore(mask, vec2(0u, 0u), vec4(1.0, 0.0, 0.0, 0.0));
        textureStore(mask, vec2(1u, 0u), view.clip_from_world[0]);
        textureStore(mask, vec2(2u, 0u), view.clip_from_world[3]);
    }
    let inside = all(id.xy < size);
    var sun = 0u;
    if inside {
        sun = sun_bytes(id.xy);
    }
    suns[local.y][local.x] = sun;
    workgroupBarrier();
    if inside && local.y % SCREEN_SUN_PIXELS == 0u {
        let rows = vec4(suns[local.y][local.x], suns[local.y + 1u][local.x], suns[local.y + 2u][local.x], suns[local.y + 3u][local.x]);
        textureStore(mask, vec2(id.x, SCREEN_SUN_FROM + id.y / SCREEN_SUN_PIXELS), vec4<f32>(rows));
    }
}

fn sun_bytes(pixel: vec2<u32>) -> u32 {
    let z = textureLoad(depth, pixel, 0);
    if z <= 0.0 {
        return 0u;
    }
    let held = sun_held(LevelSource(0u, 0u), sun_index(), vec4(pixel_position(pixel, z), 1.0), pixel_normal(pixel), vec2<f32>(pixel) + 0.5, world_lookup());
    let bytes = vec3<u32>(round(saturate(held) * 255.0));
    return bytes.x | (bytes.y << 8u) | (bytes.z << 16u);
}

fn pixel_position(pixel: vec2<u32>, z: f32) -> vec3<f32> {
    let uv = (vec2<f32>(pixel) + 0.5 - view.viewport.xy) / view.viewport.zw;
    let seen = view.world_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return seen.xyz / seen.w;
}

fn pixel_normal(pixel: vec2<u32>) -> vec3<f32> {
    return normalize(textureLoad(normals, pixel, 0).xyz * 2.0 - 1.0);
}
