struct Caster {
    clip_from_world: mat4x4<f32>,
    world_from_local: mat4x4<f32>,
    cut: vec4<f32>,
}

@group(0) @binding(0) var<uniform> caster: Caster;
@group(0) @binding(1) var scratch: texture_depth_2d;
@group(1) @binding(0) var cut_texture: texture_2d<f32>;
@group(1) @binding(1) var cut_sampler: sampler;

struct Output {
    @builtin(position) clip: vec4<f32>,
    @location(0) unclipped_depth: f32,
    @location(1) uv: vec2<f32>,
}

fn placed(position: vec3<f32>, uv: vec2<f32>) -> Output {
    let world = caster.world_from_local * vec4(position, 1.0);
    var out: Output;
    out.clip = caster.clip_from_world * vec4(world.xyz, 1.0);
    out.unclipped_depth = out.clip.z;
    // Without depth clip control, as Bevy's emulation: clamp what reaches the rasteriser, the fragment writes the
    // true depth.
    @if(UNCLIPPED_DEPTH_ORTHO_EMULATION)
    out.clip.z = min(out.clip.z, 1.0);
    out.uv = uv;
    return out;
}

@vertex
fn prop_vertex(@location(0) position: vec3<f32>) -> Output {
    return placed(position, vec2(0.0));
}

@vertex
fn prop_vertex_uv(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> Output {
    return placed(position, uv);
}

fn cut_away(in: Output) {
    if textureSample(cut_texture, cut_sampler, in.uv).a < caster.cut.x {
        discard;
    }
}

@if(UNCLIPPED_DEPTH_ORTHO_EMULATION)
@fragment
fn prop_fragment(in: Output) -> @builtin(frag_depth) f32 {
    return in.unclipped_depth;
}

@if(UNCLIPPED_DEPTH_ORTHO_EMULATION)
@fragment
fn prop_fragment_masked(in: Output) -> @builtin(frag_depth) f32 {
    cut_away(in);
    return in.unclipped_depth;
}

@if(!UNCLIPPED_DEPTH_ORTHO_EMULATION)
@fragment
fn prop_fragment_masked(in: Output) {
    cut_away(in);
}

@vertex
fn copy_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return vec4(f32(index & 1u) * 4.0 - 1.0, f32(index >> 1u) * 4.0 - 1.0, 0.0, 1.0);
}

fn scratch_depth(pixel: vec4<f32>) -> f32 {
    return textureLoad(scratch, vec2<u32>(pixel.xy) % textureDimensions(scratch), 0);
}

@fragment
fn copy_fragment(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4(scratch_depth(pixel), 0.0, 0.0, 1.0);
}

@fragment
fn copy_fragment_packed(@builtin(position) pixel: vec4<f32>) -> @location(0) vec4<f32> {
    return unpack4x8unorm(bitcast<u32>(scratch_depth(pixel)));
}
