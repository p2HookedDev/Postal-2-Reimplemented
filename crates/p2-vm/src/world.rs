use std::sync::Arc;

use p2_assets::{Model, Terrain};
use p2_collision::{BspHulls, MeshCollision};
use p2_math::Vector;
use p2_package::PackageId;

use rustc_hash::FxHashMap;

use crate::{LoaderRef, ObjectId, Prop, Value, Vm};

mod build;
mod tracing;

struct WorldShape {
    shape: Arc<MeshCollision>,
    model: bool,
    actor: ObjectId,
}

const GRID_CELL: f32 = 512.0;
const GRID_MAX_CELLS: i64 = 256;

#[derive(Default)]
struct ShapeGrid {
    cells: FxHashMap<(i32, i32), Vec<usize>>,
    wide: Vec<usize>,
    count: usize,
}

pub(crate) struct LevelMesh {
    pub(crate) vertices: Vec<[f32; 3]>,
    pub(crate) triangles: Vec<[u32; 3]>,
}

fn grid_cells(min: Vector, max: Vector) -> Option<(std::ops::RangeInclusive<i32>, std::ops::RangeInclusive<i32>)> {
    if ![min.x, min.y, min.z, max.x, max.y, max.z].iter().all(|v| v.is_finite()) || min.x > max.x || min.y > max.y {
        return None;
    }
    let cell = |v: f32| (v / GRID_CELL).floor() as i32;
    let (x0, x1, y0, y1) = (cell(min[0]), cell(max[0]), cell(min[1]), cell(max[1]));
    let span = |low: i32, high: i32| i64::from(high) - i64::from(low) + 1;
    (span(x0, x1).saturating_mul(span(y0, y1)) <= GRID_MAX_CELLS).then_some((x0..=x1, y0..=y1))
}

impl ShapeGrid {
    fn new(shapes: &[WorldShape]) -> Self {
        let mut grid = Self { count: shapes.len(), ..Self::default() };
        for (index, world) in shapes.iter().enumerate() {
            let (min, max) = world.shape.bounds();
            match grid_cells(min, max) {
                Some((xs, ys)) => {
                    for x in xs {
                        for y in ys.clone() {
                            grid.cells.entry((x, y)).or_default().push(index);
                        }
                    }
                }
                None => grid.wide.push(index),
            }
        }
        grid
    }

    fn near_segment(&self, start: Vector, end: Vector, pad: f32, scratch: &mut NearScratch) {
        let NearScratch { found, stamps, query } = scratch;
        found.clear();
        if ![start.x, start.y, start.z, end.x, end.y, end.z, pad].iter().all(|v| v.is_finite()) {
            found.extend(0..self.count);
            return;
        }
        if stamps.len() < self.count {
            stamps.resize(self.count, 0);
        }
        *query = query.wrapping_add(1);
        if *query == 0 {
            stamps.fill(0);
            *query = 1;
        }
        let cells = crate::hash::segment_cells([start.x, start.y], [end.x, end.y], pad, GRID_CELL).filter_map(|c| self.cells.get(&c));
        for &index in self.wide.iter().chain(cells.flatten()) {
            if stamps[index] != *query {
                stamps[index] = *query;
                found.push(index);
            }
        }
        found.sort_unstable();
    }

    /// The queries run with `&self`, also from parallel passes, so the scratch is per thread; a nested query gets a fresh
    /// one.
    fn with_near<R>(&self, start: Vector, end: Vector, pad: f32, f: impl FnOnce(&[usize]) -> R) -> R {
        let mut scratch = NEAR_SCRATCH.take();
        self.near_segment(start, end, pad, &mut scratch);
        let result = f(&scratch.found);
        NEAR_SCRATCH.set(scratch);
        result
    }
}

#[derive(Default)]
struct NearScratch {
    found: Vec<usize>,
    stamps: Vec<u32>,
    query: u32,
}

thread_local! {
    static NEAR_SCRATCH: std::cell::Cell<NearScratch> = std::cell::Cell::default();
}

#[derive(Default)]
pub(crate) struct World {
    map: Option<PackageId>,
    model: Option<Model>,
    hulls: BspHulls,
    zone_actors: Vec<Option<ObjectId>>,
    terrains: Vec<Terrain>,
    boxes: Vec<WorldShape>,
    lines: Vec<WorldShape>,
    box_grid: ShapeGrid,
    line_grid: ShapeGrid,
    static_actors: Vec<bool>,
    mesh_materials: FxHashMap<ObjectId, Option<(PackageId, Arc<[i32]>)>>,
    pub(crate) primitives: crate::collision::Primitives,
    pub(crate) frame: u64,
}

impl World {
    pub(crate) fn is_static(&self, actor: ObjectId) -> bool {
        self.static_actors.get(actor.0 as usize).copied().unwrap_or_default()
    }
}

impl<L: LoaderRef> Vm<L> {
    pub fn velocity(&self, actor: ObjectId) -> Vector {
        self.store.vector(actor, Prop::Velocity)
    }

    /// Retail divides an emitter's step by `TimeDilation` when its `Owner` is a player (unverified),
    /// so the player's own effects run in real time.
    pub fn particle_time_scale(&self, actor: ObjectId) -> f32 {
        let player_owned = self.store.object_of(actor, Prop::Owner).is_some_and(|owner| self.is_player(owner));
        let dilation = self.level.map_or(1.0, |level| self.store.float_of(level, Prop::TimeDilation));
        if player_owned && dilation > 0.0 { 1.0 / dilation } else { 1.0 }
    }

    pub fn drag_actors(&self) -> Vec<(Vector, Vector, f32, f32)> {
        const FT_DRAG_ALONG: u8 = 1;
        let float = |actor, name: Prop| match self.store.get(actor, name) {
            Some(Value::Float(value)) => value,
            _ => 0.0,
        };
        self.actors()
            .filter(|&actor| !self.store.is_deleted(actor) && self.store.get(actor, Prop::ForceType) == Some(Value::Byte(FT_DRAG_ALONG)))
            .filter(|&actor| float(actor, Prop::ForceRadius) != 0.0)
            .map(|actor| (self.store.location(actor), self.velocity(actor), float(actor, Prop::ForceRadius), float(actor, Prop::ForceScale)))
            .collect()
    }

    pub fn zone_at(&self, point: Vector) -> Option<ObjectId> {
        self.region_at(point).0
    }

    pub(crate) fn region_at(&self, point: Vector) -> (Option<ObjectId>, i32, u8) {
        let Some(model) = self.world.model.as_ref() else { return (self.level(), -1, 0) };
        let zone = p2_collision::point_zone(model, point);
        let leaf = p2_collision::point_leaf(model, point).and_then(|leaf| i32::try_from(leaf).ok()).unwrap_or(-1);
        (self.world.zone_actors.get(usize::from(zone)).copied().flatten().or_else(|| self.level()), leaf, zone)
    }

    pub(crate) fn has_zones(&self) -> bool {
        self.world.model.as_ref().is_some_and(|model| !model.zones.is_empty())
    }
}

pub(crate) mod trace {
    pub const PAWNS: u32 = 0x1;
    pub const MOVERS: u32 = 0x2;
    pub const LEVEL: u32 = 0x4;
    pub const VOLUMES: u32 = 0x8;
    pub const OTHERS: u32 = 0x10;
    pub const ONLY_PROJ_ACTOR: u32 = 0x20;
    pub const BLOCKING: u32 = 0x40;
    pub const LEVEL_GEOMETRY: u32 = 0x80;
    pub const WORLD: u32 = MOVERS | LEVEL | LEVEL_GEOMETRY;
    pub const PROJ_TARGETS: u32 = 0xbf;
    pub const ALL_COLLIDING: u32 = 0x9f;
}

fn face_along(positions: &[[f32; 3]], triangles: &mut [[u32; 3]], normal: Vector) {
    for triangle in triangles {
        let [a, b, c] = triangle.map(|i| Vector::from(positions[i as usize]));
        if (b - a).cross(c - a).dot(normal) < 0.0 {
            triangle.swap(1, 2);
        }
    }
}

fn brush_corners(model: &Model) -> impl Iterator<Item = [f32; 3]> + '_ {
    model.nodes.iter().flat_map(move |node| {
        (node.vert_pool..node.vert_pool + usize::from(node.num_vertices)).filter_map(move |i| model.verts.get(i).and_then(|v| model.points.get(v.point)).copied())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fans_face_along_the_node_plane() {
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut triangles = [[0, 1, 2]];
        face_along(&positions, &mut triangles, [0.0, 0.0, 1.0].into());
        assert_eq!(triangles, [[0, 1, 2]]);
        face_along(&positions, &mut triangles, [0.0, 0.0, -1.0].into());
        assert_eq!(triangles, [[0, 2, 1]]);
    }

    #[test]
    fn brush_corners_are_the_nodes_pool_points() {
        let node = |vert_pool: usize, num_vertices: u8| p2_assets::BspNode {
            plane: [0.0, 0.0, 1.0, 0.0],
            node_flags: 0,
            vert_pool,
            surface: 0,
            back: None,
            front: None,
            coplanar: None,
            collision_bound: None,
            render_bound: None,
            zones: [0; 2],
            num_vertices,
            leaves: [-1; 2],
            section: None,
            first_vertex: 0,
            light_map: None,
        };
        let vert = |point: usize| p2_assets::Vert { point, side: None };
        let model = Model {
            vectors: Vec::new(),
            points: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [9.0; 3], [0.0, 0.0, 1.0]],
            nodes: vec![node(0, 3), node(4, 2)],
            surfaces: Vec::new(),
            verts: vec![vert(0), vert(1), vert(2), vert(3), vert(4), vert(0), vert(7)],
            num_shared_sides: 0,
            zones: Vec::new(),
            polys: 0,
            bounds: Vec::new(),
            leaf_hulls: Vec::new(),
            leaves: Vec::new(),
            lights: Vec::new(),
            root_outside: false,
            linked: false,
            sections: Vec::new(),
            light_maps: Vec::new(),
            light_map_textures: Vec::new(),
        };
        let corners: Vec<[f32; 3]> = brush_corners(&model).collect();
        assert_eq!(corners, [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]]);
    }

    #[test]
    fn near_segment_once_each_ascending() {
        let mut grid = ShapeGrid { wide: vec![4], count: 6, ..ShapeGrid::default() };
        for (cell, shapes) in [((0, 0), vec![1, 2]), ((1, 0), vec![2, 3, 4]), ((40, 40), vec![5])] {
            grid.cells.insert(cell, shapes);
        }
        let other = ShapeGrid { wide: vec![2, 0, 1], count: 3, ..ShapeGrid::default() };
        let mut scratch = NearScratch::default();
        let (start, end) = (Vector::new(10.0, 10.0, 0.0), Vector::new(600.0, 10.0, 0.0));
        for _ in 0..3 {
            grid.near_segment(start, end, 0.0, &mut scratch);
            assert_eq!(scratch.found, [1, 2, 3, 4]);
            other.near_segment(start, end, 0.0, &mut scratch);
            assert_eq!(scratch.found, [0, 1, 2]);
        }
        scratch.query = u32::MAX;
        grid.near_segment(start, end, 0.0, &mut scratch);
        assert_eq!(scratch.found, [1, 2, 3, 4]);
    }
}
