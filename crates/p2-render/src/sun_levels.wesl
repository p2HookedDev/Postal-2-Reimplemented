// The cached sun levels as `sun_levels.rs` lays them out: tiles of depth, and under them one row of records. A
// material binds the image in its depth map slot, a particle in its own group (`P2_PARTICLE_LEVELS`); Bevy's 1 x 1
// default image stands for no levels. The image is fetched at each read, as `lamp_shadow.wesl` fetches its atlas.
@if(!P2_PARTICLE_LEVELS && !BINDLESS)
import bevy_pbr::render::pbr_bindings;
@if(!P2_PARTICLE_LEVELS && BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};

@if(P2_PARTICLE_LEVELS)
@group(3) @binding(5) var particle_levels: texture_2d<f32>;
@if(P2_PARTICLE_LEVELS)
@group(3) @binding(6) var particle_levels_sampler: sampler;
@if(LAMP_MASK_PASS)
@group(1) @binding(8) var pass_levels: texture_2d<f32>;
@if(LAMP_MASK_PASS)
@group(1) @binding(9) var pass_levels_sampler: sampler;

const HEADER_TEXELS: u32 = 1u;
const RECORD_TEXELS: u32 = 24u;
// Hardware filtering weighs its four compares with 8-bit fractions on desktop GPUs; the gather does the same.
const WEIGHT_STEPS: f32 = 256.0;

// The material's bindless texture and sampler; unread without bindless.
struct LevelSource {
    texture: u32,
    sampling: u32,
}

struct LevelAtlas {
    source: LevelSource,
    size: vec2<u32>,
    // Without a filterable 32-bit float format each float's bits fill the four bytes of a texel.
    packed: bool,
}

struct LevelRecord {
    clip_from_world: mat4x4<f32>,
    texel_size: f32,
    sky: bool,
    direction_to_light: vec3<f32>,
    corner: vec2<f32>,
    size: f32,
}

fn source_size(source: LevelSource) -> vec2<u32> {
@if(LAMP_MASK_PASS) {
    return textureDimensions(pass_levels);
}
@elif(P2_PARTICLE_LEVELS) {
    return textureDimensions(particle_levels);
}
@elif(BINDLESS) {
    return textureDimensions(bindless_textures_2d[source.texture]);
}
@else {
    return textureDimensions(pbr_bindings::depth_map_texture);
}
}

fn source_load(source: LevelSource, at: vec2<u32>) -> vec4<f32> {
@if(LAMP_MASK_PASS) {
    return textureLoad(pass_levels, at, 0);
}
@elif(P2_PARTICLE_LEVELS) {
    return textureLoad(particle_levels, at, 0);
}
@elif(BINDLESS) {
    return textureLoad(bindless_textures_2d[source.texture], at, 0);
}
@else {
    return textureLoad(pbr_bindings::depth_map_texture, at, 0);
}
}

fn source_gather(source: LevelSource, uv: vec2<f32>) -> vec4<f32> {
@if(LAMP_MASK_PASS) {
    return textureGather(0, pass_levels, pass_levels_sampler, uv);
}
@elif(P2_PARTICLE_LEVELS) {
    return textureGather(0, particle_levels, particle_levels_sampler, uv);
}
@elif(BINDLESS) {
    return textureGather(0, bindless_textures_2d[source.texture], bindless_samplers_filtering[source.sampling], uv);
}
@else {
    return textureGather(0, pbr_bindings::depth_map_texture, pbr_bindings::depth_map_sampler, uv);
}
}

// Columns r, g, b and a of the four texels.
fn source_gather_bytes(source: LevelSource, uv: vec2<f32>) -> mat4x4<f32> {
@if(LAMP_MASK_PASS) {
    return mat4x4<f32>(
        textureGather(0, pass_levels, pass_levels_sampler, uv),
        textureGather(1, pass_levels, pass_levels_sampler, uv),
        textureGather(2, pass_levels, pass_levels_sampler, uv),
        textureGather(3, pass_levels, pass_levels_sampler, uv),
    );
}
@elif(P2_PARTICLE_LEVELS) {
    return mat4x4<f32>(
        textureGather(0, particle_levels, particle_levels_sampler, uv),
        textureGather(1, particle_levels, particle_levels_sampler, uv),
        textureGather(2, particle_levels, particle_levels_sampler, uv),
        textureGather(3, particle_levels, particle_levels_sampler, uv),
    );
}
@elif(BINDLESS) {
    return mat4x4<f32>(
        textureGather(0, bindless_textures_2d[source.texture], bindless_samplers_filtering[source.sampling], uv),
        textureGather(1, bindless_textures_2d[source.texture], bindless_samplers_filtering[source.sampling], uv),
        textureGather(2, bindless_textures_2d[source.texture], bindless_samplers_filtering[source.sampling], uv),
        textureGather(3, bindless_textures_2d[source.texture], bindless_samplers_filtering[source.sampling], uv),
    );
}
@else {
    return mat4x4<f32>(
        textureGather(0, pbr_bindings::depth_map_texture, pbr_bindings::depth_map_sampler, uv),
        textureGather(1, pbr_bindings::depth_map_texture, pbr_bindings::depth_map_sampler, uv),
        textureGather(2, pbr_bindings::depth_map_texture, pbr_bindings::depth_map_sampler, uv),
        textureGather(3, pbr_bindings::depth_map_texture, pbr_bindings::depth_map_sampler, uv),
    );
}
}

fn unpacked(texel: vec4<f32>, packed: bool) -> f32 {
    if packed {
        return bitcast<f32>(pack4x8unorm(texel));
    }
    return texel.x;
}

fn level_atlas(source: LevelSource) -> LevelAtlas {
    let size = source_size(source);
    // A float texel loads with alpha 1; a packed level count's high byte is never 255.
    let packed = size.x > 1u && source_load(source, vec2(0u, size.y - 1u)).w < 1.0;
    return LevelAtlas(source, size, packed);
}

fn record_texel(atlas: LevelAtlas, index: u32) -> f32 {
    return unpacked(source_load(atlas.source, vec2(index, atlas.size.y - 1u)), atlas.packed);
}

fn record_row(atlas: LevelAtlas, at: u32) -> vec4<f32> {
    return vec4(record_texel(atlas, at), record_texel(atlas, at + 1u), record_texel(atlas, at + 2u), record_texel(atlas, at + 3u));
}

fn level_count(atlas: LevelAtlas) -> u32 {
    if atlas.size.x <= 1u {
        return 0u;
    }
    return u32(record_texel(atlas, 0u));
}

fn level_first(level: u32) -> u32 {
    return HEADER_TEXELS + level * RECORD_TEXELS;
}

fn level_is_sky(atlas: LevelAtlas, level: u32) -> bool {
    return record_texel(atlas, level_first(level) + 17u) > 0.5;
}

fn level_texel_size(atlas: LevelAtlas, level: u32) -> f32 {
    return record_texel(atlas, level_first(level) + 16u);
}

// The matrix is stored by rows: the first two give a point's place in the level, all a cheap test needs.
fn level_rows_xy(atlas: LevelAtlas, level: u32) -> mat2x4<f32> {
    let first = level_first(level);
    return mat2x4<f32>(record_row(atlas, first), record_row(atlas, first + 4u));
}

fn level_xy(rows: mat2x4<f32>, position: vec4<f32>) -> vec2<f32> {
    return vec2(dot(rows[0], position), dot(rows[1], position));
}

fn level_record(atlas: LevelAtlas, level: u32) -> LevelRecord {
    let first = level_first(level);
    let rows = mat4x4<f32>(record_row(atlas, first), record_row(atlas, first + 4u), record_row(atlas, first + 8u), record_row(atlas, first + 12u));
    return LevelRecord(
        transpose(rows),
        record_texel(atlas, first + 16u),
        record_texel(atlas, first + 17u) > 0.5,
        vec3(record_texel(atlas, first + 18u), record_texel(atlas, first + 19u), record_texel(atlas, first + 20u)),
        vec2(record_texel(atlas, first + 21u), record_texel(atlas, first + 22u)),
        record_texel(atlas, first + 23u),
    );
}

// As Bevy's `world_to_directional_light_local`: xy the tile's uv, z the depth, w 0 outside the level.
fn level_local(record: LevelRecord, position: vec4<f32>) -> vec4<f32> {
    let clip = record.clip_from_world * position;
    if clip.w <= 0.0 {
        return vec4(0.0);
    }
    let ndc = clip.xyz / clip.w;
    if any(ndc.xy < vec2(-1.0)) || ndc.z < 0.0 || any(ndc > vec3(1.0)) {
        return vec4(0.0);
    }
    return vec4(ndc.xy * vec2(0.5, -0.5) + vec2(0.5), ndc.z, 1.0);
}

fn atlas_quad(atlas: LevelAtlas, at: vec2<f32>) -> vec4<f32> {
    let uv = at / vec2<f32>(atlas.size);
    if !atlas.packed {
        return source_gather(atlas.source, uv);
    }
    let bytes = transpose(source_gather_bytes(atlas.source, uv));
    return vec4(
        bitcast<f32>(pack4x8unorm(bytes[0])),
        bitcast<f32>(pack4x8unorm(bytes[1])),
        bitcast<f32>(pack4x8unorm(bytes[2])),
        bitcast<f32>(pack4x8unorm(bytes[3])),
    );
}

// One 2 x 2 compare filter in the level's tile, as a comparison sampler with reverse Z gives it: a texel is lit when the
// depth is at or in front of it. Taps stay half a texel inside the tile, as the tile's own edge would clamp them.
fn level_tap(atlas: LevelAtlas, record: LevelRecord, uv: vec2<f32>, depth: f32) -> f32 {
    let spot = clamp(uv * record.size - 0.5, vec2(0.0), vec2(record.size - 1.0 - 1.0 / WEIGHT_STEPS));
    let base = floor(spot);
    let blend = floor((spot - base) * WEIGHT_STEPS) / WEIGHT_STEPS;
    let lit = step(atlas_quad(atlas, record.corner + base + 1.0), vec4(depth));
    return mix(mix(lit.w, lit.z, blend.x), mix(lit.x, lit.y, blend.x), blend.y);
}
