// Retail cleared depth between the sky and the level; the sky's depth is squeezed behind every level depth instead.

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
