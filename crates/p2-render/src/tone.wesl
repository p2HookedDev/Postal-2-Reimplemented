import package::p2_render::tone_curve::pbr_neutral;

// `sun`: its place on the screen in uv, the rays' strength (0 without rays) and the frame's width over its height.
// `tint`: the rays' colour. `grade`: exposure, saturation, contrast, vignette. `lens`: colour fringe in uv at the corner.
// `balance`: the white balance's gains on red, green and blue.
struct Post {
    sun: vec4<f32>,
    tint: vec4<f32>,
    grade: vec4<f32>,
    lens: vec4<f32>,
    balance: vec4<f32>,
}

@group(2) @binding(0) var scene: texture_2d<f32>;
@group(2) @binding(1) var scene_sampler: sampler;
@group(2) @binding(2) var<uniform> post: Post;

const LUMINANCE: vec3<f32> = vec3(0.2126, 0.7152, 0.0722);
const RAY_STEPS: i32 = 24;
// The share of the way to the sun a ray is followed.
const RAY_LENGTH: f32 = 0.9;
const RAY_DECAY: f32 = 0.96;
// In frame heights from the sun: how far the sky around it feeds rays, and how far from it a pixel still shows them.
const RAY_SOURCE: f32 = 0.45;
const RAY_REACH: f32 = 1.3;
const RAY_BRIGHTEST: f32 = 2.0;
const VIGNETTE_FROM: f32 = 0.35;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vertex(@location(0) position: vec3<f32>, @location(2) uv: vec2<f32>) -> VertexOutput {
    return VertexOutput(vec4<f32>(position.xy, 0.5, 1.0), uv);
}

fn noise(at: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(at, vec2(0.06711056, 0.00583715))));
}

fn across(offset: vec2<f32>) -> f32 {
    return length(offset * vec2(post.sun.w, 1.0));
}

// Light shafts: the sky near the sun, smeared away from it. The sky stores alpha 0 and the world's surfaces 1, so
// whatever stands before the sun cuts its shaft.
fn sun_rays(uv: vec2<f32>, pixel: vec2<f32>) -> vec3<f32> {
    let strength = post.sun.z;
    if strength <= 0.0 {
        return vec3(0.0);
    }
    let to_sun = post.sun.xy - uv;
    let reach = saturate(1.0 - across(to_sun) / RAY_REACH);
    if reach <= 0.0 {
        return vec3(0.0);
    }
    let step = to_sun * (RAY_LENGTH / f32(RAY_STEPS));
    var at = uv + step * noise(pixel);
    var weight = 1.0;
    var sum = vec3(0.0);
    for (var i = 0; i < RAY_STEPS; i++) {
        if all(at >= vec2(0.0)) && all(at <= vec2(1.0)) {
            let tap = textureSampleLevel(scene, scene_sampler, at, 0.0);
            let near = saturate(1.0 - across(post.sun.xy - at) / RAY_SOURCE);
            sum += min(tap.rgb, vec3(RAY_BRIGHTEST)) * ((1.0 - tap.a) * near * near * weight);
        }
        weight *= RAY_DECAY;
        at += step;
    }
    return sum * post.tint.rgb * (strength * reach * reach / f32(RAY_STEPS));
}

// Where in the flat frame a cylindrical (Panini) view finds what it shows at `uv`. `half`: the tangent of half the
// vertical field of view; `zoom` keeps the warped frame filling the screen.
fn panini(uv: vec2<f32>, strength: f32, half: f32, zoom: f32, aspect: f32) -> vec2<f32> {
    let extent = vec2(half * aspect, half);
    let at = (uv * 2.0 - 1.0) * extent * zoom;
    let away = 1.0 + strength;
    let hyp_sq = at.x * at.x + away * away;
    let slant = at.x * strength;
    let to_cylinder = (-slant * at.x + away * sqrt(hyp_sq - slant * slant)) / hyp_sq;
    let on_cylinder = at * ((to_cylinder + strength) / away);
    return on_cylinder / (to_cylinder * extent) * 0.5 + 0.5;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let from_middle = in.uv - vec2(0.5);
    var at = in.uv;
    if post.lens.y > 0.0 {
        at = panini(in.uv, post.lens.y, post.lens.z, post.lens.w, post.sun.w);
    }
    var color = textureSample(scene, scene_sampler, at).rgb;
    let fringe = post.lens.x;
    if fringe > 0.0 {
        let shift = from_middle * (2.0 * fringe * length(from_middle));
        color.r = textureSampleLevel(scene, scene_sampler, at + shift, 0.0).r;
        color.b = textureSampleLevel(scene, scene_sampler, at - shift, 0.0).b;
    }
    color = (color * post.grade.x + sun_rays(at, in.position.xy)) * post.balance.rgb;
    color = pbr_neutral(color);
    let grey = dot(color, LUMINANCE);
    color = max(mix(vec3(grey), color, post.grade.y), vec3(0.0));
    // Contrast turns about middle grey in display light, where the eye judges it.
    let shown = max((sqrt(color) - 0.5) * post.grade.z + 0.5, vec3(0.0));
    color = shown * shown;
    let corner = saturate((across(from_middle) - VIGNETTE_FROM) / (across(vec2(0.5)) - VIGNETTE_FROM));
    color *= 1.0 - post.grade.w * corner * corner;
    return vec4<f32>(color, 1.0);
}
