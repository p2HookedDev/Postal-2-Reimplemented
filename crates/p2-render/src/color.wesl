// Display space (the frame buffer's gamma, as `MODULATE_2X` and the light passes take colours) from linear: power
// 1 / 2.2. A `Modulate2X` light held linear, (2 · L)^2.2 (light maps, vertex-lit meshes), is L = to_display(x) / 2.
fn to_display(linear: vec3<f32>) -> vec3<f32> {
    return pow(max(linear, vec3(0.0)), vec3(1.0 / 2.2));
}

fn to_linear(display: vec3<f32>) -> vec3<f32> {
    return pow(max(display, vec3(0.0)), vec3(2.2));
}

// Blood, shared by its pools, streams, drops and the game's blood pictures: the colour it scatters back where it is
// deep, and what it absorbs a Unreal unit per channel. The colour is the crimson of the game's blood splat textures,
// sRGB (95, 0, 0), and stands as a game texture does, lit in display space. Red too is gone within a fraction of a
// unit, so only a thin edge shows the floor through.
const BLOOD_BODY: vec3<f32> = vec3(0.114, 0.0006, 0.0006);
const BLOOD_SIGMA: vec3<f32> = vec3(30.0, 420.0, 400.0);
