// Display space (the frame buffer's gamma, as `MODULATE_2X` and the light passes take colours) from linear: power
// 1 / 2.2. A `Modulate2X` light held linear, (2 · L)^2.2 (light maps, vertex-lit meshes), is L = to_display(x) / 2.
fn to_display(linear: vec3<f32>) -> vec3<f32> {
    return pow(max(linear, vec3(0.0)), vec3(1.0 / 2.2));
}

fn to_linear(display: vec3<f32>) -> vec3<f32> {
    return pow(max(display, vec3(0.0)), vec3(2.2));
}
