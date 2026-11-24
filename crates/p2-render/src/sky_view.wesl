// The sky zone is placed at eye + turn^-1 (p - sky_eye) on the CPU and its depth is squeezed behind every level
// depth, so the level covers it as retail's depth clear between the two scenes did.

const SKY_DEPTH_SCALE: f32 = 5.9604645e-8;

fn sky_clip(clip: vec4<f32>) -> vec4<f32> {
    return vec4(clip.xy, clip.z * SKY_DEPTH_SCALE, clip.w);
}

fn sky_before_near(world_position: vec3<f32>, world_from_view: mat4x4<f32>, near: f32) -> bool {
    return dot(world_position - world_from_view[3].xyz, -world_from_view[2].xyz) < near;
}

fn sky_true_position(placed: vec3<f32>, eye: vec3<f32>, sky_eye: vec3<f32>, turn: mat3x3<f32>) -> vec3<f32> {
    return sky_eye + turn * (placed - eye);
}
