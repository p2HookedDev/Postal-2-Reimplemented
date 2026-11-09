//! Box queries against level BSP. The box is tested against the convex collision hulls stored per
//! solid leaf, not against the leaves themselves.

use p2_assets::{BspNode, Model};

use p2_math::Vector;

use crate::{Hit, MAX_TREE_DEPTH, child, child_outside, dot_plane, line_check};

/// The engine stops reading a hull's node list after this many planes.
const MAX_HULL_PLANES: usize = 64;
/// Hull entry flag: use the node's plane flipped.
const HULL_FLIPPED: i32 = 0x4000_0000;
/// Box extent is scaled by this while walking the tree (a conservative margin; hull tests use the exact extent).
const WALK_SCALE: f32 = 1.1;

type Plane = [f32; 4];

pub fn box_check(model: &Model, hulls: &BspHulls, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
    if extent == Vector::ZERO {
        return line_check(model, start, end);
    }
    if model.nodes.is_empty() {
        return None;
    }
    let mut sweep = Sweep { model, hulls, start, end, extent, walk: extent * WALK_SCALE, best: None, steps: model.nodes.len() };
    sweep.node(0, Some(0), model.root_outside, MAX_TREE_DEPTH);
    let (time, normal) = sweep.best?;
    // Pull the hit back along the sweep: 0.1 of the distance, at least 0.1 and at most 4 units.
    let dist = (end - start).size();
    let back = if 0.1 / dist > 0.1 {
        0.1 / dist
    } else if 4.0 / dist > 0.1 {
        0.1
    } else {
        4.0 / dist
    };
    let time = (time - back).clamp(0.0, 1.0);
    // The engine reports "no hit" when the clamped time is exactly 1.
    (time != 1.0).then(|| Hit { time, location: start.lerp(end, time), normal })
}

/// The hit's `location` is `point` pushed out of the nearest overlapping plane (×1.02), `normal` that plane's normal;
/// `time` is 0. Zero extent tests the point alone (solid space, with points on a plane counted behind it).
pub fn point_check(model: &Model, hulls: &BspHulls, point: Vector, extent: Vector) -> Option<Hit> {
    let blocked = Hit { time: 0.0, location: point, normal: Vector::ZERO };
    if model.nodes.is_empty() || extent == Vector::ZERO {
        return point_solid(model, point).then_some(blocked);
    }
    let mut overlap = Overlap { model, hulls, point, extent, walk: extent * WALK_SCALE, nearest: 100_000.0, hit: blocked, steps: model.nodes.len() };
    (!overlap.node(0, Some(0), model.root_outside, MAX_TREE_DEPTH)).then_some(overlap.hit)
}

/// [`point_check`] with zero extent: whether the point is in solid space, points on a plane counted behind it.
pub fn point_solid(model: &Model, point: Vector) -> bool {
    let mut outside = model.root_outside;
    let mut node = Some(0);
    for _ in 0..model.nodes.len() {
        let Some(n) = node.and_then(|index| model.nodes.get(index)) else { break };
        let front = dot_plane(n.plane, point) > 0.0;
        outside = child_outside(n, front, outside);
        node = child(n, front);
    }
    !outside
}

fn radius(plane: Plane, extent: Vector) -> f32 {
    (plane[0] * extent[0]).abs() + (plane[1] * extent[1]).abs() + (plane[2] * extent[2]).abs()
}

struct Sweep<'a> {
    model: &'a Model,
    hulls: &'a BspHulls,
    start: Vector,
    end: Vector,
    extent: Vector,
    walk: Vector,
    best: Option<(f32, Vector)>,
    steps: usize,
}

impl Sweep<'_> {
    fn node(&mut self, mut parent: usize, mut node: Option<usize>, mut outside: bool, depth: usize) {
        let model = self.model;
        while let Some(index) = node {
            let Some(n) = model.nodes.get(index).filter(|_| self.steps > 0 && depth > 0) else { return };
            self.steps -= 1;
            let r = radius(n.plane, self.walk);
            let (ds, de) = (dot_plane(n.plane, self.start), dot_plane(n.plane, self.end));
            let back = ds <= r || de <= r;
            let front = ds >= -r || de >= -r;
            let near_front = ds >= de;
            let (near, far) = if near_front { (front, back) } else { (back, front) };
            if near {
                self.node(index, child(n, near_front), child_outside(n, near_front, outside), depth - 1);
            }
            if !far {
                return;
            }
            parent = index;
            node = child(n, !near_front);
            outside = child_outside(n, !near_front, outside);
        }
        if outside {
            return;
        }
        let Some(planes) = self.hulls.planes(parent) else { return };
        let mut clip = Clip { t0: -1.0, t1: self.best.map_or(2.0, |(time, _)| time), normal: Vector::ZERO };
        let (start, end, extent) = (self.start, self.end, self.extent);
        if planes.iter().all(|&plane| clip.plane(plane, start, end, extent))
            && clip.t0 > -1.0
            && clip.t1 > clip.t0
            && clip.t1 > 0.0
        {
            self.best = Some((clip.t0, clip.normal));
        }
    }
}

struct Clip {
    t0: f32,
    t1: f32,
    normal: Vector,
}

impl Clip {
    fn plane(&mut self, plane: Plane, start: Vector, end: Vector, extent: Vector) -> bool {
        let r = radius(plane, extent);
        let (d1, d2) = (dot_plane(plane, start), dot_plane(plane, end));
        let mut num = d1 - r;
        // Starting within the box's reach of the plane but not behind it, moving in: enter at once.
        if d1 > d2 && num >= -r && num < 0.0 {
            num = 0.0;
        }
        let den = d1 - d2;
        let t = num / den;
        if den < -1e-5 {
            if t < self.t1 {
                self.t1 = t;
            }
        } else if den > 1e-5 {
            if t > self.t0 {
                self.t0 = t;
                self.normal = normal(plane);
            }
        } else if d1 > r && d2 > r {
            return false;
        }
        self.t1 > self.t0
    }
}

struct Overlap<'a> {
    model: &'a Model,
    hulls: &'a BspHulls,
    point: Vector,
    extent: Vector,
    walk: Vector,
    nearest: f32,
    hit: Hit,
    steps: usize,
}

impl Overlap<'_> {
    fn node(&mut self, mut parent: usize, mut node: Option<usize>, mut outside: bool, depth: usize) -> bool {
        let model = self.model;
        let mut free = true;
        while let Some(index) = node {
            let Some(n) = model.nodes.get(index).filter(|_| self.steps > 0 && depth > 0) else { return free };
            self.steps -= 1;
            let r = radius(n.plane, self.walk);
            let d = dot_plane(n.plane, self.point);
            if d > -r {
                free &= self.node(index, n.front, child_outside(n, true, outside), depth - 1);
            }
            parent = index;
            node = n.back;
            outside = child_outside(n, false, outside);
            if d > r {
                return free;
            }
        }
        if outside {
            return free;
        }
        let hulls = self.hulls;
        let blocked = hulls.planes(parent).is_some_and(|planes| planes.iter().all(|&plane| self.plane(plane)));
        if blocked { false } else { free }
    }

    fn plane(&mut self, plane: Plane) -> bool {
        let r = radius(plane, self.extent);
        let d = dot_plane(plane, self.point);
        if d > 0.0 && d < self.nearest && d < r {
            self.nearest = d;
            let push = (r - d) * 1.02;
            self.hit.location = self.point + normal(plane) * push;
            self.hit.normal = normal(plane);
        }
        r > d
    }
}

/// Every node's collision hull planes, built once per `Model`, in the engine's test order: the hull planes, the six
/// bounding box planes (0.1 outside the box on the −X, −Y, −Z, +Z sides, 0.1 inside on +X and +Y), then edge bevels.
#[derive(Default)]
pub struct BspHulls {
    planes: Vec<Plane>,
    /// Per node: the `planes` range, `None` if the node has no hull or its data is out of range.
    nodes: Vec<Option<(usize, usize)>>,
}

impl BspHulls {
    pub fn new(model: &Model) -> Self {
        let mut hulls = Self { planes: Vec::new(), nodes: Vec::with_capacity(model.nodes.len()) };
        let (mut planes, mut signs) = (Vec::new(), Vec::new());
        for node in &model.nodes {
            let range = hull_planes(model, node, &mut planes, &mut signs).map(|()| {
                let start = hulls.planes.len();
                hulls.planes.extend_from_slice(&planes);
                (start, hulls.planes.len())
            });
            hulls.nodes.push(range);
        }
        hulls
    }

    fn planes(&self, node: usize) -> Option<&[Plane]> {
        let (start, end) = (*self.nodes.get(node)?)?;
        self.planes.get(start..end)
    }
}

/// Reads `Model::leaf_hulls` from the node's `collision_bound`: node indices (bit 30 = flipped) up to −1, then the
/// bounding box as six floats. `None` if the node has no hull or the data is out of range.
fn hull_planes(model: &Model, node: &BspNode, planes: &mut Vec<Plane>, signs: &mut Vec<u8>) -> Option<()> {
    const EPS: f64 = 0.1;
    let first = node.collision_bound?;
    let list = model.leaf_hulls.get(first..)?;
    planes.clear();
    // Per plane: bit 0 / 1 = normal X negative / positive, bits 2 / 3 for Y, bits 4 / 5 for Z.
    signs.clear();
    for &entry in list.iter().take_while(|&&e| e != -1).take(MAX_HULL_PLANES) {
        let index = usize::try_from(entry & !HULL_FLIPPED).ok()?;
        let mut plane = model.nodes.get(index)?.plane;
        if entry & HULL_FLIPPED != 0 {
            plane = plane.map(|v| -v);
        }
        let sign = |v: f32, negative: u8| {
            if v < 0.0 {
                negative
            } else if v > 0.0 {
                negative << 1
            } else {
                0
            }
        };
        signs.push(sign(plane[0], 1) | sign(plane[1], 4) | sign(plane[2], 16));
        planes.push(plane);
    }
    let count = planes.len();
    let bounds = list.get(count + 1..count + 7)?;
    let float = |i: usize| f64::from(f32::from_bits(bounds[i] as u32));
    let (min, max) = ([float(0), float(1), float(2)], [float(3), float(4), float(5)]);
    planes.extend([
        [0.0, 0.0, -1.0, (EPS - min[2]) as f32],
        [0.0, 0.0, 1.0, (max[2] + EPS) as f32],
        [-1.0, 0.0, 0.0, (EPS - min[0]) as f32],
        [1.0, 0.0, 0.0, (max[0] - EPS) as f32],
        [0.0, -1.0, 0.0, (EPS - min[1]) as f32],
        [0.0, 1.0, 0.0, (max[1] - EPS) as f32],
    ]);
    // Bevels: for two hull planes whose normals point opposite ways along an axis, the plane through their
    // common edge that contains the axis, facing like the later plane.
    for i in 0..count {
        for j in 0..i {
            let either = signs[i] | signs[j];
            for axis in 0..3 {
                if either >> (2 * axis) & 3 == 3
                    && let Some(plane) = bevel(axis, planes[i], planes[j])
                {
                    planes.push(plane);
                }
            }
        }
    }
    Some(())
}

fn normal(plane: Plane) -> Vector {
    Vector::new(plane[0], plane[1], plane[2])
}

fn bevel(axis: usize, a: Plane, b: Plane) -> Option<Plane> {
    let mut unit = Vector::ZERO;
    unit[axis] = 1.0;
    let (na, nb) = (normal(a), normal(b));
    if unit.cross(na).dot(unit.cross(nb)) <= 0.001 {
        return None;
    }
    let (point, direction) = intersect_planes(a, b);
    let n = unit.cross(direction);
    let length = n.size();
    let mut n = n / length;
    if na.dot(n) < 0.0 {
        n = -n;
    }
    Some([n.x, n.y, n.z, point.dot(n)])
}

fn intersect_planes(a: Plane, b: Plane) -> (Vector, Vector) {
    let (na, nb) = (normal(a), normal(b));
    let d = na.cross(nb);
    let dd = d.dot(d);
    if dd < 1e-6 {
        return (Vector::ZERO, Vector::ZERO);
    }
    let (p, q) = (nb.cross(d), d.cross(na));
    let point = [0, 1, 2].map(|i| (a[3] * p[i] + b[3] * q[i]) / dd).into();
    let length = dd.sqrt();
    (point, d / length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor() -> Model {
        let node = BspNode {
            plane: [0.0, 0.0, 1.0, 0.0],
            node_flags: 0,
            vert_pool: 0,
            surface: 0,
            back: None,
            front: None,
            coplanar: None,
            collision_bound: Some(0),
            render_bound: None,
            zones: [0, 0],
            num_vertices: 4,
            leaves: [0, 0],
            section: None,
            first_vertex: 0,
            light_map: None,
        };
        let bounds = [-1000.0_f32, -1000.0, -1000.0, 1000.0, 1000.0, 0.0].map(|v| v.to_bits() as i32);
        let mut leaf_hulls = vec![0, -1];
        leaf_hulls.extend(bounds);
        Model {
            vectors: Vec::new(),
            points: Vec::new(),
            nodes: vec![node],
            surfaces: Vec::new(),
            verts: Vec::new(),
            num_shared_sides: 0,
            zones: Vec::new(),
            polys: 0,
            bounds: Vec::new(),
            leaf_hulls,
            leaves: Vec::new(),
            lights: Vec::new(),
            root_outside: false,
            linked: false,
            sections: Vec::new(),
            light_maps: Vec::new(),
            light_map_textures: Vec::new(),
        }
    }

    fn close(a: Vector, b: Vector) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3)
    }

    const fn v(x: f32, y: f32, z: f32) -> Vector {
        Vector::new(x, y, z)
    }

    #[test]
    fn looping_tree_walks_end() {
        let mut model = floor();
        (model.nodes[0].front, model.nodes[0].back) = (Some(0), Some(0));
        let (above, below) = (v(0.0, 0.0, 100.0), v(0.0, 0.0, -100.0));
        crate::point_zone(&model, above);
        crate::point_leaf(&model, above);
        crate::box_zones(&model, below, above);
        line_check(&model, above, below);
        let hulls = BspHulls::new(&model);
        box_check(&model, &hulls, above, below, v(10.0, 10.0, 10.0));
        point_check(&model, &hulls, above, Vector::ZERO);
        point_check(&model, &hulls, above, v(10.0, 10.0, 10.0));
    }

    #[test]
    fn a_box_is_in_the_zones_of_the_leaves_it_touches() {
        let mut model = floor();
        model.nodes[0].leaves = [0, 1];
        model.leaves = [1, 2].map(|zone| p2_assets::Leaf { zone, permeating: 0, volumetric: 0, visible_zones: 0 }).to_vec();
        assert_eq!(crate::box_zones(&model, v(0.0, 0.0, 10.0), v(5.0, 5.0, 20.0)), [2]);
        assert_eq!(crate::box_zones(&model, v(0.0, 0.0, -20.0), v(5.0, 5.0, -10.0)), [1]);
        assert_eq!(crate::box_zones(&model, v(0.0, 0.0, -10.0), v(5.0, 5.0, 10.0)), [1, 2]);
        model.nodes[0].leaves = [-1, 1];
        assert_eq!(crate::box_zones(&model, v(0.0, 0.0, -10.0), v(5.0, 5.0, 10.0)), [2]);
    }

    #[test]
    fn box_lands_on_floor() {
        let model = floor();
        let hulls = BspHulls::new(&model);
        let hit = box_check(&model, &hulls, v(0.0, 0.0, 100.0), v(0.0, 0.0, -100.0), v(10.0, 10.0, 10.0)).unwrap();
        assert!((hit.time - 0.43).abs() < 1e-5, "{hit:?}");
        assert!(close(hit.location, v(0.0, 0.0, 14.0)), "{hit:?}");
        assert_eq!(hit.normal, v(0.0, 0.0, 1.0));
        let hit = box_check(&model, &hulls, v(0.0, 0.0, 20.0), v(0.0, 0.0, 0.0), v(10.0, 10.0, 10.0)).unwrap();
        assert!(close(hit.location, v(0.0, 0.0, 12.0)), "{hit:?}");
        assert!(box_check(&model, &hulls, v(0.0, 0.0, 100.0), v(0.0, 0.0, 20.0), v(10.0, 10.0, 10.0)).is_none());
        assert!(box_check(&model, &hulls, v(0.0, 0.0, 20.0), v(0.0, 0.0, 100.0), v(10.0, 10.0, 10.0)).is_none());
    }

    #[test]
    fn box_hits_hull_bounds() {
        let model = floor();
        let hulls = BspHulls::new(&model);
        let hit = box_check(&model, &hulls, v(2000.0, 0.0, -50.0), v(0.0, 0.0, -50.0), v(10.0, 10.0, 10.0)).unwrap();
        assert!(close(hit.location, v(1013.9, 0.0, -50.0)), "{hit:?}");
        assert_eq!(hit.normal, v(1.0, 0.0, 0.0));
        assert!(box_check(&model, &hulls, v(2000.0, 0.0, 100.0), v(-2000.0, 0.0, 100.0), v(10.0, 10.0, 10.0)).is_none());
    }

    #[test]
    fn point_overlap() {
        let model = floor();
        let hulls = BspHulls::new(&model);
        let hit = point_check(&model, &hulls, v(0.0, 0.0, 5.0), v(10.0, 10.0, 10.0)).unwrap();
        assert!(close(hit.location, v(0.0, 0.0, 10.202)), "{hit:?}");
        assert_eq!(hit.normal, v(0.0, 0.0, 1.0));
        assert!(point_check(&model, &hulls, v(0.0, 0.0, 20.0), v(10.0, 10.0, 10.0)).is_none());
        assert!(point_check(&model, &hulls, v(0.0, 0.0, 1.0), Vector::ZERO).is_none());
        assert!(point_check(&model, &hulls, v(0.0, 0.0, -1.0), Vector::ZERO).is_some());
    }

    #[test]
    fn bevel_between_slopes() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let a = [-s, 0.0, s, s];
        let b = [s, 0.0, s, s];
        let plane = bevel(0, a, b).unwrap();
        assert!(close(normal(plane), v(0.0, 0.0, 1.0)), "{plane:?}");
        assert!((plane[3] - 1.0).abs() < 1e-5, "{plane:?}");
    }
}
