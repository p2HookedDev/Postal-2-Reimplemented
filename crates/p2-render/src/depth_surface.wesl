fn world_at(depth: texture_depth_2d, world_from_clip: mat4x4<f32>, pixel: vec2<i32>, size: vec2<f32>) -> vec3<f32> {
    let uv = (vec2<f32>(pixel) + 0.5) / size;
    let clip = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, textureLoad(depth, pixel, 0), 1.0);
    let world = world_from_clip * clip;
    return world.xyz / world.w;
}

fn eye_distance(view_from_world: mat4x4<f32>, p: vec3<f32>) -> f32 {
    return -(view_from_world * vec4(p, 1.0)).z;
}

fn normal_at(depth: texture_depth_2d, world_from_clip: mat4x4<f32>, eye: vec3<f32>, pixel: vec2<i32>, size: vec2<f32>, surface: vec3<f32>) -> vec3<f32> {
    let across = world_at(depth, world_from_clip, pixel + vec2(1, 0), size) - world_at(depth, world_from_clip, pixel - vec2(1, 0), size);
    let down = world_at(depth, world_from_clip, pixel + vec2(0, 1), size) - world_at(depth, world_from_clip, pixel - vec2(0, 1), size);
    let facing = cross(across, down);
    let to_eye = normalize(eye - surface);
    let n = select(to_eye, normalize(facing), dot(facing, facing) > 1e-8);
    return select(n, -n, dot(n, to_eye) < 0.0);
}

fn on_surface(view_from_world: mat4x4<f32>, eye: vec3<f32>, p: vec3<f32>, surface: vec3<f32>, n: vec3<f32>, lift: f32) -> vec3<f32> {
    return eye + (p - eye) * (eye_distance(view_from_world, surface) / eye_distance(view_from_world, p)) + n * lift;
}
