use super::Emitter;
use crate::Rng;
use crate::math::{Vector, rotate_by, vrand};
use crate::props::{CoordinateSystem, EmitterKind, EmitterProperties, ShapeExtend, StartLocationShape};

/// The full turn of the circle start shape: 6.28, as retail.
#[expect(clippy::approx_constant, reason = "BaseFX.dll multiplies by 6.28, not 2 pi")]
const CIRCLE_TURN: f32 = 6.28;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripPoint {
    pub location: Vector,
    pub size: f32,
    pub color: [u8; 4],
}

pub(super) fn spawns(props: &EmitterProperties) -> bool {
    props.kind != EmitterKind::SuperSprite || props.allow_particle_spawn
}

pub(super) fn strip_spawns(props: &EmitterProperties) -> bool {
    props.allow_particle_spawn && props.max_particles != 0
}

/// The start location (before rotation) where the class replaces the stock shapes, else `None`.
/// The draw order follows retail; a strip's polar shape counts as box (unverified).
pub(super) fn start_location(props: &EmitterProperties, rng: &mut Rng) -> Option<Vector> {
    let offset = props.start_location_offset;
    match (props.kind, props.location_shape_extend) {
        (EmitterKind::SuperSprite, ShapeExtend::Line) => {
            let range = props.start_location_range.rand(rng);
            let along = (props.line_end - props.line_start) * rng.frand();
            Some((offset + range) + along)
        }
        (EmitterKind::SuperSprite, ShapeExtend::Circle) => {
            let angle = rng.frand() * CIRCLE_TURN;
            let (r1, r2) = (props.sphere_radius_range.rand(rng), props.sphere_radius_range.rand(rng));
            let z = props.start_location_range.z.rand(rng);
            Some(offset + [angle.cos() * r1, angle.sin() * r2, z].into())
        }
        (EmitterKind::SuperSprite, ShapeExtend::Other) => Some(offset),
        (EmitterKind::Strip, _) => Some(match props.start_location_shape {
            StartLocationShape::Sphere => {
                let direction = vrand(rng);
                offset + (direction * props.sphere_radius_range.rand(rng))
            }
            _ => offset + props.start_location_range.rand(rng),
        }),
        _ => None,
    }
}

pub(super) fn independent_origin(props: &EmitterProperties, owner: &super::Owner, time: f32) -> Vector {
    match (props.kind, props.location_shape_extend) {
        (EmitterKind::SuperSprite, ShapeExtend::Line) => props.line_start,
        (EmitterKind::Strip, _) => owner.location,
        _ => owner.location + (owner.velocity * time),
    }
}

impl Emitter {
    /// Its particles stay; its slot count changes only with [`Self::reinitialize`] (as `MaxParticles` only matters to
    /// `Initialize`), and `Disabled` stays as the simulation has it (scripts' toggles come through
    /// `ParticleEmitter::set_disabled`).
    pub fn set_properties(&mut self, index: usize, props: EmitterProperties) {
        let Some(e) = self.emitters.get_mut(index) else { return };
        let disabled = e.props.disabled;
        // The drawn normal is a cache (`RealProjectionNormal`): a script's `ProjectionNormal` write leaves it; the VM's
        // value, non-zero once a `SetProjectionNormal` native or `Initialize` wrote it, replaces it.
        let real_projection_normal =
            if props.real_projection_normal == Vector::ZERO { e.props.real_projection_normal } else { props.real_projection_normal };
        e.props = EmitterProperties { disabled, real_projection_normal, ..props };
    }

    pub fn set_line(&mut self, index: usize, start: Vector, end: Vector) {
        if let Some(e) = self.emitters.get_mut(index) {
            e.props.line_start = start;
            e.props.line_end = end;
        }
    }

    /// Newest particle first. Dead particles stay where they died (retail draws them all), so a stopped
    /// stream hangs until its emitter goes. Empty for a disabled emitter.
    pub fn strip(&self, index: usize) -> Vec<StripPoint> {
        let mut points = Vec::new();
        self.strip_into(index, &mut Vec::new(), &mut points);
        points
    }

    pub fn strip_into(&self, index: usize, order: &mut Vec<usize>, out: &mut Vec<StripPoint>) {
        out.clear();
        let Some(e) = self.emitters.get(index).filter(|e| !e.props.disabled) else { return };
        let owner = self.owner;
        let relative = e.props.coordinate_system == CoordinateSystem::Relative;
        let axes = owner.rotation.axes();
        let used = &e.particles[..e.active_particles];
        order.clear();
        order.extend(0..used.len());
        order.sort_by(|&a, &b| used[a].time.total_cmp(&used[b].time));
        out.extend(order.iter().map(|&slot| {
            let p = &used[slot];
            StripPoint {
                location: if relative { rotate_by(&axes, p.location) + owner.location } else { p.location },
                size: p.size[0],
                color: p.color,
            }
        }));
    }

    pub fn bounds(&self, index: usize, reach: f32) -> Option<(Vector, Vector)> {
        let e = self.emitters.get(index).filter(|e| !e.props.disabled)?;
        let owner = self.owner;
        let relative = e.props.coordinate_system == CoordinateSystem::Relative;
        let axes = owner.rotation.axes();
        let world = |at: Vector| if relative { rotate_by(&axes, at) + owner.location } else { at };
        let (strip, beam) = (e.props.kind == EmitterKind::Strip, e.props.kind == EmitterKind::Beam);
        let mut bounds = None;
        for (slot, p) in e.particles[..e.active_particles].iter().enumerate().filter(|(_, p)| strip || p.active) {
            let reach = reach * p.size[0].abs().max(p.size[1].abs()).max(p.size[2].abs());
            if beam {
                for point in e.beam_points.of_slot(slot) {
                    grow(&mut bounds, world(point.location), reach);
                }
            } else {
                grow(&mut bounds, world(p.location), reach);
            }
        }
        bounds
    }
}

fn grow(bounds: &mut Option<(Vector, Vector)>, at: Vector, reach: f32) {
    let (min, max) = bounds.get_or_insert((at, at));
    for axis in 0..3 {
        min[axis] = min[axis].min(at[axis] - reach);
        max[axis] = max[axis].max(at[axis] + reach);
    }
}

#[cfg(test)]
mod tests {
    use crate::math::Vector;
    use crate::props::{Range, RangeVector};
    use crate::{Emitter, EmitterKind, EmitterProperties, EmitterSettings, ShapeExtend};

    #[test]
    fn strip_runs_from_the_nozzle_along_the_arc() {
        let props = EmitterProperties {
            kind: EmitterKind::Strip,
            max_particles: 13,
            lifetime_range: Range::new(0.5, 0.5),
            acceleration: [0.0, 0.0, -2000.0].into(),
            start_velocity_range: RangeVector { x: Range::new(600.0, 600.0), y: Range::new(0.0, 0.0), z: Range::new(500.0, 500.0) },
            start_size_range: RangeVector::splat(2.0, 2.0),
            seconds_before_inactive: 0.0,
            ..EmitterProperties::default()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
        for _ in 0..15 {
            emitter.tick(1.0 / 30.0, [100.0, 0.0, 50.0].into(), [0; 3].into());
        }
        let strip = emitter.strip(0);
        assert!(strip.len() >= 10, "{strip:?}");
        assert!((strip[0].location[0] - 100.0).abs() < 25.0, "{strip:?}");
        assert!(strip.windows(2).all(|w| w[1].location[0] >= w[0].location[0]), "{strip:?}");
        assert!(strip.last().unwrap().location[0] > 200.0, "{strip:?}");
    }

    #[test]
    fn bounds_hold_the_ribbon() {
        let props = EmitterProperties {
            kind: EmitterKind::Strip,
            max_particles: 13,
            lifetime_range: Range::new(0.5, 0.5),
            start_velocity_range: RangeVector { x: Range::new(600.0, 600.0), y: Range::new(0.0, 0.0), z: Range::new(500.0, 500.0) },
            start_size_range: RangeVector::splat(2.0, 2.0),
            seconds_before_inactive: 0.0,
            ..EmitterProperties::default()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
        for _ in 0..15 {
            emitter.tick(1.0 / 30.0, [100.0, 0.0, 50.0].into(), [0; 3].into());
        }
        let (min, max) = emitter.bounds(0, 1.0).expect("particles");
        let strip = emitter.strip(0);
        assert!(!strip.is_empty());
        for point in &strip {
            for axis in 0..3 {
                assert!(min[axis] <= point.location[axis] - point.size && point.location[axis] + point.size <= max[axis], "{min:?} {max:?} {point:?}");
            }
        }
        emitter.emitters_mut()[0].set_disabled(true);
        assert_eq!(emitter.bounds(0, 1.0), None);
    }

    #[test]
    fn strip_spawns_at_the_owner() {
        let props = EmitterProperties {
            kind: EmitterKind::Strip,
            max_particles: 4,
            respawn_dead_particles: false,
            lifetime_range: Range::new(1.0, 1.0),
            start_velocity_range: RangeVector::splat(100.0, 100.0),
            seconds_before_inactive: 0.0,
            ..EmitterProperties::default()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
        emitter.tick(1.0, [5.0, 6.0, 7.0].into(), [0; 3].into());
        let strip = emitter.strip(0);
        assert_eq!(strip.len(), 4);
        assert!(strip.iter().all(|p| p.location == [5.0, 6.0, 7.0].into()), "{strip:?}");
    }

    #[test]
    fn super_sprite_line_spawns_on_the_line_in_world_space() {
        let props = EmitterProperties {
            kind: EmitterKind::SuperSprite,
            location_shape_extend: ShapeExtend::Line,
            max_particles: 8,
            respawn_dead_particles: false,
            automatic_initial_spawning: false,
            lifetime_range: Range::new(10.0, 10.0),
            seconds_before_inactive: 0.0,
            ..EmitterProperties::default()
        };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props.clone()], 1);
        emitter.tick(0.1, [1000.0, 0.0, 0.0].into(), [0; 3].into());
        emitter.set_line(0, [10.0, 20.0, 0.0].into(), [10.0, 20.0, 0.0].into());
        emitter.spawn_one(0, [0.0; 3].into());
        emitter.set_line(0, [20.0, 20.0, 0.0].into(), [30.0, 20.0, 0.0].into());
        emitter.spawn_one(0, [0.0; 3].into());
        let mut at: Vec<_> = emitter.sprites(0, crate::View::default()).map(|s| s.location).collect();
        at.sort_by(|a, b| a[0].total_cmp(&b[0]));
        assert_eq!(at[0], [10.0, 20.0, 0.0].into());
        assert!((20.0..=30.0).contains(&at[1][0]) && at[1][1] == 20.0, "{at:?}");

        let spawning = EmitterProperties { automatic_initial_spawning: true, allow_particle_spawn: false, ..props };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![spawning.clone()], 1);
        emitter.tick(2.0, [0.0; 3].into(), [0; 3].into());
        assert_eq!(emitter.sprites(0, crate::View::default()).count(), 0);
        emitter.set_properties(0, EmitterProperties { allow_particle_spawn: true, ..spawning });
        emitter.tick(2.0, [0.0; 3].into(), [0; 3].into());
        assert!(emitter.sprites(0, crate::View::default()).count() > 0);
    }

    #[test]
    fn set_properties_keeps_particles() {
        let props = EmitterProperties { max_particles: 5, lifetime_range: Range::new(10.0, 10.0), seconds_before_inactive: 0.0,
            ..EmitterProperties::default() };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props.clone()], 1);
        emitter.tick(5.0, [0.0; 3].into(), [0; 3].into());
        let live = emitter.emitters()[0].live_particles();
        assert!(live > 0);
        emitter.set_properties(0, EmitterProperties { start_size_range: RangeVector::splat(50.0, 50.0), max_particles: 50, ..props });
        assert_eq!(emitter.emitters()[0].live_particles(), live);
        assert_eq!(emitter.emitters()[0].props().start_size_range, RangeVector::splat(50.0, 50.0));
    }

    #[test]
    fn projection_normal_is_cached() {
        let props = EmitterProperties { projection_normal: [0.0, 2.0, 0.0].into(), ..EmitterProperties::default() };
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props.clone()], 1);
        assert_eq!(emitter.emitters()[0].props().real_projection_normal, Vector::from([0.0, 1.0, 0.0]));
        emitter.set_properties(0, EmitterProperties { projection_normal: [1.0, 0.0, 0.0].into(), ..props.clone() });
        assert_eq!(emitter.emitters()[0].props().real_projection_normal, Vector::from([0.0, 1.0, 0.0]));
        emitter.set_properties(0, EmitterProperties { real_projection_normal: [0.0, 0.0, 1.0].into(), ..props });
        assert_eq!(emitter.emitters()[0].props().real_projection_normal, Vector::from([0.0, 0.0, 1.0]));
    }
}
