// Bevy's bloom as compute steps over one texture: layer 0 holds the downsampled mips, layer 1 sums them from the
// smallest up, and a fragment pass adds the top of that sum to the frame.

const WORKGROUP: u32 = 8u;

struct BloomStep {
    threshold: vec4<f32>,
    viewport: vec4<f32>,
    blend: f32,
}

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var bloom_sampler: sampler;
@group(0) @binding(2) var<uniform> bloom: BloomStep;
@group(0) @binding(3) var written: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var base_texture: texture_2d<f32>;

fn soft_threshold(color: vec3<f32>) -> vec3<f32> {
    let brightness = max(color.r, max(color.g, color.b));
    var softness = clamp(brightness - bloom.threshold.y, 0.0, bloom.threshold.z);
    softness = softness * softness * bloom.threshold.w;
    let contribution = max(brightness - bloom.threshold.x, softness) / max(brightness, 0.00001);
    return color * contribution;
}

fn karis_average(color: vec3<f32>) -> f32 {
    let luma = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722)) / 4.0;
    return 1.0 / (1.0 + luma);
}

struct Taps {
    a: vec3<f32>,
    b: vec3<f32>,
    c: vec3<f32>,
    d: vec3<f32>,
    e: vec3<f32>,
    f: vec3<f32>,
    g: vec3<f32>,
    h: vec3<f32>,
    i: vec3<f32>,
    j: vec3<f32>,
    k: vec3<f32>,
    l: vec3<f32>,
    m: vec3<f32>,
}

fn taps_13(uv: vec2<f32>) -> Taps {
    return Taps(
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(-2, 2)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(0, 2)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(2, 2)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(-2, 0)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(2, 0)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(-2, -2)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(0, -2)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(2, -2)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(-1, 1)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(1, 1)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(-1, -1)).rgb,
        textureSampleLevel(source_texture, bloom_sampler, uv, 0.0, vec2<i32>(1, -1)).rgb
    );
}

// Each group weighted by its brightness, so a single very bright pixel does not bloom.
fn downsample_karis(uv: vec2<f32>) -> vec3<f32> {
    let t = taps_13(uv);
    let group0 = (t.a + t.b + t.d + t.e) * (0.125 / 4.0);
    let group1 = (t.b + t.c + t.e + t.f) * (0.125 / 4.0);
    let group2 = (t.d + t.e + t.g + t.h) * (0.125 / 4.0);
    let group3 = (t.e + t.f + t.h + t.i) * (0.125 / 4.0);
    let group4 = (t.j + t.k + t.l + t.m) * (0.5 / 4.0);
    return group0 * karis_average(group0) + group1 * karis_average(group1) + group2 * karis_average(group2)
        + group3 * karis_average(group3) + group4 * karis_average(group4);
}

fn downsample_13(uv: vec2<f32>) -> vec3<f32> {
    let t = taps_13(uv);
    return (t.a + t.c + t.g + t.i) * 0.03125 + (t.b + t.d + t.f + t.h) * 0.0625 + (t.e + t.j + t.k + t.l + t.m) * 0.125;
}

fn tent(uv: vec2<f32>) -> vec3<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(source_texture));
    let x = texel.x;
    let y = texel.y;
    let a = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x - x, uv.y + y), 0.0).rgb;
    let b = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x, uv.y + y), 0.0).rgb;
    let c = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x + x, uv.y + y), 0.0).rgb;
    let d = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x - x, uv.y), 0.0).rgb;
    let e = textureSampleLevel(source_texture, bloom_sampler, uv, 0.0).rgb;
    let f = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x + x, uv.y), 0.0).rgb;
    let g = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x - x, uv.y - y), 0.0).rgb;
    let h = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x, uv.y - y), 0.0).rgb;
    let i = textureSampleLevel(source_texture, bloom_sampler, vec2<f32>(uv.x + x, uv.y - y), 0.0).rgb;
    return e * 0.25 + (b + d + f + h) * 0.125 + (a + c + g + i) * 0.0625;
}

fn texel_uv(id: vec2<u32>) -> vec2<f32> {
    return (vec2<f32>(id) + 0.5) / vec2<f32>(textureDimensions(written));
}

@compute @workgroup_size(WORKGROUP, WORKGROUP, 1)
fn downsample_first(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(written)) {
        return;
    }
    let uv = bloom.viewport.xy + texel_uv(id.xy) * bloom.viewport.zw;
    // Above zero so the chain never multiplies into black boxes; below the float limit so no NaN spreads.
    let color = clamp(downsample_karis(uv), vec3<f32>(0.0001), vec3<f32>(3.40282347e37));
    textureStore(written, id.xy, vec4<f32>(soft_threshold(color), 1.0));
}

@compute @workgroup_size(WORKGROUP, WORKGROUP, 1)
fn downsample(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(written)) {
        return;
    }
    textureStore(written, id.xy, vec4<f32>(downsample_13(texel_uv(id.xy)), 1.0));
}

@compute @workgroup_size(WORKGROUP, WORKGROUP, 1)
fn upsample(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(written)) {
        return;
    }
    let below = textureLoad(base_texture, id.xy, 0).rgb;
    textureStore(written, id.xy, vec4<f32>(tent(texel_uv(id.xy)) * bloom.blend + below, 1.0));
}

@fragment
fn composite(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(tent(uv) * bloom.blend, 1.0);
}
