//! Beam emitters: a particle's points are worked out once when it spawns, and later updates only age
//! and colour it, so the bolt stays where it was spawned.

use super::draw::key_span;
use super::{Collide, Emitter, ParticleEmitter};
use crate::Rng;
use crate::math::{Vector, fractional, rotate_by};
use crate::props::{BeamEnd, BeamEndPoint, BeamScale, CoordinateSystem, EmitterKind, EmitterProperties, RangeVector};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BeamPoint {
    pub location: Vector,
    pub fraction: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Beam {
    pub points: Vec<BeamPoint>,
    pub size: f32,
    pub color: [u8; 4],
}

const MAX_BEAM_POINTS: usize = 1024;

#[derive(Debug, Clone, Default)]
pub(super) struct BeamPoints {
    /// Point counts as sized at init: scripts changing them later change nothing.
    low: usize,
    high: usize,
    points: Vec<BeamPoint>,
    traces: Vec<usize>,
    lows: Vec<Vector>,
}

impl BeamPoints {
    pub(super) fn new(props: &EmitterProperties, slots: usize) -> Self {
        if props.kind != EmitterKind::Beam {
            return Self::default();
        }
        let count = |n: i32| usize::try_from(n).unwrap_or(0).clamp(1, MAX_BEAM_POINTS);
        let (low, high) = (count(props.beam.low_points), count(props.beam.high_points));
        Self { low, high, points: vec![BeamPoint::default(); slots * high], traces: Vec::new(), lows: Vec::with_capacity(low) }
    }

    pub(super) fn of_slot(&self, slot: usize) -> &[BeamPoint] {
        self.points.get(slot * self.high..(slot + 1) * self.high).unwrap_or(&[])
    }
}

fn random_end(points: &[BeamEndPoint], rng: &mut Rng) -> Option<Vector> {
    let total: f32 = points.iter().map(|point| point.weight).sum();
    let mut picked = points.first();
    if total != 0.0 {
        let at = rng.frand() * total;
        let mut sum = 0.0;
        for point in points {
            if sum < at && at <= sum + point.weight {
                picked = Some(point);
                break;
            }
            sum += point.weight;
        }
    }
    picked.map(|point| point.offset.rand(rng))
}

fn scale_at(keys: &[BeamScale], at: f32) -> Option<Vector> {
    let (previous, key, f) = key_span(keys, at, |key| key.relative_length)?;
    let previous = previous.map_or(Vector::from([1.0; 3]), |before| before.scale);
    Some(previous + (key.scale - previous) * f)
}

fn point_noise(previous: Vector, plain: &RangeVector, keys: Option<&[BeamScale]>, at: f32, scaled: &RangeVector, rng: &mut Rng) -> Vector {
    match keys {
        None => plain.rand(rng),
        Some(keys) => scale_at(keys, at).map_or(previous, |scale| scale * scaled.rand(rng)),
    }
}

/// Where along the scale keys point `i` of `count` is: its share of `count` (not `count - 1`), repeated.
fn scale_fraction(i: usize, count: usize, repeats: f32) -> f32 {
    fractional(i as f32 * (1.0 / count as f32) * (repeats + 1.0))
}

impl ParticleEmitter {
    pub(super) fn beam_spawned(&mut self, index: usize, rng: &mut Rng) {
        if self.props.kind == EmitterKind::Beam {
            self.fill_beam(index, None, rng);
            if self.props.beam.end_by == BeamEnd::TraceOffset {
                self.beam_points.traces.push(index);
            }
        }
    }

    pub(super) fn trace_beams(&mut self, collide: &mut Collide, rng: &mut Rng) {
        let mut traces = std::mem::take(&mut self.beam_points.traces);
        for &index in &traces {
            self.fill_beam(index, Some(&mut *collide), rng);
        }
        traces.clear();
        self.beam_points.traces = traces;
    }

    fn fill_beam(&mut self, index: usize, collide: Option<&mut Collide>, rng: &mut Rng) {
        let beam = &self.props.beam;
        let Some(p) = self.particles.get_mut(index) else { return };
        let (low, high) = (self.beam_points.low, self.beam_points.high);
        let location = p.location;
        // With no end point entries retail leaves the end at zero and pins to the world origin.
        let nowhere = (Vector::ZERO, Some(Vector::ZERO));
        let (mut end, mut pin) = match beam.end_by {
            BeamEnd::Velocity => (p.velocity * p.max_lifetime, None),
            BeamEnd::Distance => {
                // Retail normalizes the particle's own velocity here.
                p.velocity = p.velocity.safe_normal();
                (p.velocity * beam.distance_range.rand(rng), None)
            }
            BeamEnd::Offset | BeamEnd::Actor | BeamEnd::TraceOffset => {
                random_end(&beam.end_points, rng).map_or(nowhere, |offset| (offset, Some(location + offset)))
            }
            BeamEnd::OffsetAsAbsolute => random_end(&beam.end_points, rng).map_or(nowhere, |target| (target - location, Some(target))),
        };
        if let (Some(collide), Some(target)) = (collide, pin)
            && let Some(hit) = collide(location, target)
        {
            end = hit.location - location;
            pin = Some(hit.location);
        }
        let pin = pin.filter(|_| !beam.noise_determines_end_point);

        let low_step = 1.0 / (low - 1) as f32;
        let lows = &mut self.beam_points.lows;
        lows.clear();
        lows.push(location);
        let mut noise = Vector::ZERO;
        for i in 1..low {
            let keys = beam.use_low_scale.then_some(beam.low_scales.as_slice());
            // With the low scale on, retail draws from the high frequency range: kept.
            noise = point_noise(noise, &beam.low_noise, keys, scale_fraction(i, low, beam.low_scale_repeats), &beam.high_noise, rng);
            lows.push((location + (end * (i as f32 * low_step))) + noise);
        }
        if let (Some(pin), Some(last)) = (pin, lows.last_mut()) {
            *last = pin;
        }

        let Some(points) = self.beam_points.points.get_mut(index * high..(index + 1) * high) else { return };
        points[0] = BeamPoint { location: lows[0], fraction: 0.0 };
        let step = 1.0 / (high - 1) as f32;
        let (mut along, mut segment) = (step, 0usize);
        let mut noise = Vector::ZERO;
        for (i, point) in points.iter_mut().enumerate().skip(1) {
            let keys = beam.use_high_scale.then_some(beam.high_scales.as_slice());
            noise = point_noise(noise, &beam.high_noise, keys, scale_fraction(i, high, beam.high_scale_repeats), &beam.high_noise, rng);
            let t = (low - 1) as f32 * along;
            let (from, to) = (lows[segment.min(low - 1)], lows[(segment + 1).min(low - 1)]);
            *point = BeamPoint { location: ((from * (1.0 - t)) + (to * t)) + noise, fraction: i as f32 * step };
            along += step;
            if low_step <= along {
                along -= low_step;
                segment += 1;
            }
        }
        if let (Some(pin), Some(last)) = (pin, points.last_mut()) {
            last.location = pin;
        }
    }
}

impl Emitter {
    pub fn beams(&self, index: usize) -> Vec<Beam> {
        let mut beams = Vec::new();
        self.beams_into(index, &mut beams);
        beams
    }

    pub fn beams_into(&self, index: usize, out: &mut Vec<Beam>) {
        let mut count = 0;
        if let Some(e) = self.emitters.get(index).filter(|e| !e.props.disabled && e.props.kind == EmitterKind::Beam) {
            let owner = self.owner;
            let relative = e.props.coordinate_system == CoordinateSystem::Relative;
            let axes = owner.rotation.axes();
            let world = |at: Vector| if relative { rotate_by(&axes, at) + owner.location } else { at };
            for (slot, p) in e.live() {
                if count == out.len() {
                    out.push(Beam { points: Vec::new(), size: 0.0, color: [0; 4] });
                }
                let beam = &mut out[count];
                count += 1;
                beam.points.clear();
                beam.points.extend(e.beam_points.of_slot(slot).iter().map(|point| BeamPoint { location: world(point.location), fraction: point.fraction }));
                beam.size = p.size[0];
                beam.color = p.color;
            }
        }
        out.truncate(count);
    }
}

#[cfg(test)]
mod tests {
    use super::Beam;
    use crate::math::Vector;
    use crate::props::{BeamEnd, BeamEndPoint, BeamProperties, Range, RangeVector};
    use crate::{CollisionHit, Emitter, EmitterKind, EmitterProperties, EmitterSettings, Owner, PropertyValue};

    const AT: [f32; 3] = [10.0, 20.0, 30.0];

    fn along_x(distance: f32) -> RangeVector {
        RangeVector { x: Range::new(distance, distance), y: Range::new(0.0, 0.0), z: Range::new(0.0, 0.0) }
    }

    fn emitter(beam: BeamProperties, start_velocity_range: RangeVector) -> Emitter {
        let props = EmitterProperties {
            kind: EmitterKind::Beam,
            max_particles: 1,
            lifetime_range: Range::new(1.0, 1.0),
            seconds_before_inactive: 0.0,
            start_velocity_range,
            beam,
            ..EmitterProperties::default()
        };
        Emitter::new(EmitterSettings::default(), vec![props], 1)
    }

    fn offset_beam(low_points: i32, high_points: i32) -> BeamProperties {
        BeamProperties {
            end_by: BeamEnd::Offset,
            end_points: vec![BeamEndPoint { offset: along_x(100.0), weight: 0.0 }],
            low_points,
            high_points,
            ..BeamProperties::default()
        }
    }

    fn only_beam(emitter: &Emitter) -> Beam {
        let mut beams = emitter.beams(0);
        assert_eq!(beams.len(), 1);
        beams.remove(0)
    }

    fn assert_near(at: Vector, expected: [f32; 3]) {
        assert!((0..3).all(|axis| (at[axis] - expected[axis]).abs() < 1e-3), "{at:?} is not {expected:?}");
    }

    #[test]
    fn points_run_evenly_to_the_end() {
        let mut emitter = emitter(offset_beam(3, 5), along_x(0.0));
        emitter.tick(1.0, AT.into(), [0; 3].into());
        let beam = only_beam(&emitter);
        assert_eq!(beam.points.len(), 5);
        for (i, point) in beam.points.iter().enumerate() {
            assert_near(point.location, [10.0 + 25.0 * i as f32, 20.0, 30.0]);
            assert!((point.fraction - 0.25 * i as f32).abs() < 1e-5, "{point:?}");
        }
    }

    #[test]
    fn end_is_pinned_unless_noise_determines_it() {
        let noisy = BeamProperties { low_noise: RangeVector::splat(3.0, 3.0), high_noise: RangeVector::splat(5.0, 5.0), ..offset_beam(2, 3) };
        let mut pinned = emitter(noisy.clone(), along_x(0.0));
        pinned.tick(1.0, AT.into(), [0; 3].into());
        let points = only_beam(&pinned).points;
        assert_near(points[0].location, AT);
        assert_near(points[1].location, [65.0, 25.0, 35.0]);
        assert_near(points[2].location, [110.0, 20.0, 30.0]);

        let mut free = emitter(BeamProperties { noise_determines_end_point: true, ..noisy }, along_x(0.0));
        free.tick(1.0, AT.into(), [0; 3].into());
        assert_near(only_beam(&free).points[2].location, [118.0, 28.0, 38.0]);
    }

    #[test]
    fn velocity_end_and_the_bolt_stays() {
        let mut emitter = emitter(BeamProperties::default(), along_x(150.0));
        emitter.tick(1.0, AT.into(), [0; 3].into());
        let spawned = only_beam(&emitter);
        assert_eq!(spawned.points.len(), 10);
        assert_near(spawned.points[9].location, [160.0, 20.0, 30.0]);
        emitter.tick(0.1, AT.into(), [0; 3].into());
        assert_eq!(only_beam(&emitter).points, spawned.points);
    }

    #[test]
    fn trace_ends_at_the_hit() {
        let beam = BeamProperties { end_by: BeamEnd::TraceOffset, ..offset_beam(3, 5) };
        let mut emitter = emitter(beam, along_x(0.0));
        let owner = Owner { location: AT.into(), ..Owner::default() };
        emitter.tick_colliding(1.0, owner, &mut |_, _| Some(CollisionHit { location: [50.0, 20.0, 30.0].into(), normal: [-1.0, 0.0, 0.0].into() }));
        let points = only_beam(&emitter).points;
        assert_near(points[2].location, [30.0, 20.0, 30.0]);
        assert_near(points[4].location, [50.0, 20.0, 30.0]);
    }

    #[test]
    fn end_points_read_positionally() {
        let range = |v: f32| PropertyValue::Struct(vec![PropertyValue::Float(v), PropertyValue::Float(v)]);
        let lookup = |name: &str| match name {
            "DetermineEndPointBy" => Some(PropertyValue::Int(4)),
            "BeamEndPoints" => Some(PropertyValue::Array(vec![PropertyValue::Struct(vec![
                PropertyValue::Int(0),
                PropertyValue::Struct(vec![range(7.0), range(0.0), range(0.0)]),
                PropertyValue::Float(2.0),
            ])])),
            _ => None,
        };
        let beam = EmitterProperties::from_lookup(lookup).beam;
        assert_eq!(beam.end_by, BeamEnd::TraceOffset);
        assert_eq!(beam.end_points, vec![BeamEndPoint { offset: along_x(7.0), weight: 2.0 }]);
        assert_eq!((beam.low_points, beam.high_points), (3, 10));
    }
}
