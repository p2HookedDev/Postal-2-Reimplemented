//! Box sweeps against triangles.
//! A pawn's collision cylinder moves as a box of half-size (radius, radius, height).

use p2_assets::Terrain;

use p2_math::Vector;

use crate::{Hit, MeshCollision};

const UNITS: [Vector; 3] = [Vector::new(1.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(0.0, 0.0, 1.0)];

fn triangle_axes(edges: [Vector; 3]) -> [Vector; 13] {
    let mut axes = [Vector::ZERO; 13];
    axes[..3].copy_from_slice(&UNITS);
    axes[3] = edges[0].cross(edges[1]);
    for (i, unit) in UNITS.iter().enumerate() {
        for (j, edge) in edges.iter().enumerate() {
            axes[4 + i * 3 + j] = unit.cross(*edge);
        }
    }
    axes
}

/// Separating-axis test. A box already overlapping the triangle hits at time 0 if it entered less
/// than one `delta` ago (entry time above −1) and keeps going in; one leaving or deeper does not.
fn box_triangle(start: Vector, delta: Vector, extent: Vector, tri: [[f32; 3]; 3]) -> Option<(f32, Vector)> {
    let [a, b, c] = tri.map(Vector::from);
    let edges = [b - a, c - b, a - c];
    let (mut enter, mut exit) = (f32::NEG_INFINITY, f32::INFINITY);
    let mut normal = Vector::ZERO;
    for axis in triangle_axes(edges) {
        let length_squared = axis.size_squared();
        if length_squared < 1e-12 {
            continue;
        }
        let projected = [a, b, c].map(|p| axis.dot(p - start));
        let reach = extent.x * axis.x.abs() + extent.y * axis.y.abs() + extent.z * axis.z.abs();
        let low = projected.iter().copied().fold(f32::INFINITY, f32::min) - reach;
        let high = projected.iter().copied().fold(f32::NEG_INFINITY, f32::max) + reach;
        let speed = axis.dot(delta);
        // Motion under 1e-9 along the axis's unit direction is no motion: `speed` is that motion times the axis's length.
        if speed * speed < 1e-18 * length_squared {
            if low > 0.0 || high < 0.0 {
                return None;
            }
            continue;
        }
        let (t0, t1) = if speed > 0.0 { (low / speed, high / speed) } else { (high / speed, low / speed) };
        if t0 > enter {
            enter = t0;
            normal = if speed > 0.0 { -axis } else { axis };
        }
        exit = exit.min(t1);
        if enter > exit {
            return None;
        }
    }
    (enter > -1.0 && enter <= 1.0 && exit > 0.0).then(|| (enter.max(0.0), normal / normal.size()))
}

pub fn box_overlaps_triangle(center: Vector, extent: Vector, tri: [[f32; 3]; 3]) -> bool {
    let tri = tri.map(Vector::from);
    let edges = [tri[1] - tri[0], tri[2] - tri[1], tri[0] - tri[2]];
    triangle_axes(edges).into_iter().all(|axis| {
        let projected = tri.map(|p| axis.dot(p - center));
        let reach = extent.x * axis.x.abs() + extent.y * axis.y.abs() + extent.z * axis.z.abs();
        let low = projected.iter().copied().fold(f32::INFINITY, f32::min);
        let high = projected.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        low <= reach && high >= -reach
    })
}

fn nearest(hits: impl Iterator<Item = (f32, Vector)>, start: Vector, end: Vector) -> Option<Hit> {
    hits.min_by(|a, b| a.0.total_cmp(&b.0)).map(|(time, normal)| Hit { time, location: start.lerp(end, time), normal })
}

fn swept_bounds(start: Vector, end: Vector, extent: Vector) -> (Vector, Vector) {
    ([0, 1, 2].map(|i| start[i].min(end[i]) - extent[i]).into(), [0, 1, 2].map(|i| start[i].max(end[i]) + extent[i]).into())
}

fn overlaps(min: Vector, max: Vector, tri: &[[f32; 3]; 3]) -> bool {
    (0..3).all(|i| tri.iter().any(|p| p[i] >= min[i]) && tri.iter().any(|p| p[i] <= max[i]))
}

/// Of hits at the same time, the first mesh's and in it the lowest triangle's wins.
pub fn mesh_box_check<'a>(meshes: impl IntoIterator<Item = &'a MeshCollision>, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
    let (min, max) = swept_bounds(start, end, extent);
    let delta = end - start;
    let hits = meshes.into_iter().filter(|mesh| mesh.touches(min, max)).filter_map(|mesh| mesh_first_hit(mesh, start, delta, extent, (min, max)));
    nearest(hits, start, end)
}

fn mesh_first_hit(mesh: &MeshCollision, start: Vector, delta: Vector, extent: Vector, (min, max): (Vector, Vector)) -> Option<(f32, Vector)> {
    let mut first: Option<(f32, usize, Vector)> = None;
    mesh.tree.any(min, max, |triangle| {
        let tri = mesh.triangles[triangle];
        if overlaps(min, max, &tri)
            && let Some((time, normal)) = box_triangle(start, delta, extent, tri)
            && first.is_none_or(|(best, lowest, _)| time.total_cmp(&best).then(triangle.cmp(&lowest)).is_lt())
        {
            first = Some((time, triangle, normal));
        }
        false
    });
    first.map(|(time, _, normal)| (time, normal))
}

pub fn mesh_box_overlaps<'a>(meshes: impl IntoIterator<Item = &'a MeshCollision>, center: Vector, extent: Vector) -> bool {
    let (min, max) = swept_bounds(center, center, extent);
    meshes.into_iter().filter(|mesh| mesh.touches(min, max)).any(|mesh| {
        mesh.tree.any(min, max, |triangle| {
            let tri = mesh.triangles[triangle];
            overlaps(min, max, &tri) && box_overlaps_triangle(center, extent, tri)
        })
    })
}

pub fn terrain_box_check(terrain: &Terrain, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
    let (min, max) = swept_bounds(start, end, extent);
    let delta = end - start;
    crate::terrain_first_hit(terrain, start, end, [extent.x, extent.y], 0.0, |triangle| {
        let tri = triangle.map(|i| terrain.vertices[i]);
        if overlaps(min, max, &tri) { box_triangle(start, delta, extent, tri) } else { None }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: [[f32; 3]; 3] = [[-100.0, -100.0, 0.0], [100.0, -100.0, 0.0], [0.0, 100.0, 0.0]];

    const fn v(x: f32, y: f32, z: f32) -> Vector {
        Vector::new(x, y, z)
    }

    #[test]
    fn falling_box_lands_on_its_bottom() {
        let (time, normal) = box_triangle(v(0.0, 0.0, 100.0), v(0.0, 0.0, -100.0), v(10.0, 10.0, 50.0), FLOOR).unwrap();
        assert!((time - 0.5).abs() < 1e-5, "time {time}");
        assert_eq!(normal, v(0.0, 0.0, 1.0));
    }

    #[test]
    fn sliding_above_and_starting_inside() {
        let extent = v(10.0, 10.0, 50.0);
        assert!(box_triangle(v(0.0, 0.0, 52.0), v(50.0, 0.0, 0.0), extent, FLOOR).is_none());
        assert!(box_triangle(v(0.0, 0.0, 40.0), v(0.0, 0.0, 10.0), extent, FLOOR).is_none());
        assert!(box_triangle(v(0.0, 0.0, 40.0), v(30.0, 0.0, 0.0), extent, FLOOR).is_none());
        assert!(box_triangle(v(0.0, 0.0, 40.0), v(0.0, 0.0, -10.0), extent, FLOOR).is_none());
        assert_eq!(box_triangle(v(0.0, 0.0, 40.0), v(0.0, 0.0, -20.0), extent, FLOOR), Some((0.0, v(0.0, 0.0, 1.0))));
    }

    #[test]
    fn walls_block_from_both_sides_and_fast_moves() {
        let wall = [[0.0, -100.0, -100.0], [0.0, 100.0, -100.0], [0.0, 0.0, 100.0]];
        let extent = v(25.0, 25.0, 44.0);
        let (front, normal) = box_triangle(v(-100.0, 0.0, 0.0), v(200.0, 0.0, 0.0), extent, wall).unwrap();
        assert!((front - 75.0 / 200.0).abs() < 1e-5 && normal == v(-1.0, 0.0, 0.0), "{front} {normal:?}");
        let (back, normal) = box_triangle(v(100.0, 0.0, 0.0), v(-200.0, 0.0, 0.0), extent, wall).unwrap();
        assert!((back - 75.0 / 200.0).abs() < 1e-5 && normal == v(1.0, 0.0, 0.0), "{back} {normal:?}");
        let (fast, _) = box_triangle(v(-30.0, 0.0, 0.0), v(10000.0, 0.0, 0.0), extent, wall).unwrap();
        assert!((fast - 5.0 / 10000.0).abs() < 1e-6, "{fast}");
        assert!(box_overlaps_triangle(v(-20.0, 0.0, 0.0), extent, wall) && !box_overlaps_triangle(v(-26.0, 0.0, 0.0), extent, wall));
    }

    #[test]
    fn thin_triangles_collide() {
        let sliver = [[0.0, 0.0, -100.0], [0.0, 1.0, -100.0], [0.0, 0.5, 100.0]];
        let (time, _) = box_triangle(v(-100.0, 0.0, 0.0), v(200.0, 0.0, 0.0), v(25.0, 25.0, 44.0), sliver).unwrap();
        assert!((time - 75.0 / 200.0).abs() < 1e-5, "{time}");
        assert!(box_overlaps_triangle(v(0.0, 20.0, 0.0), v(25.0, 25.0, 44.0), sliver));
    }

    #[test]
    fn box_overlaps_a_placed_mesh() {
        let wall = vec![[[0.0, -100.0, -100.0], [0.0, 100.0, -100.0], [0.0, 0.0, 100.0]]];
        let mesh = MeshCollision::placed(wall, v(500.0, 0.0, 0.0), p2_math::Rotator::ZERO, v(1.0, 1.0, 1.0), Vector::ZERO);
        let extent = v(25.0, 25.0, 44.0);
        assert!(mesh_box_overlaps([&mesh], v(480.0, 0.0, 0.0), extent));
        assert!(!mesh_box_overlaps([&mesh], v(470.0, 0.0, 0.0), extent));
        assert!(!mesh_box_overlaps([&mesh], v(-20.0, 0.0, 0.0), extent));
    }

    #[test]
    fn mesh_tree_finds_the_first_hit_of_a_scan() {
        let once: Vec<[[f32; 3]; 3]> = crate::tests::test_segments(400).into_iter().map(|(a, b)| [a.into(), b.into(), (a + v(40.0, 0.0, 40.0)).into()]).collect();
        let local: Vec<_> = once.iter().chain(&once).copied().collect();
        let mesh = MeshCollision::placed(local, Vector::ZERO, p2_math::Rotator::ZERO, v(1.0, 1.0, 1.0), Vector::ZERO);
        let extent = v(20.0, 20.0, 30.0);
        let mut hits = 0;
        for (start, end) in crate::tests::test_segments(1500) {
            let (min, max) = swept_bounds(start, end, extent);
            let delta = end - start;
            let scanned = mesh.triangles().iter().filter(|tri| overlaps(min, max, tri)).filter_map(|&tri| box_triangle(start, delta, extent, tri));
            let scan = nearest(scanned, start, end);
            assert_eq!(mesh_box_check([&mesh], start, end, extent), scan, "{start:?} -> {end:?}");
            hits += usize::from(scan.is_some());
        }
        assert!(hits > 0);
    }

    #[test]
    fn line_hits_report_their_material_slot() {
        let wall = |x: f32| vec![[[x, -100.0, -100.0], [x, 100.0, -100.0], [x, 0.0, 100.0]], [[x, -100.0, 200.0], [x, 100.0, 200.0], [x, 0.0, 300.0]]];
        let placed = |x: f32| MeshCollision::placed(wall(x), Vector::ZERO, p2_math::Rotator::ZERO, v(1.0, 1.0, 1.0), Vector::ZERO);
        let near = placed(100.0).with_slots(vec![3_u32, 5]);
        let far = placed(200.0);
        let (hit, mesh, triangle) = crate::mesh_line_check_at([&far, &near], v(0.0, 0.0, 250.0), v(300.0, 0.0, 250.0)).unwrap();
        assert_eq!((mesh, triangle, near.slot(triangle)), (1, 1, Some(5)));
        assert!((hit.time - 1.0 / 3.0).abs() < 1e-5, "{}", hit.time);
        assert_eq!(far.slot(0), None);
    }

    fn terrain_box_check_scan(terrain: &Terrain, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
        let (width, height) = (terrain.width, terrain.height);
        if terrain.vertices.len() != width * height || width < 2 || height < 2 {
            return None;
        }
        let (min, max) = swept_bounds(start, end, extent);
        let cells = |axis: usize, size: usize| {
            let cell = |v: f32| (v - terrain.location[axis]) / terrain.scale[axis] + (size / 2) as f32;
            let (a, b) = (cell(min[axis]), cell(max[axis]));
            let low = a.min(b).floor().max(0.0) as usize;
            let high = (a.max(b).ceil().max(0.0) as usize).min(size - 2);
            low..=high
        };
        let delta = end - start;
        let mut hits = Vec::new();
        for y in cells(1, height) {
            for x in cells(0, width) {
                for triangle in terrain.quad_triangles(x, y) {
                    let tri = triangle.map(|i| terrain.vertices[i]);
                    if overlaps(min, max, &tri)
                        && let Some(hit) = box_triangle(start, delta, extent, tri)
                    {
                        hits.push(hit);
                    }
                }
            }
        }
        nearest(hits.into_iter(), start, end)
    }

    #[test]
    fn terrain_box_walk_matches_the_scan() {
        let terrain = crate::tests::test_terrain();
        let mut hits = 0;
        for (i, (start, end)) in crate::tests::test_segments(2000).into_iter().enumerate() {
            let extent = v(5.0 + (i % 5) as f32 * 15.0, 5.0 + (i % 3) as f32 * 25.0, 10.0 + (i % 4) as f32 * 20.0);
            let walk = terrain_box_check(&terrain, start, end, extent);
            let scan = terrain_box_check_scan(&terrain, start, end, extent);
            match (walk, scan) {
                (None, None) => {}
                (Some(w), Some(s)) if (w.time - s.time).abs() < 1e-5 && w.normal == s.normal => hits += 1,
                _ => panic!("segment {i} {start:?} -> {end:?} extent {extent:?}: walk {walk:?}, scan {scan:?}"),
            }
        }
        assert!((200..1800).contains(&hits), "{hits} hits");
        let (start, end) = (crate::tests::at(-0.5, 2.5, 300.0), crate::tests::at(-0.5, 2.5, -300.0));
        assert!(terrain_box_check(&terrain, start, end, v(40.0, 10.0, 10.0)).is_some());
        assert!(terrain_box_check(&terrain, start, end, v(20.0, 10.0, 10.0)).is_none());
    }

    #[test]
    fn box_hits_a_slope_with_its_corner() {
        let slope = [[100.0, -100.0, 0.0], [100.0, 100.0, 0.0], [157.7, 0.0, 100.0]];
        let (time, normal) = box_triangle(v(0.0, 0.0, 60.0), v(100.0, 0.0, 0.0), v(20.0, 20.0, 50.0), slope).unwrap();
        assert!((time - 0.858).abs() < 0.01, "time {time}");
        assert!(normal.x < -0.5 && normal.z > 0.0, "normal {normal:?}");
        let pivot = v(100.0, 0.0, 0.0);
        let larger = slope.map(|p| <[f32; 3]>::from(pivot + (Vector::from(p) - pivot) * 10.0));
        let (larger_time, larger_normal) = box_triangle(v(0.0, 0.0, 60.0), v(100.0, 0.0, 0.0), v(20.0, 20.0, 50.0), larger).unwrap();
        assert!((larger_time - time).abs() < 1e-4, "{larger_time} {time}");
        assert!((larger_normal - normal).size() < 1e-4 && (normal.size() - 1.0).abs() < 1e-5, "{larger_normal:?} {normal:?}");
    }
}
