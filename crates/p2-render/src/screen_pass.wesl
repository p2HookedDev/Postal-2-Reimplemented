fn view_at(view_from_clip: mat4x4<f32>, uv: vec2<f32>, z: f32) -> vec3<f32> {
    let p = view_from_clip * vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return p.xyz / p.w;
}

fn viewport_uv(at: vec2<f32>, viewport: vec4<f32>) -> vec2<f32> {
    return (at - viewport.xy) / viewport.zw;
}

fn texel_at(uv: vec2<f32>, viewport: vec4<f32>) -> vec2<i32> {
    return vec2<i32>(viewport.xy + uv * viewport.zw);
}

fn gradient_noise(at: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(at, vec2(0.06711056, 0.00583715))));
}

fn pattern(pixel: vec2<u32>) -> u32 {
    let x = pixel.x & 3u;
    let y = pixel.y & 3u;
    let a = x ^ y;
    return ((a & 1u) << 3u) | ((y & 1u) << 2u) | (a & 2u) | ((y & 2u) >> 1u);
}
