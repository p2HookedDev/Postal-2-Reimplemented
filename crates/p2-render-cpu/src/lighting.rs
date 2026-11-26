use p2_math::{Rotator, Vector};

use p2_assets::{StaticMesh, StaticMeshInstance};
use p2_math::Rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LightType {
    None = 0,
    Steady = 1,
    Pulse = 2,
    Blink = 3,
    Flicker = 4,
    Strobe = 5,
    BackdropLight = 6,
    SubtlePulse = 7,
    TexturePaletteOnce = 8,
    TexturePaletteLoop = 9,
}

impl LightType {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Steady,
            2 => Self::Pulse,
            3 => Self::Blink,
            4 => Self::Flicker,
            5 => Self::Strobe,
            6 => Self::BackdropLight,
            7 => Self::SubtlePulse,
            8 => Self::TexturePaletteOnce,
            9 => Self::TexturePaletteLoop,
            _ => Self::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LightEffect {
    None = 0,
    TorchWaver = 1,
    FireWaver = 2,
    WateryShimmer = 3,
    Searchlight = 4,
    SlowWave = 5,
    FastWave = 6,
    CloudCast = 7,
    StaticSpot = 8,
    Shock = 9,
    Disco = 10,
    Warp = 11,
    Spotlight = 12,
    NonIncidence = 13,
    Shell = 14,
    OmniBumpMap = 15,
    Interference = 16,
    Cylinder = 17,
    Rotor = 18,
    Unused = 19,
    Sunlight = 20,
}

impl LightEffect {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::TorchWaver,
            2 => Self::FireWaver,
            3 => Self::WateryShimmer,
            4 => Self::Searchlight,
            5 => Self::SlowWave,
            6 => Self::FastWave,
            7 => Self::CloudCast,
            8 => Self::StaticSpot,
            9 => Self::Shock,
            10 => Self::Disco,
            11 => Self::Warp,
            12 => Self::Spotlight,
            13 => Self::NonIncidence,
            14 => Self::Shell,
            15 => Self::OmniBumpMap,
            16 => Self::Interference,
            17 => Self::Cylinder,
            18 => Self::Rotor,
            19 => Self::Unused,
            20 => Self::Sunlight,
            _ => Self::None,
        }
    }
}

pub const MAX_HARDWARE_LIGHTS: usize = 8;
pub const SUN_DISTANCE: f32 = 65536.0;

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
    pub special_lit: bool,
    pub corona: f32,
}

impl Default for MeshLight {
    fn default() -> Self {
        Self {
            location: Vector::ZERO,
            rotation: Rotator::ZERO,
            light_type: LightType::Steady,
            period: 32,
            phase: 0,
            intensity: 1.0,
            effect: LightEffect::None,
            brightness: 64.0,
            hue: 0,
            saturation: 255,
            radius: 64.0,
            cone: 128,
            dynamic: false,
            special_lit: false,
            corona: 0.0,
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

pub fn light_type_scale(light_type: LightType, period: u8, phase: u8, time: f32, rand: f32, strobe: bool) -> f32 {
    let pulse = |amplitude: f32, base: f32| {
        let s = f64::from(sin_tab(pulse_angle(time, i32::from(period.max(1)), phase)));
        (s * f64::from(amplitude) + f64::from(base)) as f32
    };
    match light_type {
        LightType::Steady | LightType::TexturePaletteOnce | LightType::TexturePaletteLoop => 1.0,
        LightType::Pulse => pulse(0.39, 0.6),
        LightType::Blink => {
            if pulse_angle(time, i32::from(period) + 1, phase) & 1 == 0 { 1.0 } else { 0.0 }
        }
        LightType::Flicker => {
            if 0.5 > rand { 0.0 } else { rand }
        }
        LightType::Strobe => {
            if strobe { 0.0 } else { 1.0 }
        }
        LightType::SubtlePulse => pulse(0.09, 0.9),
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
    pub fn animate(&mut self, light: MeshLight, time: f32) -> MeshLight {
        let rand = if light.light_type == LightType::Flicker { self.rng.frand() } else { 0.0 };
        if light.light_type == LightType::Strobe && self.strobe_time != time {
            self.strobe_time = time;
            self.strobe = !self.strobe;
        }
        let intensity = light_type_scale(light.light_type, light.period, light.phase, time, rand, self.strobe);
        MeshLight { intensity, ..light }
    }
}

impl MeshLight {
    pub fn color(&self, level_brightness: f32) -> [f32; 3] {
        let intensity = if self.light_type == LightType::None { 0.0 } else { self.intensity };
        hsv_color(self.hue, self.saturation, 255).map(|c| c * self.brightness / 255.0 * intensity * level_brightness)
    }

    pub fn world_radius(&self) -> f32 {
        light_world_radius(self.radius)
    }

    pub fn cone_sine(&self) -> f32 {
        1.0 - f32::from(self.cone) / 256.0
    }

    fn prepare(&self) -> PreparedLight<'_> {
        let direction = if matches!(self.effect, LightEffect::Sunlight | LightEffect::Spotlight | LightEffect::StaticSpot) { self.rotation.vector() } else { Vector::ZERO };
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
        if light.effect != LightEffect::Sunlight && distance_squared >= radius * radius {
            return 0.0;
        }
        let distance = distance_squared.sqrt();
        match light.effect {
            LightEffect::Sunlight => {
                let d = direction.dot(normal);
                if d < 0.0 { d * -2.0 } else { 0.0 }
            }
            LightEffect::Cylinder if distance < radius => {
                (1.0 - (to_light.x * to_light.x + to_light.y * to_light.y) / (radius * radius)).max(0.0) * 2.0
            }
            LightEffect::NonIncidence if to_light.dot(normal) > 0.0 && distance < radius => (1.02 - distance / radius).sqrt() * 2.0,
            LightEffect::Cylinder | LightEffect::NonIncidence => 0.0,
            LightEffect::Spotlight | LightEffect::StaticSpot => {
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

/// As the D3D driver sets it up at `Modulate2X`: colour scaled ½ (point), 0.1 (spot) or 0.875 (directional).
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
        if light.light_type == LightType::None {
            return None;
        }
        let color = light.color(level_brightness);
        let (kind, scale) = match light.effect {
            LightEffect::Sunlight => (HardwareKind::Directional, 0.25 * 3.5),
            LightEffect::Spotlight | LightEffect::StaticSpot => (HardwareKind::Spot, 0.1),
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
                out[channel] = p2_math::mul_byte_floor(source[channel], out[channel]);
            }
        }
        out[3] = source[3];
    }
    colors
}

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
        let spot = MeshLight { effect: LightEffect::Spotlight, rotation: [-16384, 0, 0].into(), ..light };
        let spot = HardwareLight::new(&spot, 1.0, [0.0; 3].into(), 50.0).unwrap();
        let below = spot.sample([0.0; 3].into(), [0.0, 0.0, 1.0].into())[0];
        assert!((below / (0.1 * full) - hardware.sample([0.0; 3].into(), [0.0, 0.0, 1.0].into())[0] / (0.5 * full)).abs() < 1e-4, "{below}");
        assert_eq!(spot.sample([150.0, 0.0, 0.0].into(), [0.0, 0.0, 1.0].into()), [0.0; 3]);
        let sun = MeshLight { effect: LightEffect::Sunlight, rotation: [-16384, 0, 0].into(), ..light };
        let sun = HardwareLight::new(&sun, 1.0, [0.0; 3].into(), 50.0).unwrap();
        assert!((sun.sample([5000.0, 0.0, 0.0].into(), [0.0, 0.0, 1.0].into())[0] - 0.875 * full).abs() < 1e-4);
    }

    #[test]
    fn light_type_scales() {
        assert_eq!(light_type_scale(LightType::Steady, 32, 0, 12.3, 0.0, false), 1.0);
        assert_eq!(light_type_scale(LightType::None, 32, 0, 0.0, 0.0, false), 0.0);
        assert_eq!(light_type_scale(LightType::BackdropLight, 32, 0, 0.0, 0.0, false), 0.0);
        assert!((light_type_scale(LightType::Pulse, 32, 0, 0.0, 0.0, false) - 0.6).abs() < 1e-6);
        assert!((light_type_scale(LightType::SubtlePulse, 32, 0, 0.0, 0.0, false) - 0.9).abs() < 1e-6);
        assert!((light_type_scale(LightType::Pulse, 32, 64, 0.0, 0.0, false) - 0.99).abs() < 1e-6);
        assert!((light_type_scale(LightType::SubtlePulse, 32, 192, 0.0, 0.0, false) - 0.81).abs() < 1e-6);
        assert!((light_type_scale(LightType::Pulse, 35, 0, 0.25, 0.0, false) - 0.99).abs() < 1e-5);
        assert_eq!(light_type_scale(LightType::Pulse, 0, 0, 1.0 / 35.0, 0.0, false), light_type_scale(LightType::Pulse, 1, 0, 1.0 / 35.0, 0.0, false));
        assert_eq!(light_type_scale(LightType::Blink, 0, 1, 0.0, 0.0, false), 1.0);
        assert_eq!(pulse_angle(1.0 / 65536.0, 1, 0), 35);
        assert_eq!(light_type_scale(LightType::Blink, 0, 0, 1.0 / 65536.0, 0.0, false), 0.0);
        assert_eq!(light_type_scale(LightType::Blink, 0, 0, 2.0 / 65536.0, 0.0, false), 1.0);
        assert_eq!(light_type_scale(LightType::Flicker, 32, 0, 0.0, 0.49, false), 0.0);
        assert_eq!(light_type_scale(LightType::Flicker, 32, 0, 0.0, 0.75, false), 0.75);
        assert_eq!(light_type_scale(LightType::Strobe, 32, 0, 0.0, 0.0, true), 0.0);
        assert_eq!(light_type_scale(LightType::Strobe, 32, 0, 0.0, 0.0, false), 1.0);
        assert_eq!(truncate(3e9), i32::MIN);
        assert_eq!(sin_tab(i32::MIN), 0.0);
    }

    #[test]
    fn clock_animates_lights() {
        let mut clock = LightClock::default();
        let strobe = MeshLight { light_type: LightType::Strobe, brightness: 255.0, ..MeshLight::default() };
        assert_eq!(clock.animate(strobe, 0.1).intensity, 0.0);
        assert_eq!(clock.animate(strobe, 0.1).intensity, 0.0);
        assert_eq!(clock.animate(strobe, 0.2).intensity, 1.0);
        let steady = clock.animate(MeshLight::default(), 0.3);
        assert_eq!(steady.intensity, 1.0);
        let mut rng = Rng::new(1);
        let flicker = MeshLight { light_type: LightType::Flicker, ..MeshLight::default() };
        let expected = light_type_scale(LightType::Flicker, 32, 0, 0.0, rng.frand(), false);
        let lit = clock.animate(flicker, 0.3);
        assert_eq!(lit.intensity, expected);
        assert_eq!(lit.world_radius(), flicker.world_radius());
        let pulse = clock.animate(MeshLight { light_type: LightType::Pulse, phase: 64, ..MeshLight::default() }, 0.0);
        let [r, ..] = pulse.color(1.0);
        assert!((r - MeshLight::default().color(1.0)[0] * 0.99).abs() < 1e-5, "{r}");
    }
}
