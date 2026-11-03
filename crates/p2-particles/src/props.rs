use crate::Rng;
use crate::math::{Rotator, Vector};

/// A property value as the caller's object model holds it.
///
/// Structs are positional, members in declaration order: `Vector` (X, Y, Z), `Plane` (X, Y, Z, W), `Rotator`
/// (Pitch, Yaw, Roll), `Range` (Min, Max), `RangeVector` (X, Y, Z ranges), `Color` (B, G, R, A),
/// `ParticleTimeScale` (`RelativeTime`, `RelativeSize`), `ParticleColorScale` (`RelativeTime`, `Color`).
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Bool(bool),
    Int(i32),
    Float(f32),
    Struct(Vec<Self>),
    Array(Vec<Self>),
}

impl PropertyValue {
    fn float(&self) -> Option<f32> {
        match *self {
            Self::Float(v) => Some(v),
            Self::Int(v) => Some(v as f32),
            _ => None,
        }
    }

    fn int(&self) -> Option<i32> {
        match *self {
            Self::Int(v) => Some(v),
            Self::Float(v) => Some(v as i32),
            Self::Bool(v) => Some(i32::from(v)),
            _ => None,
        }
    }

    fn bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(v) => Some(v),
            Self::Int(v) => Some(v != 0),
            _ => None,
        }
    }

    fn members(&self) -> &[Self] {
        match self {
            Self::Struct(members) | Self::Array(members) => members,
            _ => &[],
        }
    }

    fn floats<const N: usize>(&self) -> Option<[f32; N]> {
        let members = self.members();
        (members.len() >= N).then(|| std::array::from_fn(|i| members[i].float().unwrap_or(0.0)))
    }

    fn range(&self) -> Option<Range> {
        self.floats::<2>().map(|[min, max]| Range { min, max })
    }

    fn range_vector(&self) -> Option<RangeVector> {
        let members = self.members();
        let [x, y, z] = [0, 1, 2].map(|i| members.get(i).and_then(Self::range));
        Some(RangeVector { x: x?, y: y?, z: z? })
    }

    fn color(&self) -> Option<[u8; 4]> {
        let members = self.members();
        let byte = |i: usize| members.get(i).and_then(Self::int).map(|v| v as u8);
        Some([byte(2)?, byte(1)?, byte(0)?, byte(3)?])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Range {
    pub min: f32,
    pub max: f32,
}

impl Range {
    pub const fn new(min: f32, max: f32) -> Self {
        Self { min, max }
    }

    pub fn rand(&self, rng: &mut Rng) -> f32 {
        rng.frand() * (self.min - self.max) + self.max
    }

    pub fn center(&self) -> f32 {
        (self.min + self.max) * 0.5
    }
}

/// A draw takes Z, then Y, then X.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RangeVector {
    pub x: Range,
    pub y: Range,
    pub z: Range,
}

impl RangeVector {
    pub const fn splat(min: f32, max: f32) -> Self {
        let r = Range::new(min, max);
        Self { x: r, y: r, z: r }
    }

    pub fn rand(&self, rng: &mut Rng) -> Vector {
        let z = self.z.rand(rng);
        let y = self.y.rand(rng);
        let x = self.x.rand(rng);
        [x, y, z].into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrawStyle {
    Regular,
    AlphaBlend,
    Modulated,
    Translucent,
    AlphaModulate,
    Darken,
    Brighten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinateSystem {
    Independent,
    Relative,
    Absolute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartLocationShape {
    Box,
    Sphere,
    Polar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationSource {
    None,
    Actor,
    Offset,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocityDirection {
    None,
    StartPositionAndOwner,
    OwnerAndStartPosition,
    AddRadial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectionUsage {
    None,
    Up,
    Right,
    Forward,
    Normal,
    UpAndNormal,
    RightAndNormal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitterKind {
    #[default]
    Sprite,
    SuperSprite,
    Strip,
    Mesh,
    Beam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BeamEnd {
    #[default]
    Velocity,
    Distance,
    Offset,
    Actor,
    TraceOffset,
    OffsetAsAbsolute,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamEndPoint {
    pub offset: RangeVector,
    pub weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamScale {
    pub scale: Vector,
    pub relative_length: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BeamProperties {
    pub end_by: BeamEnd,
    pub distance_range: Range,
    pub end_points: Vec<BeamEndPoint>,
    pub u_scale: f32,
    pub v_scale: f32,
    pub rotating_sheets: i32,
    pub low_noise: RangeVector,
    pub low_points: i32,
    pub high_noise: RangeVector,
    pub high_points: i32,
    pub low_scales: Vec<BeamScale>,
    pub low_scale_repeats: f32,
    pub use_low_scale: bool,
    pub high_scales: Vec<BeamScale>,
    pub high_scale_repeats: f32,
    pub use_high_scale: bool,
    pub noise_determines_end_point: bool,
}

impl Default for BeamProperties {
    fn default() -> Self {
        let zero = RangeVector::splat(0.0, 0.0);
        Self {
            end_by: BeamEnd::Velocity,
            distance_range: Range::new(0.0, 0.0),
            end_points: Vec::new(),
            u_scale: 1.0,
            v_scale: 1.0,
            rotating_sheets: 0,
            low_noise: zero,
            low_points: 3,
            high_noise: zero,
            high_points: 10,
            low_scales: Vec::new(),
            low_scale_repeats: 0.0,
            use_low_scale: false,
            high_scales: Vec::new(),
            high_scale_repeats: 0.0,
            use_high_scale: false,
            noise_determines_end_point: false,
        }
    }
}

impl BeamProperties {
    pub fn sheets(&self) -> usize {
        usize::try_from(self.rotating_sheets).unwrap_or(0).max(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeExtend {
    None,
    Line,
    Circle,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorKey {
    pub time: f32,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeKey {
    pub time: f32,
    pub size: f32,
}

#[derive(Debug, Clone, PartialEq)]
#[expect(clippy::struct_excessive_bools, reason = "one field per script property of `ParticleEmitter`")]
pub struct EmitterProperties {
    pub acceleration: Vector,
    pub use_color_scale: bool,
    pub color_scale: Vec<ColorKey>,
    pub color_scale_repeats: f32,
    pub color_multiplier_range: RangeVector,
    pub fade_out_factor: [f32; 4],
    pub fade_out_start_time: f32,
    pub fade_out: bool,
    pub fade_in_factor: [f32; 4],
    pub fade_in_end_time: f32,
    pub fade_in: bool,
    pub coordinate_system: CoordinateSystem,
    pub max_particles: i32,
    pub effect_axis_positive_z: bool,
    pub respawn_dead_particles: bool,
    pub seconds_before_inactive: f32,
    pub use_actor_forces: bool,
    pub auto_destroy: bool,
    pub auto_reset: bool,
    pub disabled: bool,
    pub auto_reset_time_range: Range,
    pub start_location_offset: Vector,
    pub start_location_range: RangeVector,
    pub add_location_from_other_emitter: Option<usize>,
    pub start_location_shape: StartLocationShape,
    pub sphere_radius_range: Range,
    /// X = angle around Z (degrees), Y = angle from +Z (degrees), Z = radius.
    pub start_location_polar_range: RangeVector,
    pub start_mass_range: Range,
    pub use_rotation_from: RotationSource,
    pub spin_particles: bool,
    pub rotation_offset: Rotator,
    pub spin_ccw_or_cw: Vector,
    /// Turns per second.
    pub spins_per_second_range: RangeVector,
    /// Turns.
    pub start_spin_range: RangeVector,
    pub rotation_normal: Vector,
    pub use_size_scale: bool,
    pub use_regular_size_scale: bool,
    pub size_scale: Vec<SizeKey>,
    pub size_scale_repeats: f32,
    pub start_size_range: RangeVector,
    pub uniform_size: bool,
    pub particles_per_second: f32,
    pub initial_particles_per_second: f32,
    pub automatic_initial_spawning: bool,
    pub draw_style: DrawStyle,
    pub texture_u_subdivisions: i32,
    pub texture_v_subdivisions: i32,
    pub blend_between_subdivisions: bool,
    pub use_subdivision_scale: bool,
    pub subdivision_scale: Vec<f32>,
    pub subdivision_start: i32,
    pub subdivision_end: i32,
    pub use_random_subdivision: bool,
    pub initial_time_range: Range,
    pub lifetime_range: Range,
    pub initial_delay_range: Range,
    pub start_velocity_range: RangeVector,
    pub start_velocity_radial_range: Range,
    pub max_abs_velocity: Vector,
    pub velocity_loss_range: RangeVector,
    pub add_velocity_from_other_emitter: Option<usize>,
    pub add_velocity_multiplier_range: RangeVector,
    pub get_velocity_direction_from: VelocityDirection,
    pub warmup_ticks_per_second: f32,
    pub relative_warmup_time: f32,
    pub use_direction_as: DirectionUsage,
    pub projection_normal: Vector,
    /// A cache set when the emitter initialises; only the `SetProjectionNormal` natives change it, so a
    /// script writing `ProjectionNormal` directly changes nothing drawn. Zero until one of those ran.
    pub real_projection_normal: Vector,
    pub use_collision: bool,
    pub damping_factor_range: RangeVector,
    pub use_max_collisions: bool,
    pub max_collisions: Range,
    pub spawn_from_other_emitter: Option<usize>,
    pub spawn_amount: i32,
    pub use_spawned_velocity_scale: bool,
    pub spawned_velocity_scale_range: RangeVector,
    pub damp_rotation: bool,
    pub rotation_damping_factor_range: RangeVector,
    pub min_squared_velocity: f32,
    pub alpha_test: bool,
    pub alpha_ref: i32,
    pub disable_fogging: bool,
    pub kind: EmitterKind,
    pub location_shape_extend: ShapeExtend,
    pub line_start: Vector,
    pub line_end: Vector,
    pub allow_particle_spawn: bool,
    pub use_mesh_blend_mode: bool,
    pub render_two_sided: bool,
    pub beam: BeamProperties,
}

impl Default for EmitterProperties {
    fn default() -> Self {
        let zero = RangeVector::splat(0.0, 0.0);
        let one = RangeVector::splat(1.0, 1.0);
        Self {
            acceleration: Vector::ZERO,
            use_color_scale: false,
            color_scale: Vec::new(),
            color_scale_repeats: 0.0,
            color_multiplier_range: one,
            fade_out_factor: [1.0; 4],
            fade_out_start_time: 0.0,
            fade_out: false,
            fade_in_factor: [1.0; 4],
            fade_in_end_time: 0.0,
            fade_in: false,
            coordinate_system: CoordinateSystem::Independent,
            max_particles: 10,
            effect_axis_positive_z: false,
            respawn_dead_particles: true,
            seconds_before_inactive: 1.0,
            use_actor_forces: false,
            auto_destroy: false,
            auto_reset: false,
            disabled: false,
            auto_reset_time_range: Range::new(0.0, 0.0),
            start_location_offset: Vector::ZERO,
            start_location_range: zero,
            add_location_from_other_emitter: None,
            start_location_shape: StartLocationShape::Box,
            sphere_radius_range: Range::new(0.0, 0.0),
            start_location_polar_range: zero,
            start_mass_range: Range::new(1.0, 1.0),
            use_rotation_from: RotationSource::None,
            spin_particles: false,
            rotation_offset: Rotator::ZERO,
            spin_ccw_or_cw: [0.5; 3].into(),
            spins_per_second_range: zero,
            start_spin_range: zero,
            rotation_normal: Vector::ZERO,
            use_size_scale: false,
            use_regular_size_scale: true,
            size_scale: Vec::new(),
            size_scale_repeats: 0.0,
            start_size_range: RangeVector::splat(100.0, 100.0),
            uniform_size: false,
            particles_per_second: 0.0,
            initial_particles_per_second: 0.0,
            automatic_initial_spawning: true,
            draw_style: DrawStyle::Translucent,
            texture_u_subdivisions: 0,
            texture_v_subdivisions: 0,
            blend_between_subdivisions: false,
            use_subdivision_scale: false,
            subdivision_scale: Vec::new(),
            subdivision_start: 0,
            subdivision_end: 0,
            use_random_subdivision: false,
            initial_time_range: Range::new(0.0, 0.0),
            lifetime_range: Range::new(4.0, 4.0),
            initial_delay_range: Range::new(0.0, 0.0),
            start_velocity_range: zero,
            start_velocity_radial_range: Range::new(0.0, 0.0),
            max_abs_velocity: Vector::ZERO,
            velocity_loss_range: zero,
            add_velocity_from_other_emitter: None,
            add_velocity_multiplier_range: one,
            get_velocity_direction_from: VelocityDirection::None,
            warmup_ticks_per_second: 0.0,
            relative_warmup_time: 0.0,
            use_direction_as: DirectionUsage::None,
            projection_normal: [0.0, 0.0, 1.0].into(),
            real_projection_normal: Vector::ZERO,
            use_collision: false,
            damping_factor_range: one,
            use_max_collisions: false,
            max_collisions: Range::new(0.0, 0.0),
            spawn_from_other_emitter: None,
            spawn_amount: 0,
            use_spawned_velocity_scale: false,
            spawned_velocity_scale_range: zero,
            damp_rotation: false,
            rotation_damping_factor_range: zero,
            min_squared_velocity: 0.0,
            alpha_test: true,
            alpha_ref: 0,
            disable_fogging: false,
            kind: EmitterKind::Sprite,
            location_shape_extend: ShapeExtend::None,
            line_start: Vector::ZERO,
            line_end: Vector::ZERO,
            allow_particle_spawn: true,
            use_mesh_blend_mode: true,
            render_two_sided: false,
            beam: BeamProperties::default(),
        }
    }
}

macro_rules! read {
    ($lookup:ident, $name:literal, $field:expr, $convert:expr) => {
        if let Some(value) = $lookup($name).as_ref().and_then($convert) {
            $field = value;
        }
    };
}

impl EmitterProperties {
    #[expect(clippy::too_many_lines, reason = "a table: one `read!` per script property")]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<PropertyValue>) -> Self {
        let mut p = Self::default();
        let f = PropertyValue::float;
        let i = PropertyValue::int;
        let b = PropertyValue::bool;
        let v3 = |v: &PropertyValue| v.floats::<3>().map(Vector::from);
        let range = PropertyValue::range;
        let rv = PropertyValue::range_vector;
        let rotator = |v: &PropertyValue| v.floats::<3>().map(|r| Rotator::from(r.map(|c| c as i32)));
        let other_emitter = |v: &PropertyValue| v.int().map(|index| usize::try_from(index).ok());
        read!(lookup, "Acceleration", p.acceleration, v3);
        read!(lookup, "UseColorScale", p.use_color_scale, b);
        read!(lookup, "ColorScale", p.color_scale, |v: &PropertyValue| {
            let key = |key: &PropertyValue| {
                let (time, color) = key_pair(key, PropertyValue::color, [255; 4]);
                ColorKey { time, color }
            };
            Some(v.members().iter().map(key).collect())
        });
        read!(lookup, "ColorScaleRepeats", p.color_scale_repeats, f);
        read!(lookup, "ColorMultiplierRange", p.color_multiplier_range, rv);
        read!(lookup, "FadeOutFactor", p.fade_out_factor, PropertyValue::floats::<4>);
        read!(lookup, "FadeOutStartTime", p.fade_out_start_time, f);
        read!(lookup, "FadeOut", p.fade_out, b);
        read!(lookup, "FadeInFactor", p.fade_in_factor, PropertyValue::floats::<4>);
        read!(lookup, "FadeInEndTime", p.fade_in_end_time, f);
        read!(lookup, "FadeIn", p.fade_in, b);
        read!(lookup, "CoordinateSystem", p.coordinate_system, |v: &PropertyValue| {
            Some(match v.int()? {
                1 => CoordinateSystem::Relative,
                2 => CoordinateSystem::Absolute,
                _ => CoordinateSystem::Independent,
            })
        });
        read!(lookup, "MaxParticles", p.max_particles, i);
        read!(lookup, "EffectAxis", p.effect_axis_positive_z, |v: &PropertyValue| Some(v.int()? == 1));
        read!(lookup, "RespawnDeadParticles", p.respawn_dead_particles, b);
        read!(lookup, "SecondsBeforeInactive", p.seconds_before_inactive, f);
        read!(lookup, "UseActorForces", p.use_actor_forces, b);
        read!(lookup, "AutoDestroy", p.auto_destroy, b);
        read!(lookup, "AutoReset", p.auto_reset, b);
        read!(lookup, "Disabled", p.disabled, b);
        read!(lookup, "AutoResetTimeRange", p.auto_reset_time_range, range);
        read!(lookup, "StartLocationOffset", p.start_location_offset, v3);
        read!(lookup, "StartLocationRange", p.start_location_range, rv);
        read!(lookup, "AddLocationFromOtherEmitter", p.add_location_from_other_emitter, other_emitter);
        read!(lookup, "StartLocationShape", p.start_location_shape, |v: &PropertyValue| {
            Some(match v.int()? {
                1 => StartLocationShape::Sphere,
                2 => StartLocationShape::Polar,
                _ => StartLocationShape::Box,
            })
        });
        read!(lookup, "SphereRadiusRange", p.sphere_radius_range, range);
        read!(lookup, "StartLocationPolarRange", p.start_location_polar_range, rv);
        read!(lookup, "StartMassRange", p.start_mass_range, range);
        read!(lookup, "UseRotationFrom", p.use_rotation_from, |v: &PropertyValue| {
            Some(match v.int()? {
                1 => RotationSource::Actor,
                2 => RotationSource::Offset,
                3 => RotationSource::Normal,
                _ => RotationSource::None,
            })
        });
        read!(lookup, "SpinParticles", p.spin_particles, b);
        read!(lookup, "RotationOffset", p.rotation_offset, rotator);
        read!(lookup, "SpinCCWorCW", p.spin_ccw_or_cw, v3);
        read!(lookup, "SpinsPerSecondRange", p.spins_per_second_range, rv);
        read!(lookup, "StartSpinRange", p.start_spin_range, rv);
        read!(lookup, "RotationNormal", p.rotation_normal, v3);
        read!(lookup, "UseSizeScale", p.use_size_scale, b);
        read!(lookup, "UseRegularSizeScale", p.use_regular_size_scale, b);
        read!(lookup, "SizeScale", p.size_scale, |v: &PropertyValue| {
            let key = |key: &PropertyValue| {
                let (time, size) = key_pair(key, PropertyValue::float, 0.0);
                SizeKey { time, size }
            };
            Some(v.members().iter().map(key).collect())
        });
        read!(lookup, "SizeScaleRepeats", p.size_scale_repeats, f);
        read!(lookup, "StartSizeRange", p.start_size_range, rv);
        read!(lookup, "UniformSize", p.uniform_size, b);
        read!(lookup, "ParticlesPerSecond", p.particles_per_second, f);
        read!(lookup, "InitialParticlesPerSecond", p.initial_particles_per_second, f);
        read!(lookup, "AutomaticInitialSpawning", p.automatic_initial_spawning, b);
        read!(lookup, "DrawStyle", p.draw_style, |v: &PropertyValue| {
            Some(match v.int()? {
                0 => DrawStyle::Regular,
                1 => DrawStyle::AlphaBlend,
                2 => DrawStyle::Modulated,
                4 => DrawStyle::AlphaModulate,
                5 => DrawStyle::Darken,
                6 => DrawStyle::Brighten,
                _ => DrawStyle::Translucent,
            })
        });
        read!(lookup, "TextureUSubdivisions", p.texture_u_subdivisions, i);
        read!(lookup, "TextureVSubdivisions", p.texture_v_subdivisions, i);
        read!(lookup, "BlendBetweenSubdivisions", p.blend_between_subdivisions, b);
        read!(lookup, "UseSubdivisionScale", p.use_subdivision_scale, b);
        read!(lookup, "SubdivisionScale", p.subdivision_scale, |v: &PropertyValue| {
            Some(v.members().iter().map(|s| s.float().unwrap_or(0.0)).collect())
        });
        read!(lookup, "SubdivisionStart", p.subdivision_start, i);
        read!(lookup, "SubdivisionEnd", p.subdivision_end, i);
        read!(lookup, "UseRandomSubdivision", p.use_random_subdivision, b);
        read!(lookup, "InitialTimeRange", p.initial_time_range, range);
        read!(lookup, "LifetimeRange", p.lifetime_range, range);
        read!(lookup, "InitialDelayRange", p.initial_delay_range, range);
        read!(lookup, "StartVelocityRange", p.start_velocity_range, rv);
        read!(lookup, "StartVelocityRadialRange", p.start_velocity_radial_range, range);
        read!(lookup, "MaxAbsVelocity", p.max_abs_velocity, v3);
        read!(lookup, "VelocityLossRange", p.velocity_loss_range, rv);
        read!(lookup, "AddVelocityFromOtherEmitter", p.add_velocity_from_other_emitter, other_emitter);
        read!(lookup, "AddVelocityMultiplierRange", p.add_velocity_multiplier_range, rv);
        read!(lookup, "GetVelocityDirectionFrom", p.get_velocity_direction_from, |v: &PropertyValue| {
            Some(match v.int()? {
                1 => VelocityDirection::StartPositionAndOwner,
                2 => VelocityDirection::OwnerAndStartPosition,
                3 => VelocityDirection::AddRadial,
                _ => VelocityDirection::None,
            })
        });
        read!(lookup, "WarmupTicksPerSecond", p.warmup_ticks_per_second, f);
        read!(lookup, "RelativeWarmupTime", p.relative_warmup_time, f);
        read!(lookup, "UseDirectionAs", p.use_direction_as, |v: &PropertyValue| {
            Some(match v.int()? {
                1 => DirectionUsage::Up,
                2 => DirectionUsage::Right,
                3 => DirectionUsage::Forward,
                4 => DirectionUsage::Normal,
                5 => DirectionUsage::UpAndNormal,
                6 => DirectionUsage::RightAndNormal,
                _ => DirectionUsage::None,
            })
        });
        read!(lookup, "ProjectionNormal", p.projection_normal, v3);
        read!(lookup, "RealProjectionNormal", p.real_projection_normal, v3);
        read!(lookup, "UseCollision", p.use_collision, b);
        read!(lookup, "DampingFactorRange", p.damping_factor_range, rv);
        read!(lookup, "UseMaxCollisions", p.use_max_collisions, b);
        read!(lookup, "MaxCollisions", p.max_collisions, range);
        read!(lookup, "SpawnFromOtherEmitter", p.spawn_from_other_emitter, other_emitter);
        read!(lookup, "SpawnAmount", p.spawn_amount, i);
        read!(lookup, "UseSpawnedVelocityScale", p.use_spawned_velocity_scale, b);
        read!(lookup, "SpawnedVelocityScaleRange", p.spawned_velocity_scale_range, rv);
        read!(lookup, "DampRotation", p.damp_rotation, b);
        read!(lookup, "RotationDampingFactorRange", p.rotation_damping_factor_range, rv);
        read!(lookup, "MinSquaredVelocity", p.min_squared_velocity, f);
        read!(lookup, "AlphaTest", p.alpha_test, b);
        read!(lookup, "AlphaRef", p.alpha_ref, i);
        read!(lookup, "DisableFogging", p.disable_fogging, b);
        read!(lookup, "LocationShapeExtend", p.location_shape_extend, |v: &PropertyValue| {
            Some(match v.int()? {
                0 => ShapeExtend::None,
                1 => ShapeExtend::Line,
                2 => ShapeExtend::Circle,
                _ => ShapeExtend::Other,
            })
        });
        read!(lookup, "LineStart", p.line_start, v3);
        read!(lookup, "LineEnd", p.line_end, v3);
        read!(lookup, "AllowParticleSpawn", p.allow_particle_spawn, b);
        read!(lookup, "UseMeshBlendMode", p.use_mesh_blend_mode, b);
        read!(lookup, "RenderTwoSided", p.render_two_sided, b);
        read!(lookup, "DetermineEndPointBy", p.beam.end_by, |v: &PropertyValue| {
            Some(match v.int()? {
                1 => BeamEnd::Distance,
                2 => BeamEnd::Offset,
                3 => BeamEnd::Actor,
                4 => BeamEnd::TraceOffset,
                5 => BeamEnd::OffsetAsAbsolute,
                _ => BeamEnd::Velocity,
            })
        });
        read!(lookup, "BeamDistanceRange", p.beam.distance_range, range);
        read!(lookup, "BeamEndPoints", p.beam.end_points, |v: &PropertyValue| {
            Some(v.members().iter().filter_map(beam_end_point).collect())
        });
        read!(lookup, "BeamTextureUScale", p.beam.u_scale, f);
        read!(lookup, "BeamTextureVScale", p.beam.v_scale, f);
        read!(lookup, "RotatingSheets", p.beam.rotating_sheets, i);
        read!(lookup, "LowFrequencyNoiseRange", p.beam.low_noise, rv);
        read!(lookup, "LowFrequencyPoints", p.beam.low_points, i);
        read!(lookup, "HighFrequencyNoiseRange", p.beam.high_noise, rv);
        read!(lookup, "HighFrequencyPoints", p.beam.high_points, i);
        read!(lookup, "LFScaleFactors", p.beam.low_scales, beam_scales);
        read!(lookup, "LFScaleRepeats", p.beam.low_scale_repeats, f);
        read!(lookup, "UseLowFrequencyScale", p.beam.use_low_scale, b);
        read!(lookup, "HFScaleFactors", p.beam.high_scales, beam_scales);
        read!(lookup, "HFScaleRepeats", p.beam.high_scale_repeats, f);
        read!(lookup, "UseHighFrequencyScale", p.beam.use_high_scale, b);
        read!(lookup, "NoiseDeterminesEndPoint", p.beam.noise_determines_end_point, b);
        p
    }
}

fn key_pair<T>(key: &PropertyValue, value: impl Fn(&PropertyValue) -> Option<T>, default: T) -> (f32, T) {
    let members = key.members();
    let time = members.first().and_then(PropertyValue::float).unwrap_or(0.0);
    (time, members.get(1).and_then(value).unwrap_or(default))
}

fn beam_end_point(point: &PropertyValue) -> Option<BeamEndPoint> {
    let members = point.members();
    let offset = members.get(1).and_then(PropertyValue::range_vector)?;
    Some(BeamEndPoint { offset, weight: members.get(2).and_then(PropertyValue::float).unwrap_or(0.0) })
}

#[expect(clippy::unnecessary_wraps, reason = "the `read!` macro takes an `Option` from every reader")]
fn beam_scales(keys: &PropertyValue) -> Option<Vec<BeamScale>> {
    let key = |key: &PropertyValue| {
        let members = key.members();
        let scale = members.first().and_then(PropertyValue::floats::<3>).map_or(Vector::ZERO, Vector::from);
        BeamScale { scale, relative_length: members.get(1).and_then(PropertyValue::float).unwrap_or(0.0) }
    };
    Some(keys.members().iter().map(key).collect())
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmitterSettings {
    pub auto_destroy: bool,
    pub auto_reset: bool,
    pub global_offset_range: RangeVector,
    pub time_till_reset_range: Range,
    pub disable_fogging: bool,
}

impl EmitterSettings {
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<PropertyValue>) -> Self {
        let mut s = Self::default();
        read!(lookup, "AutoDestroy", s.auto_destroy, PropertyValue::bool);
        read!(lookup, "AutoReset", s.auto_reset, PropertyValue::bool);
        read!(lookup, "GlobalOffsetRange", s.global_offset_range, PropertyValue::range_vector);
        read!(lookup, "TimeTillResetRange", s.time_till_reset_range, PropertyValue::range);
        read!(lookup, "DisableFogging", s.disable_fogging, PropertyValue::bool);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_overrides_defaults() {
        let lookup = |name: &str| match name {
            "MaxParticles" => Some(PropertyValue::Int(30)),
            "StartSizeRange" => Some(PropertyValue::Struct(
                [(5.0, 10.0), (6.0, 11.0), (7.0, 12.0)]
                    .map(|(min, max)| PropertyValue::Struct(vec![PropertyValue::Float(min), PropertyValue::Float(max)]))
                    .to_vec(),
            )),
            "ColorScale" => Some(PropertyValue::Array(vec![PropertyValue::Struct(vec![
                PropertyValue::Float(0.5),
                PropertyValue::Struct([10, 20, 30, 40].map(PropertyValue::Int).to_vec()),
            ])])),
            "DrawStyle" => Some(PropertyValue::Int(1)),
            _ => None,
        };
        let p = EmitterProperties::from_lookup(lookup);
        assert_eq!(p.max_particles, 30);
        assert_eq!(p.start_size_range.y, Range::new(6.0, 11.0));
        assert_eq!(p.color_scale, vec![ColorKey { time: 0.5, color: [30, 20, 10, 40] }]);
        assert_eq!(p.draw_style, DrawStyle::AlphaBlend);
        assert_eq!(p.lifetime_range, Range::new(4.0, 4.0));
    }
}
