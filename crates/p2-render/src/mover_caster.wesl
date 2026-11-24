struct Caster {
    right: vec4<f32>,
    up: vec4<f32>,
    axis: vec4<f32>,
    lamp: vec4<f32>,
    terms: vec4<f32>,
    world_from_mesh: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> caster: Caster;
@group(1) @binding(0) var<storage, read> joints: array<mat4x4<f32>>;

struct Output {
    @builtin(position) clip: vec4<f32>,
    @location(0) from_lamp: vec3<f32>,
}

fn placed(world: vec3<f32>) -> Output {
    let from_lamp = world - caster.lamp.xyz;
    let along = dot(from_lamp, caster.axis.xyz);
    var out: Output;
    out.clip = vec4(dot(from_lamp, caster.right.xyz) * caster.terms.x, dot(from_lamp, caster.up.xyz) * caster.terms.x, caster.terms.y * along + caster.terms.z, along);
    out.from_lamp = from_lamp;
    return out;
}

@vertex
fn rigid_vertex(@location(0) position: vec3<f32>) -> Output {
    return placed((caster.world_from_mesh * vec4(position, 1.0)).xyz);
}

@vertex
fn skinned_vertex(@location(0) position: vec3<f32>, @location(1) joint_indices: vec4<u32>, @location(2) joint_weights: vec4<f32>) -> Output {
    let first = u32(caster.terms.w);
    let local = vec4(position, 1.0);
    let world = joint_weights.x * (joints[first + joint_indices.x] * local)
        + joint_weights.y * (joints[first + joint_indices.y] * local)
        + joint_weights.z * (joints[first + joint_indices.z] * local)
        + joint_weights.w * (joints[first + joint_indices.w] * local);
    return placed(world.xyz);
}

@fragment
fn fragment(in: Output) -> @location(0) vec4<f32> {
    return vec4(length(in.from_lamp) * caster.lamp.w, 0.0, 0.0, 1.0);
}

@vertex
fn clear_vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    return vec4(f32(index & 1u) * 4.0 - 1.0, f32(index >> 1u) * 4.0 - 1.0, 0.0, 1.0);
}

@fragment
fn clear_fragment() -> @location(0) vec4<f32> {
    return vec4(1.0);
}
