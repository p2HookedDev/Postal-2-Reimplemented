//! A stream: an arc through the feeder's own strip points from its nozzle, cut where it first meets the level and
//! resampled into rings by length. A thin jet is drawn as a round tube, a paste as a sheet facing the eye.

use std::f32::consts::{E, TAU};

use bevy::math::Vec3;
use p2_vm::FluidType;

use super::air_draw::{AirMesh, Shape};

pub const RINGS: usize = 21;
pub const SIDES: u32 = 8;
const SUBDIVIDE: usize = 4;
const BREAK: (f32, f32) = (0.55, 0.9);
/// A puke gush's length in seconds, the share of it where its surge peaks, and the trickle between surges,
/// against the stream's mean flow. Bending the clock by `GUSH_WARP` keeps the gushes from beating like a
/// metronome.
const GUSH_SECONDS: f32 = 0.6;
const GUSH_PEAK: f32 = 0.15;
pub const TRICKLE: f32 = 0.2;
const GUSH_WARP: f32 = 0.12;
const LEAST_POUR: f32 = 100.0;
const SHEET_STRANDS: u32 = 4;
const FRONT_SOFT: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ring {
    pub at: Vec3,
    pub way: Vec3,
    pub along: f32,
}

pub struct StreamDraw {
    pub kind: FluidType,
    pub burn_reach: Option<f32>,
    pub light: Vec3,
    pub retch: f32,
    pub speed: f32,
    pub fall: f32,
}

pub fn stream_radius(kind: FluidType) -> f32 {
    match kind {
        FluidType::Blood => 0.5,
        FluidType::Puke | FluidType::BloodyPuke => 0.9,
        FluidType::Gas | FluidType::Napalm => 0.4,
        FluidType::Urine | FluidType::BloodyUrine | FluidType::Gonorrhea => 0.35,
    }
}

pub fn paste(kind: FluidType) -> bool {
    matches!(kind, FluidType::Puke | FluidType::BloodyPuke)
}

/// A stream's flow against its mean `seconds` into its retch: 1 for a steady stream. A paste surges in gushes,
/// each a fast surge and a slow ebb into a trickle; the mean stays 1, so it pours what a steady stream would.
pub fn gush(kind: FluidType, seconds: f32) -> f32 {
    if !paste(kind) {
        return 1.0;
    }
    let turns = seconds / GUSH_SECONDS;
    let x = (turns + GUSH_WARP * (turns * 2.3).sin()).rem_euclid(1.0) / GUSH_PEAK;
    TRICKLE + (1.0 - TRICKLE) * x * (1.0 - x).exp() / (GUSH_PEAK * E)
}

pub fn reach(kind: FluidType, beat: f32) -> f32 {
    if kind == FluidType::Blood { BREAK.0 + (BREAK.1 - BREAK.0) * beat } else { 1.0 }
}

pub fn cut_at_hit(nozzle: Vec3, points: &[Vec3], hit: &impl Fn(Vec3, Vec3) -> Option<(Vec3, Vec3)>) -> Vec<Vec3> {
    let mut path = points.to_vec();
    if path.last().is_some_and(|last| last.distance_squared(nozzle) < path[0].distance_squared(nozzle)) {
        path.reverse();
    }
    if let Some((index, at)) = path.windows(2).enumerate().find_map(|(index, pair)| Some((index, hit(pair[0], pair[1])?.0))) {
        path.truncate(index + 1);
        path.push(at);
    }
    path
}

fn catmull([a, b, c, d]: [Vec3; 4], t: f32) -> Vec3 {
    0.5 * (2.0 * b + (c - a) * t + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t + (3.0 * b - a - 3.0 * c + d) * t * t * t)
}

pub fn stream_arc(path: &[Vec3], reach: f32) -> Vec<Ring> {
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

/// How much a ring burns, 0 to 1. Fire climbs in a straight line from where the stream lands, as the game's
/// starter does.
fn burning(stream: &StreamDraw, ring: &Ring, landing: &Ring) -> f32 {
    stream.burn_reach.map_or(0.0, |reach| ((reach - ring.at.distance(landing.at)) / FRONT_SOFT + 0.5).clamp(0.0, 1.0))
}

pub fn ring_vertices(rings: &[Ring], stream: &StreamDraw, mesh: &mut AirMesh) {
    if paste(stream.kind) { sheet_vertices(rings, stream, mesh) } else { tube_vertices(rings, stream, mesh) }
}

/// `SIDES` vertices a ring around a frame carried along the arc without twisting. A vertex's way is its offset
/// from the axis: the jet's radius, thinned as it speeds up so the same flow passes every ring (r = r0 sqrt(v0 /
/// v)).
fn tube_vertices(rings: &[Ring], stream: &StreamDraw, mesh: &mut AirMesh) {
    let (Some(first_ring), Some(last_ring)) = (rings.first(), rings.last()) else { return };
    let end = last_ring.along.max(1e-6);
    let width = stream_radius(stream.kind);
    let pour = stream.speed.max(LEAST_POUR);
    let mut side = first_ring.way.normalize_or(Vec3::NEG_Y).any_orthonormal_vector();
    let first = mesh.vertex_count();
    for ring in rings {
        let way = ring.way.normalize_or(Vec3::NEG_Y);
        side = (side - way * side.dot(way)).normalize_or(way.any_orthonormal_vector());
        let across = way.cross(side);
        let now = (pour * pour + 2.0 * stream.fall * (first_ring.at.y - ring.at.y)).max(0.25 * pour * pour).sqrt();
        let radius = width * (pour / now).sqrt();
        let look = Shape::Tube.look(width, stream.kind, burning(stream, ring, last_ring));
        for corner in 0..SIDES {
            let turn = TAU * corner as f32 / SIDES as f32;
            let offset = (side * turn.cos() + across * turn.sin()) * radius;
            mesh.vertex(ring.at, offset, [corner as f32, ring.along], stream.light.extend(ring.along / end), look, way.extend(pour));
        }
    }
    for ring in 1..rings.len() as u32 {
        let base = first + (ring - 1) * SIDES;
        for corner in 0..SIDES {
            let [a, b] = [base + corner, base + (corner + 1) % SIDES];
            mesh.indices.extend([a, b, b + SIDES, a, b + SIDES, a + SIDES]);
        }
    }
}

/// Two vertices a ring for each rope, widened to face the eye. A corner's x is its side times one more than its
/// rope. The way's length is the stream's width there against its mean: the gush that left the nozzle `along /
/// speed` seconds before, `retch` seconds in.
fn sheet_vertices(rings: &[Ring], stream: &StreamDraw, mesh: &mut AirMesh) {
    let Some(last_ring) = rings.last() else { return };
    let end = last_ring.along.max(1e-6);
    for strand in 0..SHEET_STRANDS {
        let first = mesh.vertex_count();
        for ring in rings {
            let look = Shape::Ribbon.look(stream_radius(stream.kind), stream.kind, burning(stream, ring, last_ring));
            let swell = gush(stream.kind, stream.retch - ring.along / stream.speed.max(LEAST_POUR)).sqrt();
            for side in [-1.0, 1.0] {
                let corner = [side * (strand + 1) as f32, ring.along];
                mesh.vertex(ring.at, ring.way * swell, corner, stream.light.extend(ring.along / end), look, ring.way.extend(stream.speed));
            }
        }
        for ring in 1..rings.len() as u32 {
            let base = first + (ring - 1) * 2;
            mesh.indices.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(count: usize) -> Vec<Vec3> {
        (0..count).map(|index| Vec3::new(index as f32 * 10.0, 0.0, 0.0)).collect()
    }

    fn no_hit(_: Vec3, _: Vec3) -> Option<(Vec3, Vec3)> {
        None
    }

    fn stream(kind: FluidType) -> StreamDraw {
        StreamDraw { kind, burn_reach: None, light: Vec3::ONE, retch: 0.0, speed: 200.0, fall: 950.0 }
    }

    #[test]
    fn fire_climbs_from_where_the_stream_lands() {
        let rings = stream_arc(&line(21), 1.0);
        let lit = StreamDraw { burn_reach: Some(90.0), ..stream(FluidType::Gas) };
        let landing = &rings[RINGS - 1];
        assert_eq!(burning(&lit, landing, landing), 1.0);
        assert_eq!(burning(&lit, &rings[0], landing), 0.0);
        assert!(rings.iter().all(|ring| burning(&stream(FluidType::Gas), ring, landing) == 0.0));
        let mut mesh = AirMesh::default();
        tube_vertices(&rings, &lit, &mut mesh);
        let burn = |vertex: usize| mesh.looks[vertex][3];
        assert_eq!((burn(0), burn((RINGS - 1) * SIDES as usize)), (0.0, 1.0));
    }

    #[test]
    fn the_arc_runs_from_the_nozzle_through_every_strip_point() {
        let mut points = line(5);
        points.reverse();
        let rings = stream_arc(&cut_at_hit(Vec3::ZERO, &points, &no_hit), 1.0);
        assert_eq!(rings.len(), RINGS);
        assert!(rings[0].at.distance(Vec3::ZERO) < 1e-4);
        assert!(rings[RINGS - 1].at.distance(Vec3::new(40.0, 0.0, 0.0)) < 1e-3);
        assert!((rings[RINGS - 1].along - 40.0).abs() < 1e-3);
        assert!(rings.iter().all(|ring| ring.way.distance(Vec3::X) < 1e-4 && ring.at.y.abs() < 1e-4));
        assert!(stream_arc(&points[..1], 1.0).is_empty());
    }

    #[test]
    fn the_arc_ends_where_it_first_meets_the_level() {
        let wall = |from: Vec3, to: Vec3| (from.x < 25.0 && to.x >= 25.0).then(|| (Vec3::new(25.0, 0.0, 0.0), Vec3::NEG_X));
        let path = cut_at_hit(Vec3::ZERO, &line(5), &wall);
        assert_eq!(path.len(), 4);
        let rings = stream_arc(&path, 1.0);
        assert!(rings[RINGS - 1].at.distance(Vec3::new(25.0, 0.0, 0.0)) < 1e-3);
        let blood = stream_arc(&path, reach(FluidType::Blood, 0.0));
        assert!((blood[RINGS - 1].at.x - 25.0 * BREAK.0).abs() < 1e-3, "the break is a share of the cut arc");
    }

    #[test]
    fn only_blood_breaks_and_its_break_beats() {
        let rings = stream_arc(&line(5), reach(FluidType::Blood, 0.0));
        assert!((rings[RINGS - 1].at.x - 40.0 * BREAK.0).abs() < 1e-3);
        assert_eq!(reach(FluidType::Blood, 1.0), BREAK.1);
        assert_eq!(reach(FluidType::Urine, 0.0), 1.0);
    }

    #[test]
    fn a_jet_is_a_closed_tube_that_thins_as_it_falls() {
        let mut mesh = AirMesh::default();
        let falling: Vec<Vec3> = (0..5).map(|index| Vec3::new(0.0, -(index as f32) * 30.0, 0.0)).collect();
        let rings = stream_arc(&falling, 1.0);
        tube_vertices(&rings, &stream(FluidType::Urine), &mut mesh);
        assert_eq!(mesh.vertex_count(), RINGS as u32 * SIDES);
        assert_eq!(mesh.indices.len(), (RINGS - 1) * SIDES as usize * 6);
        assert_eq!(mesh.indices.iter().max().copied(), Some(RINGS as u32 * SIDES - 1));
        let radius = |vertex: usize| Vec3::from_array(mesh.ways[vertex]).length();
        let (top, bottom) = (radius(0), radius((RINGS - 1) * SIDES as usize));
        assert!((top - stream_radius(FluidType::Urine)).abs() < 1e-4 && bottom < top * 0.9, "{top} {bottom}");
        assert!((0..SIDES as usize).all(|vertex| Vec3::from_array(mesh.ways[vertex]).dot(Vec3::NEG_Y).abs() < 1e-4), "the ring lies across the arc");
    }

    #[test]
    fn puke_pours_as_a_sheet_of_ropes() {
        let mut mesh = AirMesh::default();
        let rings = stream_arc(&line(3), 1.0);
        ring_vertices(&rings, &stream(FluidType::Puke), &mut mesh);
        let vertices = RINGS * 2 * SHEET_STRANDS as usize;
        assert_eq!(mesh.vertex_count() as usize, vertices);
        assert_eq!(mesh.indices.iter().max().copied(), Some(vertices as u32 - 1));
    }

    #[test]
    fn puke_gushes_but_pours_its_mean_and_other_streams_flow_steady() {
        let samples = 60_000;
        let mean = (0..samples).map(|step| gush(FluidType::Puke, step as f32 * 0.001)).sum::<f32>() / samples as f32;
        assert!((mean - 1.0).abs() < 0.05, "{mean}");
        let (low, high) = (0..600).map(|step| gush(FluidType::Puke, step as f32 * 0.001)).fold((f32::MAX, 0.0_f32), |(low, high), flow| (low.min(flow), high.max(flow)));
        assert!(low < TRICKLE + 0.05 && high > 2.0, "{low} {high}");
        assert_eq!(gush(FluidType::Urine, 0.3), 1.0);
        assert!(!paste(FluidType::Blood) && paste(FluidType::BloodyPuke));
    }
}
