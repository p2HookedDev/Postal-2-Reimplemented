import bevy_pbr::render::{
    mesh_view_bindings::clustered_lights,
    mesh_view_types::{POINT_LIGHT_FLAGS_SPOT_LIGHT_BIT, POINT_LIGHT_FLAGS_SPOT_LIGHT_Y_NEGATIVE},
    clustered_forward as clustering,
};
import package::p2_engine::modern::{Relief, highlight};

const LE_NON_INCIDENCE: u32 = 13u;
const LE_CYLINDER: u32 = 17u;

fn sample_intensity(light_id: u32, position: vec3<f32>, normal: vec3<f32>) -> f32 {
    let light = clustered_lights.data[light_id];
    let radius = light.range;
    let to_light = light.position_radius.xyz - position;
    let distance = length(to_light);
    let facing = dot(to_light, normal);
    if (light.flags & POINT_LIGHT_FLAGS_SPOT_LIGHT_BIT) == 0u {
        let effect = u32(light.position_radius.w + 0.5);
        if effect == LE_NON_INCIDENCE {
            return select(0.0, sqrt(1.02 - distance / radius) * 2.0, facing > 0.0 && distance < radius);
        }
        if effect == LE_CYLINDER {
            let across = to_light.x * to_light.x + to_light.z * to_light.z;
            return select(0.0, max(1.0 - across / (radius * radius), 0.0) * 2.0, distance < radius);
        }
        return attenuation(distance, radius, facing);
    }
    let base = attenuation(distance, radius, facing);
    if base <= 0.0 {
        return 0.0;
    }
    var direction = vec3(light.light_custom_data.x, 0.0, light.light_custom_data.y);
    direction.y = sqrt(max(0.0, 1.0 - direction.x * direction.x - direction.z * direction.z));
    if (light.flags & POINT_LIGHT_FLAGS_SPOT_LIGHT_Y_NEGATIVE) != 0u {
        direction.y = -direction.y;
    }
    // ((cos − sine) / (1 − sine))² inside the cone: Bevy's spot scale and offset hold 1 / (1 − sine), −sine / (1 − sine).
    let falloff = saturate(dot(-direction, to_light / max(distance, 1e-6)) * light.light_custom_data.z + light.light_custom_data.w);
    return falloff * falloff * base;
}

// Engine falloff, facing and within the radius: 2 · cos · (2a³ − 3a² + 1) with a = d / r.
fn attenuation(distance: f32, radius: f32, facing: f32) -> f32 {
    if facing <= 0.0 || distance > radius {
        return 0.0;
    }
    let a = distance / radius;
    return 2.0 * facing / max(distance, 1e-6) * (2.0 * a * a * a - 3.0 * a * a + 1.0);
}

fn dynamic_light(world_position: vec4<f32>, normal: vec3<f32>, ranges: clustering::ClusterableObjectIndexRanges) -> vec3<f32> {
    var light = vec3(0.0);
    for (var i = ranges.first_point_light_index_offset; i < ranges.first_rect_light_index_offset; i++) {
        let id = clustering::get_clusterable_object_id(i);
        let intensity = sample_intensity(id, world_position.xyz, normal);
        if intensity > 0.0 {
            light += clustered_lights.data[id].color_inverse_square_range.rgb * 0.5 * intensity;
        }
    }
    return light;
}

struct ReliefLight {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
}

fn dynamic_light_relief(world_position: vec4<f32>, relief: Relief, eye: vec3<f32>, ranges: clustering::ClusterableObjectIndexRanges) -> ReliefLight {
    var out = ReliefLight(vec3(0.0), vec3(0.0));
    for (var i = ranges.first_point_light_index_offset; i < ranges.first_rect_light_index_offset; i++) {
        let id = clustering::get_clusterable_object_id(i);
        let intensity = sample_intensity(id, world_position.xyz, relief.normal);
        if intensity > 0.0 {
            let color = clustered_lights.data[id].color_inverse_square_range.rgb * 0.5 * intensity;
            let to_light = clustered_lights.data[id].position_radius.xyz - world_position.xyz;
            out.diffuse += color;
            out.specular += color * highlight(relief, to_light / max(length(to_light), 1e-6), eye);
        }
    }
    return out;
}
