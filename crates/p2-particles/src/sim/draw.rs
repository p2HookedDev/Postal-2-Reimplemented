use super::{MeshParticle, Owner, Particle, ParticleEmitter, SpriteParticle, View};
use crate::math::{Rotator, Vector, fractional, non_parallel, rotate_about, rotate_by};
use crate::props::{CoordinateSystem, DirectionUsage, DrawStyle, EmitterProperties};
use crate::props::RotationSource;

impl ParticleEmitter {
    pub fn sprites(&self, owner: Owner, view: View) -> impl Iterator<Item = SpriteParticle> + '_ {
        let props = &self.props;
        let relative = props.coordinate_system == CoordinateSystem::Relative;
        let axes = owner.rotation.axes();
        let direction_to_world = move |v: Vector| if relative { rotate_by(&axes, v) } else { v };
        // Retail reads the cached `RealProjectionNormal` here, not `ProjectionNormal`.
        let normal = props.real_projection_normal;
        let rotated_normal = if props.use_rotation_from == RotationSource::Actor { rotate_by(&axes, normal) } else { normal };
        let (rotated_normal, normal) = (direction_to_world(rotated_normal), direction_to_world(normal));
        // The engine's camera "up" axis points down the screen (the texture's v0 edge is at -up).
        let (screen_right, screen_down) = (view.right, view.up * -1.0);
        let front = screen_right.cross(screen_down);
        let live: &[Particle] = if props.disabled { &[] } else { &self.particles[..self.active_particles] };
        live.iter().filter(|p| p.active).map(move |p| {
            let location = if relative { rotate_by(&axes, p.location) + owner.location } else { p.location };
            let moved = || direction_to_world((p.location - p.old_location).safe_normal());
            let [size_x, size_y] = [p.size.x, p.size.y];
            let (mut up, mut right, axis) = match props.use_direction_as {
                DirectionUsage::None => (screen_down * size_x, screen_right * size_x, front),
                DirectionUsage::Up | DirectionUsage::UpAndNormal => {
                    let moved = moved();
                    (moved * size_y, across(props.use_direction_as, moved, location, &view, normal) * size_x, front)
                }
                DirectionUsage::Right | DirectionUsage::RightAndNormal => {
                    let moved = moved();
                    (across(props.use_direction_as, moved, location, &view, normal) * size_y, moved * size_x, front)
                }
                DirectionUsage::Forward | DirectionUsage::Normal => {
                    let direction = if props.use_direction_as == DirectionUsage::Normal { rotated_normal } else { moved() };
                    // Not normalized (as in the engine): `Size.Y` times the length of the cross product.
                    let up = direction.cross(non_parallel(direction)) * size_y;
                    (up, direction.cross(up).safe_normal() * size_x, direction)
                }
            };
            if props.spin_particles {
                let angle = spin_units(p);
                (up, right) = (rotate_about(up, angle, axis), rotate_about(right, angle, axis));
            }
            let (uv, blend) = subdivision(props, p);
            SpriteParticle { location, size: [size_x, size_y], up, right, color: p.color, uv, blend }
        })
    }

    pub fn meshes(&self, owner: Owner) -> impl Iterator<Item = MeshParticle> + '_ {
        let props = &self.props;
        let relative = props.coordinate_system == CoordinateSystem::Relative;
        let frame = if relative { owner.rotation } else { Rotator::ZERO };
        let axes = owner.rotation.axes();
        let live: &[Particle] = if props.disabled { &[] } else { &self.particles[..self.active_particles] };
        live.iter().filter(|p| p.active).map(move |p| {
            let location = if relative { rotate_by(&axes, p.location) + owner.location } else { p.location };
            let spin = if props.spin_particles { mesh_spin(p) } else { Rotator::ZERO };
            MeshParticle { location, spin, frame, size: p.size, color: p.color }
        })
    }
}

fn mesh_spin(p: &Particle) -> Rotator {
    let turned = |axis: usize| (p.start_spin[axis] + p.time * p.spins_per_second[axis]) as i32;
    [turned(1), turned(0), turned(2)].into()
}

fn across(usage: DirectionUsage, moved: Vector, location: Vector, view: &View, normal: Vector) -> Vector {
    match usage {
        DirectionUsage::UpAndNormal | DirectionUsage::RightAndNormal => moved.cross(normal),
        _ => moved.cross(location - view.location).safe_normal(),
    }
}

fn relative_time(time: f32, max_lifetime: f32) -> f32 {
    if max_lifetime == 0.0 {
        return 0.0;
    }
    (time / max_lifetime).clamp(0.0, 1.0)
}

pub(super) fn key_span<K: Copy>(keys: &[K], t: f32, time: impl Fn(&K) -> f32) -> Option<(Option<K>, K, f32)> {
    let index = keys.iter().position(|key| t <= time(key))?;
    let key = keys[index];
    let previous = index.checked_sub(1).map(|before| keys[before]);
    let (key_time, previous_time) = (time(&key), previous.map_or(0.0, |before| time(&before)));
    let f = if key_time == 0.0 { 1.0 } else { (t - previous_time) / (key_time - previous_time) };
    Some((previous, key, f))
}

pub(super) fn size_scale(props: &EmitterProperties, time: f32, max_lifetime: f32) -> f32 {
    if !props.use_size_scale {
        return 1.0;
    }
    if props.use_regular_size_scale {
        return 1.0 / (time + 1.0);
    }
    if max_lifetime == 0.0 {
        return 1.0;
    }
    let t = fractional((props.size_scale_repeats + 1.0) * relative_time(time, max_lifetime));
    let Some((previous, key, f)) = key_span(&props.size_scale, t, |key| key.time) else { return 1.0 };
    let prev_size = previous.map_or(1.0, |before| before.size);
    (key.size - prev_size) * f + prev_size
}

pub(super) fn particle_color(props: &EmitterProperties, p: &Particle) -> [u8; 4] {
    let (time, max_lifetime) = (p.time, p.max_lifetime);
    let [mut r, mut g, mut b, mut a] = [255i32; 4];
    if props.use_color_scale && max_lifetime != 0.0 {
        let t = fractional((props.color_scale_repeats + 1.0) * relative_time(time, max_lifetime));
        if let Some((previous, key, f)) = key_span(&props.color_scale, t, |key| key.time) {
            let prev = previous.map_or([255; 4], |before| before.color);
            let lerp = |c: usize| ((i32::from(key.color[c]) - i32::from(prev[c])) as f32 * f + f32::from(prev[c])) as i32;
            [r, g, b] = [lerp(0), lerp(1), lerp(2)];
            a = if props.draw_style == DrawStyle::AlphaBlend { lerp(3) } else { 255 };
        }
    }
    let multiply = |c: i32, m: f32| i32::from(((c & 0xff) as f32 * m) as i32 as u8);
    [r, g, b] = [multiply(r, p.color_multiplier[0]), multiply(g, p.color_multiplier[1]), multiply(b, p.color_multiplier[2])];
    a &= 0xff;
    let fade_out = props.fade_out && props.fade_out_start_time < time && max_lifetime != props.fade_out_start_time;
    let fade_in = props.fade_in && time < props.fade_in_end_time && props.fade_in_end_time != 0.0;
    if fade_out || fade_in {
        let (amount, factor) = if props.fade_out && time > props.fade_out_start_time {
            ((time - props.fade_out_start_time) / (max_lifetime - props.fade_out_start_time), props.fade_out_factor)
        } else {
            ((props.fade_in_end_time - time) / props.fade_in_end_time, props.fade_in_factor)
        };
        let amount = amount * 255.0;
        let fade = |c: i32, factor: f32| {
            let v = c as f32 - factor * amount;
            if v < 0.0 { 0 } else if v >= 255.0 { 255 } else { v as i32 }
        };
        match props.draw_style {
            DrawStyle::Modulated => return [127, 127, 127, (255 - (factor[3] * amount) as i32) as u8],
            DrawStyle::AlphaBlend => a = fade(a, factor[3]),
            _ => [r, g, b, a] = [fade(r, factor[0]), fade(g, factor[1]), fade(b, factor[2]), fade(a, factor[3])],
        }
    }
    [r, g, b, a].map(|c| c as u8)
}

fn spin_units(p: &Particle) -> i32 {
    let turned = p.spins_per_second[0] * p.time;
    if p.spins_per_second[0] < 0.0 {
        0xffff - ((p.start_spin[0] - turned).round() as i32 & 0xffff)
    } else {
        (turned + p.start_spin[0]).round() as i32 & 0xffff
    }
}

/// Texture rectangle of subdivision `index`: column `index / V`, row `index % V` (unverified).
fn subdivision_uv(props: &EmitterProperties, index: i32, clamp: bool) -> [f32; 4] {
    let (u, v) = (props.texture_u_subdivisions, props.texture_v_subdivisions);
    let (du, dv) = (1.0 / u as f32, 1.0 / v as f32);
    let u0 = (index / v) as f32 * du;
    let v0 = (index % v) as f32 * dv;
    if clamp { [u0, v0, (u0 + du).min(1.0), (v0 + dv).min(1.0)] } else { [u0, v0, u0 + du, v0 + dv] }
}

pub(super) fn subdivision(props: &EmitterProperties, p: &Particle) -> ([f32; 4], Option<([f32; 4], u8)>) {
    let whole = [0.0, 0.0, 1.0, 1.0];
    let (u, v) = (props.texture_u_subdivisions, props.texture_v_subdivisions);
    if u == 0 || v == 0 {
        return (whole, None);
    }
    if p.subdivision != -1 {
        return (subdivision_uv(props, p.subdivision, true), None);
    }
    if p.max_lifetime == 0.0 {
        return (whole, None);
    }
    let rel = relative_time(p.time, p.max_lifetime);
    let mut count = u * v;
    let index = if props.use_subdivision_scale {
        let n = props.subdivision_scale.iter().position(|&s| s >= rel).unwrap_or(props.subdivision_scale.len());
        i32::try_from(n).unwrap_or(i32::MAX)
    } else if props.subdivision_end == 0 {
        (count as f32 * rel).floor() as i32
    } else {
        count = (props.subdivision_end - props.subdivision_start).max(1);
        (count as f32 * rel).floor() as i32 + props.subdivision_start
    };
    let index = index.clamp(0, (u * v - 1).max(0));
    let uv = subdivision_uv(props, index, true);
    if !props.blend_between_subdivisions {
        return (uv, None);
    }
    let next = if index + 1 == count { uv } else { subdivision_uv(props, index + 1, false) };
    let scale_len = i32::try_from(props.subdivision_scale.len()).unwrap_or(i32::MAX);
    let factor = if !props.use_subdivision_scale || scale_len <= count {
        fractional(count as f32 * rel)
    } else {
        let at = usize::try_from(index).unwrap_or(0);
        let prev = if at == 0 { 0.0 } else { props.subdivision_scale[at - 1] };
        let current = props.subdivision_scale.get(at).copied().unwrap_or(1.0).min(1.0);
        (rel - prev) / (current - prev)
    };
    (uv, Some((next, (factor * 255.0) as i32 as u8)))
}
