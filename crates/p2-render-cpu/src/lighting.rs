use p2_math::{Rotator, Vector};

use p2_assets::{StaticMesh, StaticMeshInstance};
use p2_math::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightType(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightEffect(pub u8);

const LT_NONE: LightType = LightType(0);
const LT_STEADY: LightType = LightType(1);
const LT_PULSE: LightType = LightType(2);
const LT_BLINK: LightType = LightType(3);
const LT_FLICKER: LightType = LightType(4);
const LT_STROBE: LightType = LightType(5);
const LT_SUBTLE_PULSE: LightType = LightType(7);
const LT_TEXTURE_PALETTE_ONCE: LightType = LightType(8);
const LT_TEXTURE_PALETTE_LOOP: LightType = LightType(9);
pub const LE_STATIC_SPOT: LightEffect = LightEffect(8);
pub const LE_SPOTLIGHT: LightEffect = LightEffect(12);
const LE_NON_INCIDENCE: LightEffect = LightEffect(13);
const LE_CYLINDER: LightEffect = LightEffect(17);
pub const LE_SUNLIGHT: LightEffect = LightEffect(20);

pub const MAX_HARDWARE_LIGHTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshLight {
    pub location: Vector,
    pub rotation: Rotator,
    pub light_type: LightType,
    pub period: u8,
    pub phase: u8,
    pub intensity: f32,
    pub effect: LightEffect,
    pub brightness: f32,
    pub hue: u8,
    pub saturation: u8,
    pub radius: f32,
    pub cone: u8,
    pub dynamic: bool,
}

impl Default for MeshLight {
    fn default() -> Self {
        Self {
            location: Vector::ZERO,
            rotation: Rotator::ZERO,
            light_type: LT_STEADY,
            period: 32,
            phase: 0,
            intensity: 1.0,
            effect: LightEffect(0),
            brightness: 64.0,
            hue: 0,
            saturation: 255,
            radius: 64.0,
            cone: 128,
            dynamic: false,
        }
    }
}

pub fn hsv_color(h: u8, s: u8, v: u8) -> [f32; 3] {
    let brightness = f32::from(v) * 1.4 / 255.0;
    let brightness = (brightness * 0.7 / (0.01 + brightness.sqrt())).clamp(0.0, 1.0);
    hue_color(h, s).map(|c| c * brightness)
}

pub(crate) fn hue_color(hue: u8, saturation: u8) -> [f32; 3] {
    let h = f32::from(hue);
    let base = if hue < 86 {
        [(85.0 - h) / 85.0, h / 85.0, 0.0]
    } else if hue < 171 {
        [0.0, (170.0 - h) / 85.0, (h - 85.0) / 85.0]
    } else {
        [(h - 170.0) / 85.0, 0.0, (255.0 - h) / 84.0]
    };
    let s = f32::from(saturation) / 255.0;
    base.map(|c| c + s * (1.0 - c))
}

/// `x87`/SSE `cvttss2si`: truncation toward zero; out of range (or NaN) gives `0x80000000`.
fn truncate(x: f32) -> i32 {
    if (-2_147_483_648.0..2_147_483_648.0).contains(&x) { x as i32 } else { i32::MIN }
}

/// A 16384-step sine table read at (i >> 2) & 16383, so i is an angle of 65536 per turn. The table
/// holds sin(k · 2π / 16384) (unverified).
fn sin_tab(i: i32) -> f32 {
    let step = (i >> 2) & 0x3fff;
    (f64::from(step) * std::f64::consts::TAU / 16384.0).sin() as f32
}

/// In single precision as retail: trunc(time · 35 · 65536 / divisor + phase · 256).
fn pulse_angle(time: f32, divisor: i32, phase: u8) -> i32 {
    truncate(time * 35.0 * 65536.0 / divisor as f32 + (i32::from(phase) << 8) as f32)
}

/// Only the colour scales, never the radius. Flicker `rand` is one draw per set-up; strobe is 0 while
/// retail's shared toggle is set. Texture palette light types are not modelled: left steady.
pub fn light_type_scale(light_type: LightType, period: u8, phase: u8, time: f32, rand: f32, strobe: bool) -> f32 {
    let pulse = |amplitude: f32, base: f32| {
        let s = f64::from(sin_tab(pulse_angle(time, i32::from(period.max(1)), phase)));
        (s * f64::from(amplitude) + f64::from(base)) as f32
    };
    match light_type {
        LT_STEADY | LT_TEXTURE_PALETTE_ONCE | LT_TEXTURE_PALETTE_LOOP => 1.0,
        LT_PULSE => pulse(0.39, 0.6),
        LT_BLINK => {
            if pulse_angle(time, i32::from(period) + 1, phase) & 1 == 0 { 1.0 } else { 0.0 }
        }
        LT_FLICKER => {
            if 0.5 > rand { 0.0 } else { rand }
        }
        LT_STROBE => {
            if strobe { 0.0 } else { 1.0 }
        }
        LT_SUBTLE_PULSE => pulse(0.09, 0.9),
        _ => 0.0,
    }
}

#[derive(Debug, Clone)]
pub struct LightClock {
    rng: Rng,
    strobe_time: f32,
    strobe: bool,
}

impl Default for LightClock {
    fn default() -> Self {
        Self { rng: Rng::new(1), strobe_time: 0.0, strobe: false }
    }
}

impl LightClock {
    /// `light` at `time` with its [`light_type_scale`] in [`MeshLight::intensity`]. Retail sets a light up for each
    /// thing it lights each frame (a flicker light draws a new random number each time); here once per frame.
    pub fn animate(&mut self, light: MeshLight, time: f32) -> MeshLight {
        let rand = if light.light_type == LT_FLICKER { self.rng.frand() } else { 0.0 };
        if light.light_type == LT_STROBE && self.strobe_time != time {
            self.strobe_time = time;
            self.strobe = !self.strobe;
        }
        let intensity = light_type_scale(light.light_type, light.period, light.phase, time, rand, self.strobe);
        MeshLight { intensity, ..light }
    }
}

impl MeshLight {
    pub fn color(&self, level_brightness: f32) -> [f32; 3] {
        let intensity = if self.light_type == LT_NONE { 0.0 } else { self.intensity };
        hsv_color(self.hue, self.saturation, 255).map(|c| c * self.brightness / 255.0 * intensity * level_brightness)
    }

    pub fn world_radius(&self) -> f32 {
        light_world_radius(self.radius)
    }

    pub fn cone_sine(&self) -> f32 {
        1.0 - f32::from(self.cone) / 256.0
    }

    fn prepare(&self) -> PreparedLight<'_> {
        let direction = if matches!(self.effect, LE_SUNLIGHT | LE_SPOTLIGHT | LE_STATIC_SPOT) { self.rotation.vector() } else { Vector::ZERO };
        let sine = self.cone_sine();
        PreparedLight { light: self, radius: self.world_radius(), direction, sine, rsine: 1.0 / (1.0 - sine) }
    }

    pub fn sample_intensity(&self, position: Vector, normal: Vector) -> f32 {
        self.prepare().sample_intensity(position, normal)
    }
}

pub fn light_world_radius(radius: f32) -> f32 {
    25.0 * (radius as i32 + 1) as f32
}

/// What [`MeshLight::sample_intensity`] derives from the light alone, worked out once for many samples.
struct PreparedLight<'a> {
    light: &'a MeshLight,
    radius: f32,
    direction: Vector,
    sine: f32,
    rsine: f32,
}

impl PreparedLight<'_> {
    fn sample_intensity(&self, position: Vector, normal: Vector) -> f32 {
        let PreparedLight { light, radius, direction, sine, rsine } = *self;
        let to_light = light.location - position;
        let distance_squared = to_light.dot(to_light);
        if light.effect != LE_SUNLIGHT && distance_squared >= radius * radius {
            return 0.0;
        }
        let distance = distance_squared.sqrt();
        match light.effect {
            LE_SUNLIGHT => {
                let d = direction.dot(normal);
                if d < 0.0 { d * -2.0 } else { 0.0 }
            }
            LE_CYLINDER if distance < radius => {
                (1.0 - (to_light.x * to_light.x + to_light.y * to_light.y) / (radius * radius)).max(0.0) * 2.0
            }
            LE_NON_INCIDENCE if to_light.dot(normal) > 0.0 && distance < radius => (1.02 - distance / radius).sqrt() * 2.0,
            LE_CYLINDER | LE_NON_INCIDENCE => 0.0,
            LE_SPOTLIGHT | LE_STATIC_SPOT => {
                let base = attenuation(distance, radius, to_light, normal);
                if base <= 0.0 {
                    return 0.0;
                }
                let along = -to_light.dot(direction);
                if along > 0.0 && along * along > sine * sine * distance_squared {
                    let falloff = along * rsine / distance - sine * rsine;
                    falloff * falloff * base
                } else {
                    0.0
                }
            }
            _ => attenuation(distance, radius, to_light, normal),
        }
    }
}

fn attenuation(distance: f32, radius: f32, to_light: Vector, normal: Vector) -> f32 {
    let facing = to_light.dot(normal);
    if facing <= 0.0 || distance > radius {
        return 0.0;
    }
    let a = distance / radius;
    let b = 2.0 * a * a * a - 3.0 * a * a + 1.0;
    b / a * (facing / radius).abs() * 2.0
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshPlacement {
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    pub pre_pivot: Vector,
}

impl MeshPlacement {
    fn axes(&self) -> [Vector; 3] {
        let [x, y, z] = self.rotation.axes();
        [x * self.scale.x, y * self.scale.y, z * self.scale.z]
    }

    fn place(&self, [x, y, z]: [Vector; 3], local: Vector) -> Vector {
        let p = local - self.pre_pivot;
        self.location + x * p.x + y * p.y + z * p.z
    }

    pub fn world_vertices(&self, mesh: &StaticMesh) -> Vec<(Vector, Vector)> {
        let axes = self.axes();
        let [x, y, z] = axes;
        mesh.positions
            .iter()
            .zip(&mesh.normals)
            .map(|(&p, &n)| {
                let position = self.place(axes, Vector::from(p));
                let normal = x * n[0] + y * n[1] + z * n[2];
                let length = normal.dot(normal).sqrt();
                (position, if length > 1e-8 { normal / length } else { Vector::ZERO })
            })
            .collect()
    }

    pub fn transform_point(&self, local: Vector) -> Vector {
        self.place(self.axes(), local)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HardwareKind {
    Point,
    Spot,
    Directional,
}

/// A light as the D3D driver sets it up at `Modulate2X`: engine colour scaled ½ (point), 0.1 (spot) or
/// 0.875 (directional); point attenuation 1 / (a₀ + a₁·d) fitted to the engine's falloff.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HardwareLight {
    pub kind: HardwareKind,
    pub position: Vector,
    pub direction: Vector,
    pub color: [f32; 3],
    pub range: f32,
    pub attenuation: [f32; 2],
    pub dynamic: bool,
}

fn radial_falloff(distance: f32, radius: f32) -> f32 {
    if distance > radius {
        return 0.0;
    }
    let a = distance / radius;
    2.0 * (2.0 * a * a * a - 3.0 * a * a + 1.0)
}

impl HardwareLight {
    pub fn new(light: &MeshLight, level_brightness: f32, center: Vector, radius: f32) -> Option<Self> {
        if light.light_type == LT_NONE {
            return None;
        }
        let color = light.color(level_brightness);
        let (kind, scale) = match light.effect {
            LE_SUNLIGHT => (HardwareKind::Directional, 0.25 * 3.5),
            LE_SPOTLIGHT | LE_STATIC_SPOT => (HardwareKind::Spot, 0.1),
            _ => (HardwareKind::Point, 0.5),
        };
        let mut out = Self {
            kind,
            position: light.location,
            direction: light.rotation.vector(),
            color: color.map(|c| c * scale),
            range: light.world_radius(),
            attenuation: [1.0, 0.0],
            dynamic: light.dynamic,
        };
        if kind != HardwareKind::Directional {
            let to_center = center - light.location;
            let distance = to_center.dot(to_center).sqrt();
            let near = (distance - radius).max(0.0) + 1.0;
            let far = (distance + radius).min(out.range) - 1.0;
            let near_attenuation = 1.0 / radial_falloff(near, out.range);
            let far_attenuation = 1.0 / radial_falloff(far, out.range);
            let constant = if far > near {
                (near_attenuation - (far_attenuation - near_attenuation) / (far - near) * near).max(0.0)
            } else {
                near_attenuation
            };
            let linear = ((near_attenuation - constant) / near).max(0.0);
            if !(constant.is_finite() && linear.is_finite()) {
                return None;
            }
            out.attenuation = [constant, linear];
        }
        Some(out)
    }

    pub fn sample(&self, position: Vector, normal: Vector) -> [f32; 3] {
        let strength = match self.kind {
            HardwareKind::Directional => normal.dot(-self.direction).max(0.0),
            HardwareKind::Point | HardwareKind::Spot => {
                let to_light = self.position - position;
                let distance = to_light.dot(to_light).sqrt();
                if distance > self.range || distance < 1e-6 {
                    return [0.0; 3];
                }
                let to_light = to_light / distance;
                if self.kind == HardwareKind::Spot && -to_light.dot(self.direction) <= std::f32::consts::FRAC_1_SQRT_2 {
                    return [0.0; 3];
                }
                let [constant, linear] = self.attenuation;
                normal.dot(to_light).max(0.0) / (constant + linear * distance).max(1e-6)
            }
        };
        self.color.map(|c| c * strength)
    }
}

/// The lights the renderer gives an actor (unverified): those reaching its lit sphere, strongest
/// first, sun lights first, spots only with the sphere's centre in their cone, at most `max`.
pub fn relevant_lights<'a>(candidates: impl IntoIterator<Item = &'a MeshLight>, center: Vector, radius: f32, max: usize) -> Vec<MeshLight> {
    let mut keyed: Vec<(f32, MeshLight)> = candidates
        .into_iter()
        .filter(|light| light.light_type != LT_NONE)
        .filter_map(|light| {
            let to_center = center - light.location;
            let reach = light.world_radius() + radius;
            let key = match light.effect {
                LE_SUNLIGHT => f32::MAX,
                LE_SPOTLIGHT | LE_STATIC_SPOT => {
                    let length = to_center.dot(to_center).sqrt().max(1e-6);
                    let along = to_center.dot(light.rotation.vector()) / length;
                    // The list's limit is squared, so wider than `sample_intensity`'s cone (unverified).
                    let cone = light.cone_sine();
                    if along > 0.0 && along > cone * cone { (1.0 - to_center.dot(to_center) / (reach * reach)) * light.brightness } else { 0.0 }
                }
                _ => (1.0 - to_center.dot(to_center) / (reach * reach)) * light.brightness,
            };
            (key > 0.0).then_some((key, *light))
        })
        .collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    keyed.into_iter().take(max.min(MAX_HARDWARE_LIGHTS)).map(|(_, light)| light).collect()
}

/// Whether the engine recomputes the saved colours before drawing: a light whose "missing or dynamic"
/// state equals its `applied` flag. `lights` matches `instance.lights`; `None` = light actor gone.
pub fn needs_relight(instance: &StaticMeshInstance, lights: &[Option<MeshLight>]) -> bool {
    instance.lights.iter().zip(lights).any(|(info, light)| light.is_none_or(|l| l.dynamic) == info.applied)
}

pub fn static_mesh_lighting(
    mesh: &StaticMesh,
    instance: &StaticMeshInstance,
    lights: &[Option<MeshLight>],
    world: &[(Vector, Vector)],
    level_brightness: f32,
) -> Vec<[u8; 4]> {
    let mut colors = vec![[0u8; 4]; world.len()];
    for (info, light) in instance.lights.iter().zip(lights) {
        let Some(light) = light.filter(|l| !l.dynamic) else { continue };
        let color = light.color(level_brightness).map(|c| c * 0.5);
        let light = light.prepare();
        for (index, (&(position, normal), out)) in world.iter().zip(&mut colors).enumerate() {
            if !info.reaches(index) {
                continue;
            }
            let intensity = light.sample_intensity(position, normal);
            let add = [color[0], color[1], color[2], 1.0].map(|c| (c * intensity * 255.0).floor().clamp(0.0, 255.0) as u8);
            for (channel, add) in out.iter_mut().zip(add) {
                *channel = channel.saturating_add(add);
            }
        }
    }
    for (index, out) in colors.iter_mut().enumerate() {
        let source = mesh.colors.get(index).copied().unwrap_or([255; 4]);
        if mesh.use_vertex_color {
            for channel in 0..3 {
                out[channel] = (u16::from(source[channel]) * u16::from(out[channel]) / 255) as u8;
            }
        }
        out[3] = source[3];
    }
    colors
}

/// Zone ambient plus `AmbientGlow` / 255 on each channel. Glow 255 pulses (0.25 + 0.2 · sin(8t));
/// this gives its mean.
pub fn actor_ambient(zone: [f32; 3], glow: u8) -> [f32; 3] {
    let glow = if glow == 255 { 0.25 } else { f32::from(glow) / 255.0 };
    zone.map(|c| c + glow)
}

/// One colour per vertex and every light's visibility array (vertices + 7) / 8 bytes; the engine
/// ignores an instance that does not fit.
pub fn instance_fits(mesh: &StaticMesh, instance: &StaticMeshInstance) -> bool {
    let vertices = mesh.positions.len();
    instance.colors.len() == vertices && instance.lights.iter().all(|l| l.visibility.len() == vertices.div_ceil(8))
}

pub fn hardware_ambient(ambient: [f32; 3]) -> [f32; 3] {
    ambient.map(|c| (c * 255.0).floor().clamp(0.0, 255.0) * 0.5)
}

/// D3D vertex light of emissive + ambient + hardware lights, clamped, before `Modulate2X`. Without a
/// fitting instance the emissive part is black. The colour stream as diffuse is (unverified).
pub fn static_mesh_colors(
    mesh: &StaticMesh,
    instance: Option<&StaticMeshInstance>,
    lights: &[Option<MeshLight>],
    placement: &MeshPlacement,
    level_brightness: f32,
    ambient: [f32; 3],
    hardware: &[HardwareLight],
) -> Vec<[u8; 4]> {
    let fitting = instance.filter(|instance| instance_fits(mesh, instance));
    let relight = fitting.is_some_and(|instance| needs_relight(instance, lights));
    let applies = |light: &HardwareLight| fitting.is_none() || light.dynamic;
    let world = if relight || hardware.iter().any(applies) { placement.world_vertices(mesh) } else { Vec::new() };
    let mut colors = match fitting {
        Some(instance) if relight => static_mesh_lighting(mesh, instance, lights, &world, level_brightness),
        Some(instance) => instance.colors.clone(),
        None => vec![[0, 0, 0, 255]; mesh.positions.len()],
    };
    let ambient = hardware_ambient(ambient);
    for (index, color) in colors.iter_mut().enumerate() {
        let mut light = ambient;
        if let Some(&(position, normal)) = world.get(index) {
            for hardware in hardware.iter().filter(|light| applies(light)) {
                let [r, g, b] = hardware.sample(position, normal);
                light = [light[0] + r * 255.0, light[1] + g * 255.0, light[2] + b * 255.0];
            }
        }
        let tint = mesh.colors.get(index).filter(|_| mesh.use_vertex_color).copied().unwrap_or([255; 4]);
        for channel in 0..3 {
            let added = f32::from(color[channel]) + light[channel] * f32::from(tint[channel]) / 255.0;
            color[channel] = added.floor().clamp(0.0, 255.0) as u8;
        }
    }
    colors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_white_and_red() {
        let full = 1.4 * 0.7 / (0.01 + 1.4f32.sqrt());
        let white = hsv_color(0, 255, 255);
        assert!(white.iter().all(|&c| (c - full).abs() < 1e-6), "{white:?}");
        let red = hsv_color(0, 0, 255);
        assert!((red[0] - full).abs() < 1e-6 && red[1] == 0.0 && red[2] == 0.0, "{red:?}");
        assert_eq!(hsv_color(0, 255, 0), [0.0; 3]);
    }

    #[test]
    fn point_light_falloff() {
        let light = MeshLight { location: [0.0, 0.0, 100.0].into(), radius: 7.0, ..MeshLight::default() };
        assert_eq!(light.world_radius(), 200.0);
        let up = light.sample_intensity([0.0; 3].into(), [0.0, 0.0, 1.0].into());
        assert!((up - 1.0).abs() < 1e-5, "{up}");
        assert_eq!(light.sample_intensity([0.0; 3].into(), [0.0, 0.0, -1.0].into()), 0.0);
        assert_eq!(light.sample_intensity([0.0, 0.0, -150.0].into(), [0.0, 0.0, 1.0].into()), 0.0);
    }

    #[test]
    fn lighting_follows_visibility_bits() {
        let mesh = StaticMesh {
            physics_props: 0,
            sections: Vec::new(),
            positions: vec![[0.0; 3], [0.0; 3]],
            normals: vec![[0.0, 0.0, 1.0]; 2],
            colors: vec![[255, 255, 255, 128]; 2],
            use_vertex_color: false,
            uvs: Vec::new(),
            indices: Vec::new(),
            ..StaticMesh::default()
        };
        let light = MeshLight { location: [0.0, 0.0, 100.0].into(), radius: 7.0, brightness: 255.0, ..MeshLight::default() };
        let info = |actor| p2_assets::StaticMeshLight { actor, visibility: vec![0b01], applied: false };
        let instance = StaticMeshInstance { colors: vec![[0; 4]; 2], lights: vec![info(1), info(2)] };
        let placement = MeshPlacement { location: [0.0; 3].into(), rotation: [0; 3].into(), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into() };
        assert!(needs_relight(&instance, &[Some(light), None]));
        let colors = static_mesh_lighting(&mesh, &instance, &[Some(light), None], &placement.world_vertices(&mesh), 1.0);
        assert_eq!(colors, [[104, 104, 104, 128], [0, 0, 0, 128]]);
        let ambient = [0.2, 0.1, 0.0];
        let hardware = HardwareLight::new(&light, 1.0, [0.0; 3].into(), 50.0).unwrap();
        let drawn = static_mesh_colors(&mesh, Some(&instance), &[Some(light), None], &placement, 1.0, ambient, &[hardware]);
        assert_eq!(drawn, [[129, 116, 104, 128], [25, 12, 0, 128]]);
        let unfit = StaticMeshInstance { colors: vec![[0; 4]; 3], ..instance };
        let drawn = static_mesh_colors(&mesh, Some(&unfit), &[Some(light), None], &placement, 1.0, ambient, &[]);
        assert_eq!(drawn, [[25, 12, 0, 255]; 2]);
        assert_eq!(static_mesh_colors(&mesh, None, &[], &placement, 1.0, ambient, &[]), drawn);
        let lit = static_mesh_colors(&mesh, None, &[], &placement, 1.0, ambient, &[hardware]);
        let expected = (hardware.sample([0.0; 3].into(), [0.0, 0.0, 1.0].into())[0] * 255.0 + 25.5).floor();
        assert_eq!(f32::from(lit[0][0]), expected, "{lit:?}");
        assert!(lit[0][0] > 100, "{lit:?}");
    }

    #[test]
    fn hardware_lights_like_the_driver() {
        let light = MeshLight { location: [0.0, 0.0, 100.0].into(), radius: 7.0, brightness: 255.0, ..MeshLight::default() };
        let hardware = HardwareLight::new(&light, 1.0, [0.0; 3].into(), 50.0).unwrap();
        let full = hsv_color(0, 255, 255)[0];
        let near = hardware.sample([0.0, 0.0, 49.0].into(), [0.0, 0.0, 1.0].into())[0];
        assert!((near - 0.5 * full * radial_falloff(51.0, 200.0)).abs() < 1e-4, "{near}");
        let far = hardware.sample([0.0, 0.0, -40.0].into(), [0.0, 0.0, 1.0].into())[0];
        assert!(far > 0.0 && far < near, "{far} {near}");
        assert_eq!(hardware.sample([0.0, 0.0, -150.0].into(), [0.0, 0.0, 1.0].into()), [0.0; 3]);
        assert_eq!(hardware.sample([0.0; 3].into(), [0.0, 0.0, -1.0].into()), [0.0; 3]);
        let side = hardware.sample([0.0; 3].into(), [1.0, 0.0, 0.0].into());
        assert_eq!(side, [0.0; 3]);
        let spot = MeshLight { effect: LE_SPOTLIGHT, rotation: [-16384, 0, 0].into(), ..light };
        let spot = HardwareLight::new(&spot, 1.0, [0.0; 3].into(), 50.0).unwrap();
        let below = spot.sample([0.0; 3].into(), [0.0, 0.0, 1.0].into())[0];
        assert!((below / (0.1 * full) - hardware.sample([0.0; 3].into(), [0.0, 0.0, 1.0].into())[0] / (0.5 * full)).abs() < 1e-4, "{below}");
        assert_eq!(spot.sample([150.0, 0.0, 0.0].into(), [0.0, 0.0, 1.0].into()), [0.0; 3]);
        let sun = MeshLight { effect: LE_SUNLIGHT, rotation: [-16384, 0, 0].into(), ..light };
        let sun = HardwareLight::new(&sun, 1.0, [0.0; 3].into(), 50.0).unwrap();
        assert!((sun.sample([5000.0, 0.0, 0.0].into(), [0.0, 0.0, 1.0].into())[0] - 0.875 * full).abs() < 1e-4);
    }

    #[test]
    fn relevant_lights_strongest_first() {
        let at = |x: f32, brightness: f32| MeshLight { location: [x, 0.0, 0.0].into(), radius: 7.0, brightness, ..MeshLight::default() };
        let sun = MeshLight { effect: LE_SUNLIGHT, location: [1e6, 0.0, 0.0].into(), brightness: 10.0, ..MeshLight::default() };
        let candidates = [at(100.0, 64.0), at(200.0, 255.0), at(10.0, 255.0), at(400.0, 255.0), sun];
        let lights = relevant_lights(&candidates, [0.0; 3].into(), 50.0, 3);
        let xs: Vec<f32> = lights.iter().map(|l| l.location[0]).collect();
        assert_eq!(xs, [1e6, 10.0, 200.0]);
        assert_eq!(relevant_lights(&candidates, [0.0; 3].into(), 50.0, 20).len(), 4);
    }

    #[test]
    fn light_type_scales() {
        assert_eq!(light_type_scale(LT_STEADY, 32, 0, 12.3, 0.0, false), 1.0);
        assert_eq!(light_type_scale(LT_NONE, 32, 0, 0.0, 0.0, false), 0.0);
        assert_eq!(light_type_scale(LightType(6), 32, 0, 0.0, 0.0, false), 0.0);
        assert!((light_type_scale(LT_PULSE, 32, 0, 0.0, 0.0, false) - 0.6).abs() < 1e-6);
        assert!((light_type_scale(LT_SUBTLE_PULSE, 32, 0, 0.0, 0.0, false) - 0.9).abs() < 1e-6);
        assert!((light_type_scale(LT_PULSE, 32, 64, 0.0, 0.0, false) - 0.99).abs() < 1e-6);
        assert!((light_type_scale(LT_SUBTLE_PULSE, 32, 192, 0.0, 0.0, false) - 0.81).abs() < 1e-6);
        assert!((light_type_scale(LT_PULSE, 35, 0, 0.25, 0.0, false) - 0.99).abs() < 1e-5);
        assert_eq!(light_type_scale(LT_PULSE, 0, 0, 1.0 / 35.0, 0.0, false), light_type_scale(LT_PULSE, 1, 0, 1.0 / 35.0, 0.0, false));
        assert_eq!(light_type_scale(LT_BLINK, 0, 1, 0.0, 0.0, false), 1.0);
        assert_eq!(pulse_angle(1.0 / 65536.0, 1, 0), 35);
        assert_eq!(light_type_scale(LT_BLINK, 0, 0, 1.0 / 65536.0, 0.0, false), 0.0);
        assert_eq!(light_type_scale(LT_BLINK, 0, 0, 2.0 / 65536.0, 0.0, false), 1.0);
        assert_eq!(light_type_scale(LT_FLICKER, 32, 0, 0.0, 0.49, false), 0.0);
        assert_eq!(light_type_scale(LT_FLICKER, 32, 0, 0.0, 0.75, false), 0.75);
        assert_eq!(light_type_scale(LT_STROBE, 32, 0, 0.0, 0.0, true), 0.0);
        assert_eq!(light_type_scale(LT_STROBE, 32, 0, 0.0, 0.0, false), 1.0);
        assert_eq!(truncate(3e9), i32::MIN);
        assert_eq!(sin_tab(i32::MIN), 0.0);
    }

    #[test]
    fn clock_animates_lights() {
        let mut clock = LightClock::default();
        let strobe = MeshLight { light_type: LT_STROBE, brightness: 255.0, ..MeshLight::default() };
        assert_eq!(clock.animate(strobe, 0.1).intensity, 0.0);
        assert_eq!(clock.animate(strobe, 0.1).intensity, 0.0);
        assert_eq!(clock.animate(strobe, 0.2).intensity, 1.0);
        let steady = clock.animate(MeshLight::default(), 0.3);
        assert_eq!(steady.intensity, 1.0);
        let mut rng = Rng::new(1);
        let flicker = MeshLight { light_type: LT_FLICKER, ..MeshLight::default() };
        let expected = light_type_scale(LT_FLICKER, 32, 0, 0.0, rng.frand(), false);
        let lit = clock.animate(flicker, 0.3);
        assert_eq!(lit.intensity, expected);
        assert_eq!(lit.world_radius(), flicker.world_radius());
        let pulse = clock.animate(MeshLight { light_type: LT_PULSE, phase: 64, ..MeshLight::default() }, 0.0);
        let [r, ..] = pulse.color(1.0);
        assert!((r - MeshLight::default().color(1.0)[0] * 0.99).abs() < 1e-5, "{r}");
    }
}
