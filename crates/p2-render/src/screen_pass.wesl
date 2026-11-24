// What the modern full-screen passes share.

// The view-space point the view sees at `uv` and depth `z`.
fn view_at(view_from_clip: mat4x4<f32>, uv: vec2<f32>, z: f32) -> vec3<f32> {
    let p = view_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return p.xyz / p.w;
}
