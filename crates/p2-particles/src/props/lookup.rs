use crate::math::{Rotator, Vector};
use super::{PropertyValue, DrawStyle, CoordinateSystem, StartLocationShape, RotationSource, VelocityDirection, DirectionUsage, ShapeExtend, BeamEnd, ColorKey, SizeKey, EmitterProperties};
use super::beam::{beam_end_point, beam_scales};

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
