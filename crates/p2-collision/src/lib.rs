use p2_assets::{BspNode, Model, StaticMesh, Terrain};
use p2_math::{Rotator, Vector};

mod extent;
mod primitive;
mod sweep;
mod tree;
pub use extent::{BspHulls, box_check, point_check, point_solid};
pub use primitive::{Primitive, pull_back, pull_back_units, segment_cylinder, world_to_local};
pub use sweep::{box_overlaps_triangle, mesh_box_check, mesh_box_overlaps, terrain_box_check};
/// `NF_NotCsg | NF_IsNew`: nodes with these flags do not divide solid from empty space.
const NOT_CSG: u8 = 0x21;
const MAX_TREE_DEPTH: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub time: f32,
    pub location: Vector,
    pub normal: Vector,
}

/// At most one step per node, so a malformed tree whose children loop ends (`Model`'s fields are public;
/// `Model::validate` checks ranges only).
fn walk(model: &Model, point: Vector) -> impl Iterator<Item = (&BspNode, bool)> {
    let first = model.nodes.first().map(|n| (n, dot_plane(n.plane, point) >= 0.0));
    std::iter::successors(first, move |&(n, front)| {
        let next = model.nodes.get(child(n, front)?)?;
        Some((next, dot_plane(next.plane, point) >= 0.0))
    })
    .take(model.nodes.len())
}

pub fn point_zone(model: &Model, point: Vector) -> u8 {
    if model.zones.is_empty() {
        return 0;
    }
    walk(model, point).last().map_or(0, |(n, front)| n.zones[usize::from(front)])
}

pub fn point_leaf(model: &Model, point: Vector) -> Option<usize> {
    let leaf = walk(model, point).last().map_or(-1, |(n, front)| n.leaves[usize::from(front)]);
    usize::try_from(leaf).ok().filter(|&leaf| leaf < model.leaves.len())
}

pub fn box_zones(model: &Model, min: Vector, max: Vector) -> Vec<u8> {
    let (center, extent) = ((min + max) * 0.5, (max - min) * 0.5);
    let mut zones = Vec::new();
    let mut stack: Vec<usize> = if model.nodes.is_empty() { Vec::new() } else { vec![0] };
    let mut visits = 0;
    while let Some(i) = stack.pop().filter(|_| visits < model.nodes.len()) {
        visits += 1;
        let Some(n) = model.nodes.get(i) else { continue };
        let (dist, reach) = (dot_plane(n.plane, center), (0..3).map(|k| n.plane[k].abs() * extent[k]).sum::<f32>());
        for (front, touches) in [(false, dist - reach < 0.0), (true, dist + reach >= 0.0)] {
            match (touches, child(n, front)) {
                (false, _) => {}
                (true, Some(child)) => stack.push(child),
                (true, None) => {
                    let leaf = usize::try_from(n.leaves[usize::from(front)]).ok().and_then(|leaf| model.leaves.get(leaf));
                    zones.extend(leaf.and_then(|leaf| u8::try_from(leaf.zone).ok()));
                }
            }
        }
    }
    zones.sort_unstable();
    zones.dedup();
    zones
}

pub fn line_check(model: &Model, start: Vector, end: Vector) -> Option<Hit> {
    line_check_node(model, start, end).map(|(hit, _)| hit)
}

pub fn line_check_node(model: &Model, start: Vector, end: Vector) -> Option<(Hit, Option<usize>)> {
    if model.nodes.is_empty() {
        return None;
    }
    let (a, b) = (LinePoint { at: start, time: 0.0 }, LinePoint { at: end, time: 1.0 });
    let entry = Entry { normal: Vector::new(0.0, 0.0, 1.0), node: None };
    let (time, entry) = check(model, 0, model.root_outside, a, b, entry, model.nodes.len().min(MAX_TREE_DEPTH))?;
    Some((Hit { time, location: start.lerp(end, time), normal: entry.normal }, entry.node))
}

/// Whether the child on side `front` is outside, given the parent's state. Outside means empty space;
/// the space beyond the level (`RootOutside` false) is solid.
fn child_outside(node: &BspNode, front: bool, outside: bool) -> bool {
    let csg = node.num_vertices != 0 && node.node_flags & NOT_CSG == 0;
    if front { outside || csg } else { outside && !csg }
}

fn child(node: &BspNode, front: bool) -> Option<usize> {
    if front { node.front } else { node.back }
}

fn dot_plane(plane: [f32; 4], p: Vector) -> f32 {
    let [x, y, z, w] = plane;
    x * p.x + y * p.y + z * p.z - w
}

#[derive(Clone, Copy)]
struct LinePoint {
    at: Vector,
    time: f32,
}

#[derive(Clone, Copy)]
struct Entry {
    normal: Vector,
    node: Option<usize>,
}

fn check(model: &Model, index: usize, outside: bool, a: LinePoint, b: LinePoint, entry: Entry, depth: usize) -> Option<(f32, Entry)> {
    let node = model.nodes.get(index).filter(|_| depth > 0)?;
    let (da, db) = (dot_plane(node.plane, a.at), dot_plane(node.plane, b.at));
    let descend = |front: bool, a: LinePoint, b: LinePoint, entry: Entry| {
        let outside = child_outside(node, front, outside);
        match child(node, front) {
            Some(next) => check(model, next, outside, a, b, entry, depth - 1),
            None => (!outside).then_some((a.time, entry)),
        }
    };
    if da >= 0.0 && db >= 0.0 {
        return descend(true, a, b, entry);
    }
    if da < 0.0 && db < 0.0 {
        return descend(false, a, b, entry);
    }
    let t = da / (da - db);
    let mid = LinePoint { at: a.at.lerp(b.at, t), time: a.time + (b.time - a.time) * t };
    let near_front = da >= 0.0;
    let [x, y, z, _] = node.plane;
    let crossing = Entry { normal: if near_front { Vector::new(x, y, z) } else { Vector::new(-x, -y, -z) }, node: Some(index) };
    descend(near_front, a, mid, entry).or_else(|| descend(!near_front, mid, b, crossing))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sides {
    Both,
    FrontOnly,
}

fn terrain_ready(terrain: &Terrain) -> bool {
    terrain.vertices.len() == terrain.width * terrain.height && terrain.width >= 2 && terrain.height >= 2
}

/// `terrain.vertices` must be filled (rebuilt from the heightmap if the saved ones were stale). Of hits at the same time,
/// the quad first in heightmap order (row, then column, then triangle) wins.
pub fn terrain_line_check(terrain: &Terrain, start: Vector, end: Vector, sides: Sides) -> Option<Hit> {
    terrain_first_hit(terrain, start, end, [0.0; 2], TIE_SLACK, |triangle| {
        let [a, b, c] = triangle.map(|i| Vector::from(terrain.vertices[i]));
        if sides == Sides::FrontOnly && !terrain_front(terrain, [a, b, c], end - start) {
            return None;
        }
        let t = segment_triangle(start, end, a, b, c)?;
        Some((t, facing(triangle_normal(a, b, c), start, end)))
    })
}

pub(crate) fn terrain_front(terrain: &Terrain, [a, b, c]: [Vector; 3], delta: Vector) -> bool {
    let up = triangle_normal(a, c, b);
    let front = if terrain.inverted { -up } else { up };
    front.dot(delta) < -1e-4
}

fn terrain_first_hit(terrain: &Terrain, start: Vector, end: Vector, extent: [f32; 2], row_slack: f32, hit: impl Fn([usize; 3]) -> Option<(f32, Vector)>) -> Option<Hit> {
    if !terrain_ready(terrain) {
        return None;
    }
    let mut best: Option<(f32, (usize, usize, usize), Vector)> = None;
    for (entry, y, xs) in terrain_rows(terrain, start, end, extent) {
        if best.is_some_and(|(t, ..)| entry > t + row_slack) {
            break;
        }
        for x in xs {
            for (k, triangle) in terrain.quad_triangles(x, y).enumerate() {
                if let Some((t, normal)) = hit(triangle)
                    && best.is_none_or(|(best, key, _)| t.total_cmp(&best).then((y, x, k).cmp(&key)).is_lt())
                {
                    best = Some((t, (y, x, k), normal));
                }
            }
        }
    }
    best.map(|(time, _, normal)| Hit { time, location: start.lerp(end, time), normal })
}

/// Margin, in heightmap cells, by which [`terrain_rows`] widens a segment: quads it only touches along an edge or
/// at a corner are kept, as are hits rounding puts a hair outside their quad.
const CELL_MARGIN: f32 = 1e-3;

/// Line time past the best hit for which [`terrain_line_check`] still walks a row: a hit on an edge two rows share
/// ties, and the rows' entry times differ from it by rounding.
const TIE_SLACK: f32 = 1e-4;

fn terrain_rows(terrain: &Terrain, start: Vector, end: Vector, pad: [f32; 2]) -> impl Iterator<Item = (f32, usize, std::ops::RangeInclusive<usize>)> {
    let sizes = [terrain.width, terrain.height];
    let cell = |p: Vector| [0, 1].map(|k| (p[k] - terrain.location[k]) / terrain.scale[k] + (sizes[k] / 2) as f32);
    let ([ax, ay], [bx, by]) = (cell(start), cell(end));
    let [px, py] = [0, 1].map(|k| (pad[k] / terrain.scale[k]).abs() + CELL_MARGIN);
    let span = |low: f32, high: f32, size: usize| {
        let (low, high) = (low.floor().max(0.0), high.floor().min((size - 2) as f32));
        (low <= high).then_some(low as usize..=high as usize)
    };
    let finite = [ax, ay, bx, by, px, py].iter().all(|v| v.is_finite());
    let rows = span(ay.min(by) - py, ay.max(by) + py, terrain.height).filter(|_| finite);
    let (first, last) = rows.map_or((1, 0), |rows| (*rows.start(), *rows.end()));
    let (dx, dy) = (bx - ax, by - ay);
    (first..=last).map(move |i| if dy < 0.0 { first + last - i } else { i }).filter_map(move |y| {
        let (s0, s1) = if dy == 0.0 {
            (0.0, 1.0)
        } else {
            let (a, b) = ((y as f32 - py - ay) / dy, (y as f32 + 1.0 + py - ay) / dy);
            (a.min(b).max(0.0), a.max(b).min(1.0))
        };
        let (x0, x1) = (ax + s0 * dx, ax + s1 * dx);
        Some((s0, y, span(x0.min(x1) - px, x0.max(x1) + px, terrain.width)?))
    })
}

fn triangle_normal(a: Vector, b: Vector, c: Vector) -> Vector {
    let n = (b - a).cross(c - a);
    let length = n.size();
    if length > 0.0 { n / length } else { Vector::new(0.0, 0.0, 1.0) }
}

fn facing(normal: Vector, start: Vector, end: Vector) -> Vector {
    if normal.dot(end - start) > 0.0 { -normal } else { normal }
}

fn segment_triangle(start: Vector, end: Vector, a: Vector, b: Vector, c: Vector) -> Option<f32> {
    let direction = end - start;
    let (e1, e2) = (b - a, c - a);
    let p = direction.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-8 {
        return None;
    }
    let s = start - a;
    let u = s.dot(p) / det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = direction.dot(q) / det;
    if v >= 0.0 && u + v <= 1.0 {
        let t = e2.dot(q) / det;
        (0.0..=1.0).contains(&t).then_some(t)
    } else {
        None
    }
}
/// The simplified `collision_model` when the mesh has one and `UseSimpleBoxCollision` or
/// `UseSimpleLineCollision` asks; else `CollisionTriangles` if the tree has nodes; else nothing.
pub fn static_mesh_collision(mesh: &StaticMesh, collision_model: Option<&Model>, zero_extent: bool) -> (Vec<[[f32; 3]; 3]>, Vec<u32>) {
    let simple = if zero_extent { mesh.use_simple_line_collision } else { mesh.use_simple_box_collision };
    if let Some(model) = collision_model.filter(|_| simple) {
        return (model_triangles(model), Vec::new());
    }
    if mesh.collision_nodes == 0 {
        return (Vec::new(), Vec::new());
    }
    let corner = |i: u32| mesh.positions.get(i as usize).copied();
    let corners = |t: &[u32; 3]| Some([corner(t[0])?, corner(t[1])?, corner(t[2])?]);
    let triangles = mesh.collision_triangles.iter().filter_map(corners).collect();
    let slots = mesh.collision_triangles.iter().zip(&mesh.collision_materials).filter(|(t, _)| corners(t).is_some()).map(|(_, &slot)| slot).collect();
    (triangles, slots)
}

pub fn model_triangles(model: &Model) -> Vec<[[f32; 3]; 3]> {
    let mut triangles = Vec::new();
    for node in model.nodes.iter().filter(|n| n.num_vertices >= 3) {
        let verts = model.verts.get(node.vert_pool..node.vert_pool + usize::from(node.num_vertices)).unwrap_or_default();
        let Some(points) = verts.iter().map(|v| model.points.get(v.point).copied()).collect::<Option<Vec<_>>>() else { continue };
        triangles.extend(p2_assets::fan_triangles(points.len()).map(|t| t.map(|i| points[i])));
    }
    triangles
}

#[derive(Debug, Clone)]
pub struct MeshCollision {
    triangles: Vec<[[f32; 3]; 3]>,
    tree: tree::TriangleTree,
    slots: std::sync::Arc<[u32]>,
}

impl MeshCollision {
    pub fn new(mesh: &StaticMesh, location: Vector, rotation: Rotator, scale: Vector, pre_pivot: Vector) -> Self {
        let (mut local, mut slots) = (Vec::new(), Vec::new());
        for (slot, section) in mesh.sections.iter().enumerate() {
            let start = usize::from(section.first_index);
            let end = (start + usize::from(section.num_primitives) * 3).min(mesh.indices.len());
            for corners in mesh.indices.get(start..end).unwrap_or_default().as_chunks::<3>().0 {
                let [a, b, c] = corners.map(|i| mesh.positions.get(usize::from(i)).copied());
                if let (Some(a), Some(b), Some(c)) = (a, b, c) {
                    local.push([a, b, c]);
                    slots.push(slot as u32);
                }
            }
        }
        Self::placed(local, location, rotation, scale, pre_pivot).with_slots(slots)
    }

    pub fn with_slots(mut self, slots: impl Into<std::sync::Arc<[u32]>>) -> Self {
        let slots: std::sync::Arc<[u32]> = slots.into();
        if slots.len() == self.triangles.len() {
            self.slots = slots;
        }
        self
    }

    pub fn slot(&self, triangle: usize) -> Option<u32> {
        self.slots.get(triangle).copied()
    }

    pub fn placed(local: impl AsRef<[[[f32; 3]; 3]]>, location: Vector, rotation: Rotator, scale: Vector, pre_pivot: Vector) -> Self {
        let axes = rotation.axes();
        let world = |p: [f32; 3]| {
            let local = [0, 1, 2].map(|i| (p[i] - pre_pivot[i]) * scale[i]);
            [0, 1, 2].map(|c| location[c] + local[0] * axes[0][c] + local[1] * axes[1][c] + local[2] * axes[2][c])
        };
        let triangles: Vec<[[f32; 3]; 3]> = local.as_ref().iter().map(|t| t.map(world)).collect();
        let tree = tree::TriangleTree::new(&triangles);
        Self { triangles, tree, slots: std::sync::Arc::default() }
    }

    pub fn triangles(&self) -> &[[[f32; 3]; 3]] {
        &self.triangles
    }

    /// An empty mesh's box runs from `f32::MAX` to `f32::MIN`, so it touches nothing.
    pub fn bounds(&self) -> (Vector, Vector) {
        self.tree.root_box().map_or((Vector::new(f32::MAX, f32::MAX, f32::MAX), Vector::new(f32::MIN, f32::MIN, f32::MIN)), |(min, max)| (min.into(), max.into()))
    }

    fn touches(&self, min: Vector, max: Vector) -> bool {
        let (low, high) = self.bounds();
        (0..3).all(|i| low[i] <= max[i] && high[i] >= min[i])
    }
}

/// A tree over the boxes of a mesh list, for line checks that skip the meshes a segment does not come near.
#[derive(Debug, Clone, Default)]
pub struct MeshSet {
    tree: tree::TriangleTree,
}

impl MeshSet {
    pub fn new(meshes: &[MeshCollision]) -> Self {
        let boxes: Vec<[[f32; 3]; 2]> = meshes
            .iter()
            .map(|mesh| {
                let (min, max) = mesh.bounds();
                [min.into(), max.into()]
            })
            .collect();
        Self { tree: tree::TriangleTree::new(&boxes) }
    }

    /// The same answer as [`mesh_line_check_at`] over `meshes`, which must be the list the set was built from.
    pub fn line_check_at(&self, meshes: &[MeshCollision], start: Vector, end: Vector) -> Option<(Hit, usize, usize)> {
        let mut best = None;
        self.tree.along(start, end, 1.0, |index| earliest_in(&meshes[index], index, start, end, &mut best));
        found(best, start, end)
    }
}

pub fn mesh_line_check<'a>(meshes: impl IntoIterator<Item = &'a MeshCollision>, start: Vector, end: Vector) -> Option<Hit> {
    mesh_line_check_at(meshes, start, end).map(|(hit, ..)| hit)
}

/// Of hits at the same time, the first mesh's and in it the lowest triangle's wins.
pub fn mesh_line_check_at<'a>(meshes: impl IntoIterator<Item = &'a MeshCollision>, start: Vector, end: Vector) -> Option<(Hit, usize, usize)> {
    let mut best = None;
    for (index, mesh) in meshes.into_iter().enumerate() {
        earliest_in(mesh, index, start, end, &mut best);
    }
    found(best, start, end)
}

/// Keeps the smallest (time, mesh, triangle) in `best`, so mesh order does not matter; returns the next mesh's limit.
fn earliest_in(mesh: &MeshCollision, index: usize, start: Vector, end: Vector, best: &mut Option<(f32, usize, usize, Vector)>) -> f32 {
    mesh.tree.along(start, end, best.map_or(1.0, |(time, ..)| time), |triangle| {
        let [a, b, c] = mesh.triangles[triangle].map(Vector::from);
        if let Some(t) = segment_triangle(start, end, a, b, c)
            && best.is_none_or(|(time, first_mesh, first, _)| (t, index, triangle) < (time, first_mesh, first))
        {
            *best = Some((t, index, triangle, facing(triangle_normal(a, b, c), start, end)));
        }
        best.map_or(1.0, |(time, ..)| time)
    });
    best.map_or(1.0, |(time, ..)| time)
}

fn found(best: Option<(f32, usize, usize, Vector)>, start: Vector, end: Vector) -> Option<(Hit, usize, usize)> {
    best.map(|(time, index, triangle, normal)| (Hit { time, location: start.lerp(end, time), normal }, index, triangle))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn test_terrain() -> Terrain {
        let (width, height) = (9, 7);
        let mut terrain = Terrain {
            width,
            height,
            heightmap: 0,
            scale: Vector::new(64.0, 48.0, 64.0),
            location: Vector::new(100.0, -50.0, 20.0),
            vertices: Vec::new(),
            vertex_colors: Vec::new(),
            quad_visibility: vec![-1; (width * height).div_ceil(32)],
            edge_turn: vec![0; (width * height).div_ceil(32)],
            layers: Vec::new(),
            sector_size: 16,
            inverted: false,
            deco_layers: Vec::new(),
        };
        for quad in (0..width * height).filter(|quad| (quad % width + quad / width) % 2 == 0) {
            terrain.edge_turn[quad / 32] |= 1 << (quad % 32);
        }
        let hidden = 3 * width + 4;
        terrain.quad_visibility[hidden / 32] &= !(1 << (hidden % 32));
        let heights: Vec<u16> = (0..width * height)
            .map(|i| {
                let (x, y) = ((i % width) as f32, (i / width) as f32);
                (32767.0 + 300.0 * (x * 1.3).sin() + 200.0 * (y * 0.7).cos()) as u16
            })
            .collect();
        terrain.vertices = terrain.vertices_from_heights(&heights);
        terrain
    }

    pub(crate) fn at(cx: f32, cy: f32, z: f32) -> Vector {
        Vector::new(100.0 + (cx - 4.0) * 64.0, -50.0 + (cy - 3.0) * 48.0, 20.0 + z)
    }

    pub(crate) fn test_segments(count: usize) -> Vec<(Vector, Vector)> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut random = move |low: f32, high: f32| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            low + (high - low) * ((state >> 40) as f32 / 16_777_216.0)
        };
        (0..count)
            .map(|i| {
                let (za, zb) = (random(-150.0, 250.0), random(-250.0, 150.0));
                match i % 8 {
                    0 => (at(random(-2.0, 10.0), random(-2.0, 8.0), za), at(random(-2.0, 10.0), random(-2.0, 8.0), zb)),
                    1 => {
                        let (cx, cy) = (random(-1.0, 9.0), random(-1.0, 7.0));
                        let (cx, cy) = if i % 16 == 1 { (cx.floor(), cy.floor()) } else { (cx, cy) };
                        (at(cx, cy, 250.0), at(cx, cy, -250.0))
                    }
                    2 => {
                        let cx = random(-1.0, 10.0).floor();
                        (at(cx, random(-2.0, 8.0), za), at(cx, random(-2.0, 8.0), zb))
                    }
                    3 => {
                        let cy = random(-1.0, 8.0).floor();
                        (at(random(-2.0, 10.0), cy, za), at(random(-2.0, 10.0), cy, zb))
                    }
                    4 => {
                        let (cx, cy, n) = (random(-1.0, 9.0).floor(), random(-1.0, 7.0).floor(), random(1.0, 6.0).floor());
                        let ny = if random(0.0, 1.0) < 0.5 { n } else { -n };
                        (at(cx, cy, za), at(cx + n, cy + ny, zb))
                    }
                    5 => (at(random(-30.0, -1.5), random(-5.0, 10.0), za), at(random(-30.0, -1.5), random(-5.0, 10.0), zb)),
                    6 => {
                        let inside = at(random(0.0, 8.0), random(0.0, 6.0), za);
                        let outside = at(random(-40.0, 50.0), random(-40.0, 50.0), zb);
                        if i % 16 == 6 { (inside, outside) } else { (outside, inside) }
                    }
                    _ => {
                        let (cx, cy, z) = (random(0.0, 8.0), random(0.0, 6.0), random(-130.0, 150.0));
                        (at(cx, cy, z), at(cx + random(-0.01, 0.01), cy + random(-0.01, 0.01), z - random(0.0, 3.0)))
                    }
                }
            })
            .collect()
    }

    fn test_mesh_triangles() -> Vec<[[f32; 3]; 3]> {
        let mut triangles: Vec<[[f32; 3]; 3]> = test_segments(400).into_iter().map(|(a, b)| [a.into(), b.into(), (a + Vector::new(40.0, 0.0, 40.0)).into()]).collect();
        let terrain = test_terrain();
        for y in 0..terrain.height - 1 {
            for x in 0..terrain.width - 1 {
                for triangle in terrain.quad_triangles(x, y) {
                    triangles.push(triangle.map(|i| terrain.vertices[i]));
                }
            }
        }
        triangles.extend_from_within(..);
        triangles
    }

    fn mesh_line_check_scan(meshes: &[MeshCollision], start: Vector, end: Vector) -> Option<(Hit, usize, usize)> {
        let mut best: Option<(f32, Vector, usize, usize)> = None;
        for (index, mesh) in meshes.iter().enumerate() {
            for (triangle, tri) in mesh.triangles.iter().enumerate() {
                let [a, b, c] = tri.map(Vector::from);
                if let Some(t) = segment_triangle(start, end, a, b, c)
                    && best.is_none_or(|(best, ..)| t < best)
                {
                    best = Some((t, facing(triangle_normal(a, b, c), start, end), index, triangle));
                }
            }
        }
        best.map(|(time, normal, index, triangle)| (Hit { time, location: start.lerp(end, time), normal }, index, triangle))
    }

    #[test]
    fn line_walk_finds_the_first_hit_of_a_scan() {
        let one = Vector::new(1.0, 1.0, 1.0);
        let local = test_mesh_triangles();
        let straight = MeshCollision::placed(&local, Vector::ZERO, Rotator::ZERO, one, Vector::ZERO);
        let turned = MeshCollision::placed(&local, Vector::new(30.0, -20.0, 10.0), Rotator::new(1500, 6000, -900), one, Vector::ZERO);
        let sets = [vec![straight.clone()], vec![turned.clone()], vec![turned, straight]];
        let mut hits = 0;
        for meshes in &sets {
            for (start, end) in test_segments(4000) {
                let walk = mesh_line_check_at(meshes, start, end);
                let scan = mesh_line_check_scan(meshes, start, end);
                match (walk, scan) {
                    (None, None) => {}
                    (Some((w, wm, wt)), Some((s, sm, st))) if (w.time - s.time).abs() < 1e-5 && (w.time != s.time || (wm, wt) == (sm, st)) => hits += 1,
                    _ => panic!("{start:?} -> {end:?}: walk {walk:?}, scan {scan:?}"),
                }
            }
        }
        assert!(hits > 100, "{hits} hits");
    }

    #[test]
    fn a_mesh_set_finds_the_hit_of_the_mesh_list() {
        let one = Vector::new(1.0, 1.0, 1.0);
        let local = test_mesh_triangles();
        let mut meshes: Vec<MeshCollision> = local.chunks(37).map(|chunk| MeshCollision::placed(chunk, Vector::ZERO, Rotator::ZERO, one, Vector::ZERO)).collect();
        meshes.push(MeshCollision::placed(Vec::<[[f32; 3]; 3]>::new(), Vector::ZERO, Rotator::ZERO, one, Vector::ZERO));
        meshes.push(MeshCollision::placed(&local, Vector::new(30.0, -20.0, 10.0), Rotator::new(1500, 6000, -900), one, Vector::ZERO));
        meshes.extend_from_within(..);
        meshes.rotate_left(5);
        let set = MeshSet::new(&meshes);
        let (mut hits, mut late_twins) = (0, 0);
        for (start, end) in test_segments(4000) {
            let list = mesh_line_check_at(&meshes, start, end);
            assert_eq!(set.line_check_at(&meshes, start, end), list, "{start:?} -> {end:?}");
            if let Some((hit, index, _)) = list {
                hits += 1;
                late_twins += usize::from(mesh_line_check_at(&meshes[index + 1..], start, end).is_some_and(|(later, ..)| later.time == hit.time));
            }
        }
        assert!(hits > 100 && late_twins > 100, "{hits} hits, {late_twins} ties");
    }

    #[test]
    fn line_walk_keeps_grazing_hits() {
        let one = Vector::new(1.0, 1.0, 1.0);
        let turned = MeshCollision::placed(test_mesh_triangles(), Vector::ZERO, Rotator::new(1500, 6000, -900), one, Vector::ZERO);
        let mut hits = 0;
        for &tri in turned.triangles() {
            let mesh = MeshCollision::placed([tri], Vector::ZERO, Rotator::ZERO, one, Vector::ZERO);
            let [a, b, c] = tri.map(Vector::from);
            let (middle, along, normal) = ((a + b + c) * (1.0 / 3.0), (b - a) * 0.5, triangle_normal(a, b, c));
            let tilted = [0.0, 1e-4, 1e-2, 1.0].map(|tilt| (middle - along + normal * tilt, middle + along - normal * tilt));
            for (start, end) in tilted.into_iter().chain([(a + normal * 50.0, a - normal * 50.0)]) {
                let walk = mesh_line_check_at([&mesh], start, end);
                assert_eq!(walk, mesh_line_check_scan(std::slice::from_ref(&mesh), start, end), "{start:?} -> {end:?}");
                hits += usize::from(walk.is_some());
            }
        }
        assert!(hits > 100, "{hits} hits");
    }

    fn terrain_line_check_scan(terrain: &Terrain, start: Vector, end: Vector) -> Option<Hit> {
        let (width, height) = (terrain.width, terrain.height);
        if terrain.vertices.len() != width * height || width < 2 || height < 2 {
            return None;
        }
        let cell = |p: Vector, axis: usize, size: usize| {
            (p[axis] - terrain.location[axis]) / terrain.scale[axis] + (size / 2) as f32
        };
        let range = |axis: usize, size: usize| {
            let (a, b) = (cell(start, axis, size), cell(end, axis, size));
            let low = (a.min(b) - CELL_MARGIN).floor().max(0.0) as usize;
            let high = (a.max(b).ceil().max(0.0) as usize).min(size - 2);
            low..=high
        };
        let mut best: Option<(f32, Vector)> = None;
        for y in range(1, height) {
            for x in range(0, width) {
                for triangle in terrain.quad_triangles(x, y) {
                    let [a, b, c] = triangle.map(|i| Vector::from(terrain.vertices[i]));
                    if let Some(t) = segment_triangle(start, end, a, b, c)
                        && best.is_none_or(|(best, _)| t < best)
                    {
                        best = Some((t, facing(triangle_normal(a, b, c), start, end)));
                    }
                }
            }
        }
        best.map(|(time, normal)| Hit { time, location: start.lerp(end, time), normal })
    }

    #[test]
    fn terrain_walk_matches_the_scan() {
        let terrain = test_terrain();
        let mut hits = 0;
        for (i, (start, end)) in test_segments(4000).into_iter().enumerate() {
            let (walk, scan) = (terrain_line_check(&terrain, start, end, Sides::Both), terrain_line_check_scan(&terrain, start, end));
            match (walk, scan) {
                (None, None) => {}
                (Some(w), Some(s)) if (w.time - s.time).abs() < 1e-5 && w.normal == s.normal => hits += 1,
                _ => panic!("segment {i} {start:?} -> {end:?}: walk {walk:?}, scan {scan:?}"),
            }
        }
        assert!((400..3600).contains(&hits), "{hits} hits");
    }

    #[test]
    fn terrain_rows_follow_the_line() {
        let terrain = test_terrain();
        let rows: Vec<_> = terrain_rows(&terrain, at(2.5, 3.5, 300.0), at(2.5, 3.5, -300.0), [0.0; 2]).collect();
        assert_eq!(rows, vec![(0.0, 3, 2..=2)]);
        assert!(terrain_rows(&terrain, at(-5.0, 1.0, 0.0), at(-1.5, 6.0, 0.0), [0.0; 2]).next().is_none());
        assert!(terrain_rows(&terrain, at(1.0, 7.5, 0.0), at(6.0, 9.0, 0.0), [0.0; 2]).next().is_none());
        let diagonal: usize = terrain_rows(&terrain, at(0.2, 0.1, 0.0), at(7.9, 5.9, 0.0), [0.0; 2]).map(|(_, _, xs)| xs.count()).sum();
        assert!(diagonal <= 18, "{diagonal} quads");
        let back: Vec<usize> = terrain_rows(&terrain, at(7.9, 5.9, 0.0), at(0.2, 0.1, 0.0), [0.0; 2]).map(|(_, y, _)| y).collect();
        assert_eq!(back, [5, 4, 3, 2, 1, 0]);
    }
}
