//! A stream's ribbon: an arc through the feeder's own strip points from its nozzle, resampled into rings by length.

use bevy::math::Vec3;
use p2_vm::FluidType;

use super::air_draw::{AirMesh, Shape};

pub const RINGS: usize = 21;
/// Arc points made between two strip points.
const SUBDIVIDE: usize = 4;
/// Where a blood stream breaks into drops, as a share of its arc, between beats and at the systole.
const BREAK: (f32, f32) = (0.55, 0.9);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ring {
    pub at: Vec3,
    pub way: Vec3,
    /// Unreal units from the nozzle.
    pub along: f32,
}

pub fn stream_radius(kind: FluidType) -> f32 {
    match kind {
        FluidType::Blood => 0.5,
        FluidType::Puke | FluidType::BloodyPuke => 0.7,
        FluidType::Gas | FluidType::Napalm => 0.4,
        FluidType::Urine | FluidType::BloodyUrine | FluidType::Gonorrhea => 0.35,
    }
}

/// The share of the arc drawn: a blood stream breaks into drops, which beat with the wound.
pub fn reach(kind: FluidType, beat: f32) -> f32 {
    if kind == FluidType::Blood { BREAK.0 + (BREAK.1 - BREAK.0) * beat } else { 1.0 }
}

fn catmull([a, b, c, d]: [Vec3; 4], t: f32) -> Vec3 {
    0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t + (3.0 * b - a - 3.0 * c + d) * t * t * t)
}

/// `RINGS` rings along the arc through `points` from the end nearer `nozzle`, cut at `reach` of its length; none for
/// fewer than two points.
pub fn stream_arc(nozzle: Vec3, points: &[Vec3], reach: f32) -> Vec<Ring> {
    let mut path: Vec<Vec3> = points.to_vec();
    if path.last().is_some_and(|last| last.distance_squared(nozzle) < path[0].distance_squared(nozzle)) {
        path.reverse();
    }
    if path.len() < 2 {
        return Vec::new();
    }
    let last = path.len() as isize - 1;
    let at = |index: isize| path[index.clamp(0, last) as usize];
    let dense: Vec<Vec3> = (0..last)
        .flat_map(|index| (0..SUBDIVIDE).map(move |step| catmull([at(index - 1), at(index), at(index + 1), at(index + 2)], step as f32 / SUBDIVIDE as f32)))
        .chain(path.last().copied())
        .collect();
    let lengths: Vec<f32> = std::iter::once(0.0)
        .chain(dense.windows(2).scan(0.0, |total, pair| {
            *total += pair[0].distance(pair[1]);
            Some(*total)
        }))
        .collect();
    let end = lengths[lengths.len() - 1] * reach.clamp(0.0, 1.0);
    if end <= 0.0 {
        return Vec::new();
    }
    (0..RINGS)
        .map(|ring| {
            let want = end * ring as f32 / (RINGS - 1) as f32;
            let next = lengths.partition_point(|&length| length < want).clamp(1, dense.len() - 1);
            let span = lengths[next] - lengths[next - 1];
            let share = if span > 0.0 { (want - lengths[next - 1]) / span } else { 0.0 };
            Ring { at: dense[next - 1].lerp(dense[next], share), way: (dense[next] - dense[next - 1]).normalize_or_zero(), along: want }
        })
        .collect()
}

/// Two vertices a ring, widened to face the eye in `liquid_ribbon.wesl`.
pub fn ring_vertices(rings: &[Ring], kind: FluidType, burning: f32, light: Vec3, mesh: &mut AirMesh) {
    let radius = stream_radius(kind);
    let first = mesh.vertex_count();
    for ring in rings {
        for side in [-1.0, 1.0] {
            mesh.vertex(ring.at, ring.way, [side, ring.along], light, Shape::Ribbon.look(radius, kind, burning));
        }
    }
    for ring in 1..rings.len() as u32 {
        let base = first + (ring - 1) * 2;
        mesh.indices.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(count: usize) -> Vec<Vec3> {
        (0..count).map(|index| Vec3::new(index as f32 * 10.0, 0.0, 0.0)).collect()
    }

    #[test]
    fn the_arc_runs_from_the_nozzle_through_every_strip_point() {
        let mut points = line(5);
        points.reverse();
        let rings = stream_arc(Vec3::ZERO, &points, 1.0);
        assert_eq!(rings.len(), RINGS);
        assert!(rings[0].at.distance(Vec3::ZERO) < 1e-4);
        assert!(rings[RINGS - 1].at.distance(Vec3::new(40.0, 0.0, 0.0)) < 1e-3);
        assert!((rings[RINGS - 1].along - 40.0).abs() < 1e-3);
        assert!(rings.iter().all(|ring| ring.way.distance(Vec3::X) < 1e-4 && ring.at.y.abs() < 1e-4));
        assert!(stream_arc(Vec3::ZERO, &points[..1], 1.0).is_empty());
    }

    #[test]
    fn only_blood_breaks_and_its_break_beats() {
        let rings = stream_arc(Vec3::ZERO, &line(5), reach(FluidType::Blood, 0.0));
        assert!((rings[RINGS - 1].at.x - 40.0 * BREAK.0).abs() < 1e-3);
        assert_eq!(reach(FluidType::Blood, 1.0), BREAK.1);
        assert_eq!(reach(FluidType::Urine, 0.0), 1.0);
    }

    #[test]
    fn a_ring_makes_two_vertices_and_a_segment_two_triangles() {
        let mut mesh = AirMesh::default();
        let rings = stream_arc(Vec3::ZERO, &line(3), 1.0);
        ring_vertices(&rings, FluidType::Gas, 0.0, Vec3::ONE, &mut mesh);
        assert_eq!(mesh.vertex_count() as usize, RINGS * 2);
        assert_eq!(mesh.indices.len(), (RINGS - 1) * 6);
        assert_eq!(mesh.indices.iter().max().copied(), Some(RINGS as u32 * 2 - 1));
    }
}
