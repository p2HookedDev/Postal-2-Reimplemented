import bevy_pbr::render::mesh_view_bindings::view;

const LUMINANCE: vec3<f32> = vec3(0.2126, 0.7152, 0.0722);

const DIELECTRIC_F0: f32 = 0.04;
// A near-mirror's sub-pixel highlight is a firefly, so one light's peak stays under this.
const GGX_PEAK: f32 = 256.0;

const MODERN_ON: u32 = 1u;
const NO_RELIEF: u32 = 2u;
const NO_GLOSS: u32 = 4u;

fn modern_bits(flag: f32) -> u32 {
    return u32(max(flag, 0.0) + 0.5);
}

// `diffuse` in display light, `specular` and `bounce` in linear light; `specular_edge` is the highlight's part
// that grows towards grazing. `sky` is what a probe sees of the sky, x below 0 without a probe; `grounded` is
// how much light from all around reaches past nearby movers, 1 in the open. `unshadowed` is the lamp light, in
// display light, that no shadow test held.
struct LampLight {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
    specular_edge: vec3<f32>,
    bounce: vec3<f32>,
    sky: vec4<f32>,
    grounded: f32,
    unshadowed: vec3<f32>,
}

fn fresnel_five(cosine: f32) -> f32 {
    let f1 = 1.0 - cosine;
    let f2 = f1 * f1;
    return f2 * f2 * f1;
}

fn ggx(normal: vec3<f32>, to_light: vec3<f32>, eye: vec3<f32>, facing: f32, a: f32, f0: vec3<f32>) -> vec3<f32> {
    let a2 = a * a;
    let k = a * 0.5;
    let sum = to_light + eye;
    let halfway = sum * inverseSqrt(max(dot(sum, sum), 1e-12));
    let nh = saturate(dot(normal, halfway));
    let nv = max(dot(normal, eye), 1e-4);
    let vh = saturate(dot(eye, halfway));
    let d = nh * nh * (a2 - 1.0) + 1.0;
    let visibility = 0.25 / ((facing * (1.0 - k) + k) * (nv * (1.0 - k) + k));
    return min(a2 / (d * d) * visibility, GGX_PEAK) * (f0 + (vec3(1.0) - f0) * fresnel_five(vh)) * facing;
}

fn to_eye(world_position: vec3<f32>) -> vec3<f32> {
    let to_camera = view.world_position.xyz - world_position;
    return to_camera / max(length(to_camera), 1e-6);
}
