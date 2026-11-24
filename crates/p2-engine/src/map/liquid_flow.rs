use bevy::math::Vec3;
use p2_math::Vector;
use p2_particles::StripPoint;

pub(super) const PIECES: usize = 4;
/// Seeds stay small: the shader reads one as a float.
const SEEDS: u32 = 1023;

/// The seed of a modern puddle's edge, never 0 (0 marks a sprite that is no puddle).
pub(super) fn puddle_seed(emitter: u32) -> u32 {
    emitter % SEEDS + 1
}

/// A Catmull-Rom curve through the script's points, `PIECES` points per segment, passing through every script point.
/// The scripts already throw a stream's points under gravity and carry its texture along: only the corners are new.
pub(super) fn ribbon(points: &[StripPoint]) -> Vec<StripPoint> {
    let last = points.len().saturating_sub(1);
    let at = |i: usize| Vec3::from_array(points[i.min(last)].location.into());
    let mut out = Vec::with_capacity(last * PIECES + 1);
    for i in 0..last {
        let (p0, p1, p2, p3) = (at(i.saturating_sub(1)), at(i), at(i + 1), at(i + 2));
        let (from, to) = (points[i], points[i + 1]);
        for k in 0..PIECES {
            let t = k as f32 / PIECES as f32;
            let curve = 0.5 * (2.0 * p1 + (p2 - p0) * t + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t);
            out.push(StripPoint { location: Vector::from(curve.to_array()), size: from.size + (to.size - from.size) * t, color: from.color });
        }
    }
    out.extend(points.get(last).copied());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, z: f32) -> StripPoint {
        StripPoint { location: [x, 0.0, z].into(), size: 2.0, color: [255; 4] }
    }

    fn at(p: &StripPoint) -> Vec3 {
        Vec3::from_array(p.location.into())
    }

    #[test]
    fn ribbon_passes_through_the_script_points() {
        let points = [point(0.0, 0.0), point(10.0, -5.0), point(20.0, -20.0), point(25.0, -40.0)];
        let smooth = ribbon(&points);
        assert_eq!(smooth.len(), 3 * PIECES + 1);
        for (i, p) in points.iter().enumerate() {
            assert!(at(&smooth[i * PIECES]).abs_diff_eq(at(p), 1e-4), "{i}");
        }
    }

    #[test]
    fn a_straight_stream_stays_straight() {
        assert!(ribbon(&[point(0.0, 0.0), point(10.0, 0.0), point(20.0, 0.0)]).iter().all(|p| at(p).z == 0.0));
        assert!(ribbon(&[point(0.0, 0.0), point(0.0, -10.0), point(0.0, -20.0)]).iter().all(|p| at(p).x.abs() < 1e-4));
    }

    #[test]
    fn a_puddle_seed_is_never_zero() {
        assert_eq!(puddle_seed(7), 8);
        assert_eq!(puddle_seed(1023), 1);
    }
}
