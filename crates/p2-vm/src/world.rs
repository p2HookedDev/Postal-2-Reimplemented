use std::sync::Arc;

use p2_assets::{
    Model, Terrain, fan_triangles, poly_flags, read_level, read_model, read_static_mesh, read_static_mesh_materials, read_terrain, read_texture,
    texture_heights,
};
use p2_collision::{
    Hit, MeshCollision, box_check, line_check, line_check_node, mesh_box_check, mesh_line_check, mesh_line_check_at, pull_back, pull_back_units,
    segment_cylinder, terrain_box_check, terrain_line_check,
};
use p2_math::{Rotator, Vector};
use p2_package::PackageId;
use p2_script::{Objects, Source};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::enums::EDrawType;
use crate::native_id::Native;
use crate::natives::vector_arg;
use crate::{Error, Kind, LoaderRef, ObjectId, Prop, Value, Vm};

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
    pub(crate) one_sided: bool,
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
    zone_actors: Vec<Option<ObjectId>>,
    terrains: Vec<Terrain>,
    boxes: Vec<WorldShape>,
    lines: Vec<WorldShape>,
    box_grid: ShapeGrid,
    line_grid: ShapeGrid,
    pub(crate) static_actors: FxHashSet<ObjectId>,
    mesh_materials: FxHashMap<ObjectId, Option<(PackageId, Arc<[i32]>)>>,
    pub(crate) primitives: crate::collision::Primitives,
    pub(crate) frame: u64,
}

impl<L: LoaderRef> Vm<L> {
    /// Static world-geometry actors block any pawn that collides with the world, whatever their
    /// `bBlockActors` / `bBlockPlayers`. Class-blocking volumes and cylinder actors stay with the others.
    pub(crate) fn build_world(&mut self, map: PackageId) {
        let loader = self.loader.clone();
        let loaded = loader.package(map);
        let (pkg, data) = (&loaded.package, &loaded.data[..]);
        let model = pkg
            .exports
            .iter()
            .find(|e| pkg.object_name(e.class_index) == Some("Level"))
            .and_then(|level| read_level(pkg, data, level).ok())
            .and_then(|level| pkg.exports.get(usize::try_from(level.model.checked_sub(1)?).ok()?))
            .and_then(|model| read_model(pkg, data, model).ok());
        let mut terrains = Vec::new();
        for (index, export) in pkg.exports.iter().enumerate() {
            if pkg.object_name(export.class_index) != Some("TerrainInfo") {
                continue;
            }
            let Ok(object) = self.load_object(map, index) else { continue };
            let Some(class) = self.object(object).class else { continue };
            let Ok(mut terrain) = read_terrain(pkg, data, export, &self.class(class).layout) else { continue };
            let heights = loader.resolve_loaded(map, terrain.heightmap).and_then(|found| {
                let loaded = loader.package(found.package);
                let source = Source { package: &loaded.package, data: &loaded.data };
                let texture = read_texture(&mut Objects::new(&*loader), source, found.export?).ok()?;
                texture_heights(&texture).map(|(_, _, heights)| heights)
            });
            if let Some(heights) = heights {
                terrain.vertices = terrain.vertices_from_heights(&heights);
                terrains.push(terrain);
            }
        }
        let zone_refs: Vec<i32> = model.as_ref().map(|model| model.zones.iter().map(|zone| zone.actor).collect()).unwrap_or_default();
        let zone_actors = zone_refs
            .into_iter()
            .map(|reference: i32| reference.checked_sub(1).and_then(|export| usize::try_from(export).ok()).and_then(|export| self.load_object(map, export).ok()))
            .collect();
        self.world = World { map: Some(map), model, zone_actors, terrains, ..World::default() };
        self.hash = crate::hash::CollisionHash::default();
        let actors: Vec<_> = self.actors().collect();
        for actor in actors {
            let world_geometry = [Prop::bStatic, Prop::bWorldGeometry, Prop::bCollideActors].into_iter().all(|f| self.flag_of(actor, f));
            if !world_geometry || self.is_deleted(actor) || (self.is_a(actor, Kind::BlockingVolume) && self.flag_of(actor, Prop::bClassBlocker)) {
                continue;
            }
            let Some(shape) = self.actor_shape(actor) else { continue };
            if self.flag_of(actor, Prop::bBlockNonZeroExtentTraces) {
                self.world.boxes.push(WorldShape { shape: shape.boxes, model: shape.model, actor });
            }
            if self.flag_of(actor, Prop::bBlockZeroExtentTraces) {
                self.world.lines.push(WorldShape { shape: shape.lines, model: shape.model, actor });
            }
            self.world.static_actors.insert(actor);
        }
        self.world.box_grid = ShapeGrid::new(&self.world.boxes);
        self.world.line_grid = ShapeGrid::new(&self.world.lines);
    }

    /// Not the engine's set (unverified): static meshes with `bBlockKarma` by collision triangles,
    /// or `KPhysicsProps` hulls with `UseSimpleKarmaCollision`.
    pub(crate) fn level_meshes(&self) -> Vec<LevelMesh> {
        const NF_NOT_CSG: u8 = 0x01;
        let mut meshes = Vec::new();
        let fan = |positions: &mut Vec<[f32; 3]>, triangles: &mut Vec<[u32; 3]>, polygon: &[[f32; 3]]| {
            let base = positions.len() as u32;
            positions.extend_from_slice(polygon);
            triangles.extend(fan_triangles(polygon.len()).map(|t| t.map(|i| base + i as u32)));
        };
        if let Some(model) = &self.world.model {
            let (mut positions, mut triangles) = (Vec::new(), Vec::new());
            let skip = poly_flags::PORTAL | poly_flags::FAKE_BACKDROP;
            for polygon in model.polygons() {
                let solid = model.nodes.get(polygon.node).is_some_and(|n| n.node_flags & NF_NOT_CSG == 0);
                if solid && polygon.poly_flags & skip == 0 {
                    fan(&mut positions, &mut triangles, &polygon.positions);
                }
            }
            meshes.push(LevelMesh { vertices: positions, triangles, one_sided: false });
        }
        for terrain in &self.world.terrains {
            let triangles = terrain.karma_triangles().into_iter().map(|t| t.map(|i| i as u32)).collect();
            meshes.push(LevelMesh { vertices: terrain.vertices.clone(), triangles, one_sided: true });
        }
        let (mut positions, mut triangles) = (Vec::new(), Vec::new());
        let loader = self.loader.clone();
        let mut read = FxHashMap::default();
        for actor in self.actors() {
            let blocking = [Prop::bStatic, Prop::bCollideActors, Prop::bBlockActors].into_iter().all(|f| self.flag_of(actor, f));
            let Some(mesh) = self.object_of(actor, Prop::StaticMesh).filter(|_| blocking && self.draw_type_of(actor) == Some(EDrawType::StaticMesh)) else { continue };
            let Some((package, export)) = loader.loaded_export_by_path_of_class(&self.object(mesh).path, "StaticMesh") else { continue };
            let mesh = read.entry((package, export)).or_insert_with(|| {
                let source = loader.package(package);
                read_static_mesh(&source.package, &source.data, &source.package.exports[export]).ok()
            });
            let Some(mesh) = mesh.as_ref() else { continue };
            let scale = self.vector(actor, Prop::DrawScale3D).map(|s| s * self.float_of(actor, Prop::DrawScale));
            let placed = MeshCollision::new(mesh, self.vector(actor, Prop::Location), self.rotator_of(actor, Prop::Rotation), scale, self.vector(actor, Prop::PrePivot));
            for triangle in placed.triangles() {
                fan(&mut positions, &mut triangles, triangle);
            }
        }
        meshes.push(LevelMesh { vertices: positions, triangles, one_sided: false });
        meshes.retain(|mesh| !mesh.triangles.is_empty());
        meshes
    }

    /// As retail, every actor with `bBlockKarma` gets its own Karma model, without draw scale for a
    /// `BlockingVolume`. A `KarmaBlockingVolume` does not collide or block, and traces skip it.
    pub(crate) fn karma_blocking_hulls(&mut self) -> Vec<Vec<[f32; 3]>> {
        let volumes: Vec<ObjectId> = self
            .actors()
            .filter(|&a| !self.is_deleted(a) && self.is_a(a, Kind::BlockingVolume) && self.flag_of(a, Prop::bBlockKarma))
            .collect();
        let mut hulls = Vec::new();
        for volume in volumes {
            let Some(model) = self.object_of(volume, Prop::Brush).and_then(|brush| self.brush_model(brush)) else { continue };
            let (location, pre_pivot) = (self.vector(volume, Prop::Location), self.vector(volume, Prop::PrePivot));
            let axes = self.rotator_of(volume, Prop::Rotation).axes();
            hulls.push(brush_corners(&model).map(|p| [0, 1, 2].map(|c| location[c] + (0..3).map(|k| axes[k][c] * (p[k] - pre_pivot[k])).sum::<f32>())).collect());
        }
        hulls
    }

    pub(crate) fn vector(&self, object: ObjectId, property: impl crate::PropertyName) -> Vector {
        self.member_words::<3>(object, property).map_or(Vector::ZERO, |members| {
            members.map(|m| match m {
                (word, crate::memory::Shape::Float) => f32::from_bits(word),
                _ => 0.0,
            }).into()
        })
    }

    pub(crate) fn vec3_of(&self, object: ObjectId, property: impl crate::PropertyName) -> Vector {
        self.members::<3>(object, property).map_or(Vector::ZERO, |members| members.map(|m| crate::interp::float(&m)).into())
    }

    pub fn location(&self, actor: ObjectId) -> Vector {
        self.vector(actor, Prop::Location)
    }

    pub fn velocity(&self, actor: ObjectId) -> Vector {
        self.vector(actor, Prop::Velocity)
    }

    /// Retail divides an emitter's step by `TimeDilation` when its `Owner` is a player (unverified),
    /// so the player's own effects run in real time.
    pub fn particle_time_scale(&self, actor: ObjectId) -> f32 {
        let player_owned = self.object_of(actor, Prop::Owner).is_some_and(|owner| self.is_player(owner));
        let dilation = self.level.map_or(1.0, |level| self.float_of(level, Prop::TimeDilation));
        if player_owned && dilation > 0.0 { 1.0 / dilation } else { 1.0 }
    }

    pub fn drag_actors(&self) -> Vec<(Vector, Vector, f32, f32)> {
        const FT_DRAG_ALONG: u8 = 1;
        let float = |actor, name: Prop| match self.get(actor, name) {
            Some(Value::Float(value)) => value,
            _ => 0.0,
        };
        self.actors()
            .filter(|&actor| !self.is_deleted(actor) && self.get(actor, Prop::ForceType) == Some(Value::Byte(FT_DRAG_ALONG)))
            .filter(|&actor| float(actor, Prop::ForceRadius) != 0.0)
            .map(|actor| (self.location(actor), self.velocity(actor), float(actor, Prop::ForceRadius), float(actor, Prop::ForceScale)))
            .collect()
    }

    pub fn rotation(&self, actor: ObjectId) -> Rotator {
        self.member_words::<3>(actor, Prop::Rotation).map_or([0; 3].into(), |members| {
            members.map(|m| match m {
                (word, crate::memory::Shape::Int) => word as i32,
                _ => 0,
            }).into()
        })
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

    pub fn trace_world(&self, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
        if extent != Vector::ZERO {
            let bsp = self.trace_bsp(start, end, extent);
            return bsp.into_iter().chain(self.trace_boxes(start, end, extent)).min_by(|a, b| a.time.total_cmp(&b.time));
        }
        // Pulled back like the engine's line checks (BSP and terrain 0.5 units, static meshes `pull_back_units`): what
        // lies at a hit (a puddle's flat sprite) stays above the surface's neighbouring triangles.
        let world = &self.world;
        let length = (end - start).size();
        let bsp = world.model.as_ref().and_then(|model| line_check(model, start, end)).map(|hit| pull_back(start, end, hit, 0.5));
        let terrain = world.terrains.iter().filter_map(|t| terrain_line_check(t, start, end)).map(|hit| pull_back(start, end, hit, 0.5));
        let mesh = self
            .with_near_lines(start, end, |near| mesh_line_check(near.iter().map(|&i| &*world.lines[i].shape), start, end))
            .map(|hit| pull_back(start, end, hit, pull_back_units(false, length)));
        bsp.into_iter().chain(terrain).chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))
    }

    /// Pulled back like retail: terrain by 0.5 units (unverified), static meshes by 0.1 of the trace,
    /// at least 0.1 and at most 1 unit, brushes 0.1 to 4 units.
    pub(crate) fn trace_boxes(&self, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
        let world = &self.world;
        let length = (end - start).size();
        let back = |hit: Hit, units: f32| {
            let time = if length > 1e-6 { (hit.time - units / length).clamp(0.0, 1.0) } else { hit.time };
            Hit { time, location: start.lerp(end, time), ..hit }
        };
        let terrain = world.terrains.iter().filter_map(|t| terrain_box_check(t, start, end, extent)).map(|hit| back(hit, 0.5));
        let (mesh, brush) = self.with_near_boxes(start, end, extent, |near| {
            let shapes = |model: bool| near.iter().map(|&i| &world.boxes[i]).filter(move |w| w.model == model).map(|w| &*w.shape);
            (mesh_box_check(shapes(false), start, end, extent), mesh_box_check(shapes(true), start, end, extent))
        });
        let mesh = mesh.map(|hit| back(hit, pull_back_units(false, length)));
        let brush = brush.map(|hit| back(hit, pull_back_units(true, length)));
        terrain.chain(mesh).chain(brush).min_by(|a, b| a.time.total_cmp(&b.time))
    }

    pub(crate) fn trace_bsp(&self, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
        self.world.model.as_ref().and_then(|model| box_check(model, start, end, extent))
    }

    /// A blocking volume's inside alone does not count. Terrain has no point check here yet,
    /// so a box sunk in terrain is not found.
    pub(crate) fn world_encroached(&self, location: Vector, extent: Vector) -> bool {
        let bsp = self.world.model.as_ref().is_some_and(|model| p2_collision::point_check(model, location, extent).is_some());
        bsp || self.with_near_boxes(location, location, extent, |near| {
            p2_collision::mesh_box_overlaps(near.iter().map(|&i| &*self.world.boxes[i].shape), location, extent)
        })
    }

    pub(crate) fn trace_lines(&self, start: Vector, end: Vector) -> Option<Hit> {
        let world = &self.world;
        let terrain = world.terrains.iter().filter_map(|t| terrain_line_check(t, start, end));
        let mesh = self.with_near_lines(start, end, |near| mesh_line_check(near.iter().map(|&i| &*world.lines[i].shape), start, end));
        terrain.chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))
    }

    fn with_near_boxes<R>(&self, start: Vector, end: Vector, extent: Vector, f: impl FnOnce(&[usize]) -> R) -> R {
        self.world.box_grid.with_near(start, end, extent.x.max(extent.y), f)
    }

    fn with_near_lines<R>(&self, start: Vector, end: Vector, f: impl FnOnce(&[usize]) -> R) -> R {
        self.world.line_grid.with_near(start, end, 0.0, f)
    }

    pub(crate) fn is_owned_by(&self, actor: ObjectId, owner: ObjectId) -> bool {
        let mut current = Some(actor);
        for _ in 0..64 {
            match current {
                Some(at) if at == owner => return true,
                Some(at) => current = self.object_of(at, Prop::Owner),
                None => return false,
            }
        }
        false
    }

    pub(crate) fn should_trace(&self, actor: ObjectId, source: Option<ObjectId>, flags: u32) -> bool {
        use crate::{Kind, Prop};
        let source_not_pawn = source.is_some_and(|s| !self.is_a(s, Kind::Pawn));
        if self.is_a(actor, Kind::Pawn) {
            return !(self.flag_of(actor, Prop::bOnlyAffectPawns) && source_not_pawn) && flags & trace::PAWNS != 0;
        }
        if self.is_a(actor, Kind::Mover) {
            return flags & trace::MOVERS != 0;
        }
        if source.is_some_and(|s| self.flag_of(s, Prop::bOnlyAffectPawns)) || (self.flag_of(actor, Prop::bOnlyAffectPawns) && source_not_pawn) {
            return false;
        }
        let volume = self.is_a(actor, Kind::Volume);
        if volume && self.is_a(actor, Kind::BlockingVolume) && source.is_none_or(|s| !self.class_blocks(actor, s)) {
            return false;
        }
        if self.flag_of(actor, Prop::bWorldGeometry) && flags & trace::LEVEL_GEOMETRY != 0 {
            return true;
        }
        if flags & (if volume { trace::VOLUMES } else { trace::OTHERS }) == 0 || (!volume && self.flag_of(actor, Prop::bWorldGeometry)) {
            return false;
        }
        if flags & trace::ONLY_PROJ_ACTOR != 0 {
            self.flag_of(actor, Prop::bProjTarget) || (self.flag_of(actor, Prop::bBlockActors) && self.flag_of(actor, Prop::bBlockPlayers))
        } else if flags & trace::BLOCKING != 0 {
            source.is_some_and(|s| self.is_blocked_by(s, actor))
        } else {
            true
        }
    }

    pub(crate) fn line_check(&mut self, start: Vector, end: Vector, extent: Vector, source: Option<ObjectId>, flags: u32) -> Vec<(Option<ObjectId>, Hit)> {
        let mut hits = Vec::new();
        self.line_check_each(start, end, extent, source, flags, |actor, hit| {
            hits.push((actor, hit));
            false
        });
        hits.sort_by(|a, b| a.1.time.total_cmp(&b.1.time));
        hits
    }

    pub(crate) fn first_hit(&mut self, start: Vector, end: Vector, extent: Vector, source: Option<ObjectId>, flags: u32) -> Option<(Option<ObjectId>, Hit)> {
        let mut best: Option<(Option<ObjectId>, Hit)> = None;
        self.line_check_each(start, end, extent, source, flags, |actor, hit| {
            if best.as_ref().is_none_or(|(_, b)| hit.time.total_cmp(&b.time).is_lt()) {
                best = Some((actor, hit));
            }
            false
        });
        best
    }

    pub(crate) fn keep_line_of_sight(&mut self, source: Option<ObjectId>, from: Vector, items: &mut Vec<Value>) {
        items.retain(|item| match item {
            &Value::Object(Some(object)) => {
                let to = self.vector(object, Prop::Location);
                self.first_hit(from, to, Vector::ZERO, source, trace::WORLD).is_none_or(|(actor, _)| actor == Some(object))
            }
            _ => false,
        });
    }

    pub(crate) fn any_hit(&mut self, start: Vector, end: Vector, extent: Vector, source: Option<ObjectId>, flags: u32) -> bool {
        let mut blocked = false;
        self.line_check_each(start, end, extent, source, flags, |_, _| {
            blocked = true;
            true
        });
        blocked
    }

    pub fn corona_blocked(&mut self, eye: Vector, light: Vector) -> bool {
        self.any_hit(eye, light, Vector::ZERO, None, trace::WORLD)
    }

    pub fn zones_seen_from(&self, eye: Vector) -> u64 {
        let Some(model) = self.world.model.as_ref() else { return u64::MAX };
        let Some(leaf) = p2_collision::point_leaf(model, eye).and_then(|leaf| model.leaves.get(leaf)) else { return u64::MAX };
        if leaf.visible_zones == 0 {
            return u64::MAX;
        }
        leaf.visible_zones | 1 | (1u64 << (p2_collision::point_zone(model, eye) & 63))
    }

    pub fn zone_number(&self, point: Vector) -> u8 {
        self.world.model.as_ref().map_or(0, |model| p2_collision::point_zone(model, point))
    }

    fn line_check_each(
        &mut self,
        start: Vector,
        end: Vector,
        extent: Vector,
        source: Option<ObjectId>,
        flags: u32,
        mut visit: impl FnMut(Option<ObjectId>, Hit) -> bool,
    ) {
        let mut reach = 1.0;
        let timed = self.diagnostics.count_work.then(std::time::Instant::now);
        self.count(crate::profiling::Work::Trace);
        let level_hit = if flags & (trace::LEVEL | trace::LEVEL_GEOMETRY) != 0 { self.trace_world(start, end, extent) } else { None };
        let timed = timed.map(|started| {
            self.count_n(crate::profiling::Work::TraceWorldMicros, started.elapsed().as_micros() as usize);
            std::time::Instant::now()
        });
        if let Some(hit) = level_hit {
            reach = hit.time;
            if visit(self.level, hit) {
                return;
            }
        }
        if flags & (trace::PAWNS | trace::MOVERS | trace::VOLUMES | trace::OTHERS | trace::LEVEL_GEOMETRY) == 0 {
            return;
        }
        let zero_extent = extent == Vector::ZERO;
        let blocks_traces = if zero_extent { Prop::bBlockZeroExtentTraces } else { Prop::bBlockNonZeroExtentTraces };
        let length = (end - start).size();
        let candidates = self.colliders_along([start.x, start.y], [end.x, end.y], extent.x.max(extent.y));
        for entry in &candidates {
            let actor = entry.actor;
            if self.world.static_actors.contains(&actor)
                || Some(actor) == source
                || self.is_deleted(actor)
                || !self.flag_of(actor, Prop::bCollideActors)
                || !self.flag_of(actor, blocks_traces)
                || source.is_some_and(|s| self.is_owned_by(s, actor))
                || !self.should_trace(actor, source, flags)
            {
                continue;
            }
            self.count(crate::profiling::Work::TraceCandidate);
            let ragdoll = if zero_extent { self.ragdoll_line_check(actor, start, end) } else { None };
            if ragdoll.is_some() {
                self.count(crate::profiling::Work::TraceRagdoll);
            }
            let hit = if let Some(hit) = ragdoll {
                hit
            } else if let Some(shape) = self.actor_shape(actor) {
                let units = if zero_extent && shape.model { 0.5 } else { pull_back_units(shape.model, length) };
                if zero_extent { mesh_line_check([&*shape.lines], start, end) } else { mesh_box_check([&*shape.boxes], start, end, extent) }
                    .map(|hit| pull_back(start, end, hit, units))
            } else {
                let radius = self.float_of(actor, Prop::CollisionRadius) + extent.x.max(extent.y);
                let height = self.float_of(actor, Prop::CollisionHeight) + extent.z;
                segment_cylinder(start, end, self.vector(actor, Prop::Location), radius, height)
            };
            if let Some(hit) = hit.filter(|hit| hit.time <= reach)
                && visit(Some(actor), hit)
            {
                break;
            }
        }
        self.give_entries(candidates);
        if let Some(started) = timed {
            self.count_n(crate::profiling::Work::TraceActorsMicros, started.elapsed().as_micros() as usize);
        }
    }

    /// `Trace(out HitLocation, out HitNormal, TraceEnd, TraceStart, bTraceActors, Extent, out Material)`.
    #[expect(clippy::unnecessary_wraps, reason = "every native dispatcher has this shape: `natives::call` returns its result")]
    pub(crate) fn trace_native(&mut self, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
        let here = this.map_or(Vector::ZERO, |this| self.vector(this, Prop::Location));
        let source = this.map(|this| if self.is_a(this, Kind::Controller) { self.object_of(this, Prop::Pawn).unwrap_or(this) } else { this });
        if native == Native::FastTrace {
            let end = vector_arg(args, 0).unwrap_or(here);
            let start = vector_arg(args, 1).unwrap_or(here);
            return Ok(Some(Value::Bool(!self.any_hit(start, end, Vector::ZERO, source, trace::WORLD))));
        }
        let end = vector_arg(args, 2).unwrap_or(here);
        let start = vector_arg(args, 3).unwrap_or(here);
        let actors = match args.get(4) {
            None | Some(Value::Map) => this.is_some_and(|this| self.flag_of(this, Prop::bCollideActors)),
            Some(value) => crate::interp::truthy(value),
        };
        let extent = vector_arg(args, 5).unwrap_or(Vector::ZERO);
        let flags = if actors { trace::PROJ_TARGETS } else { trace::WORLD };
        let Some((actor, hit)) = self.first_hit(start, end, extent, source, flags) else {
            // A miss still writes the zeroed check result: scripts that keep `HitLocation` (the bullet tracer's
            // `LastHitLocation`) see (0, 0, 0), not the previous hit.
            for slot in args.iter_mut().take(2) {
                *slot = Value::Vector(Vector::ZERO);
            }
            if let Some(slot @ Value::Object(_)) = args.get_mut(6) {
                *slot = Value::Object(None);
            }
            return Ok(Some(Value::Object(None)));
        };
        if let Some(slot) = args.get_mut(0) {
            *slot = Value::Vector(hit.location);
        }
        if let Some(slot) = args.get_mut(1) {
            *slot = Value::Vector(hit.normal);
        }
        if matches!(args.get(6), Some(Value::Object(_))) {
            let material = if extent == Vector::ZERO { self.hit_material(actor, start, end, &hit) } else { None };
            args[6] = Value::Object(material);
        }
        Ok(Some(Value::Object(actor)))
    }

    /// None for terrains, cylinders, the simplified collision model and box traces (unverified),
    /// and where retail falls back to the default material. Loaded so the script reads its `SurfaceType`.
    fn hit_material(&mut self, actor: Option<ObjectId>, start: Vector, end: Vector, hit: &Hit) -> Option<ObjectId> {
        let (mesh_actor, slot) = if actor.is_some() && actor == self.level {
            let model = self.world.model.as_ref();
            if let Some((_, Some(node))) = model.and_then(|m| line_check_node(m, start, end)).filter(|(bsp, _)| bsp.time <= hit.time) {
                let model = model?;
                let material = model.surfaces.get(model.nodes.get(node)?.surface)?.material;
                let map = self.world.map?;
                let loader = self.loader.clone();
                let id = self.object_ref(&loader.package(map).package, map, material)?;
                self.load_stub(id);
                return Some(id);
            }
            let lines = &self.world.lines;
            let (mesh_hit, world, triangle) = self.with_near_lines(start, end, |near| {
                let (mesh_hit, index, triangle) = mesh_line_check_at(near.iter().map(|&i| &*lines[i].shape), start, end)?;
                Some((mesh_hit, &lines[*near.get(index)?], triangle))
            })?;
            if mesh_hit.time > hit.time {
                return None;
            }
            (world.actor, world.shape.slot(triangle)?)
        } else {
            let actor = actor?;
            let shape = self.actor_shape(actor)?;
            let (_, _, triangle) = mesh_line_check_at([&*shape.lines], start, end)?;
            (actor, shape.lines.slot(triangle)?)
        };
        let slot = slot as usize;
        if let Some(Value::Array(skins)) = self.get(mesh_actor, Prop::Skins)
            && let Some(&Value::Object(Some(skin))) = skins.get(slot)
        {
            self.load_stub(skin);
            return Some(skin);
        }
        let mesh = self.object_of(mesh_actor, Prop::StaticMesh)?;
        let loader = self.loader.clone();
        let (package, materials) = match self.world.mesh_materials.get(&mesh) {
            Some(known) => known.clone()?,
            None => {
                let read = loader.loaded_export_by_path_of_class(&self.object(mesh).path, "StaticMesh").and_then(|(package, export)| {
                    let source = loader.package(package);
                    let materials = read_static_mesh_materials(&source.package, &source.data, &source.package.exports[export]).ok()?;
                    Some((package, Arc::<[i32]>::from(materials)))
                });
                self.world.mesh_materials.insert(mesh, read.clone());
                read?
            }
        };
        let source = loader.package(package);
        let id = self.object_ref(&source.package, package, *materials.get(slot)?)?;
        self.load_stub(id);
        Some(id)
    }

    pub(crate) fn trace_actors_items(&mut self, this: Option<ObjectId>, args: &[Value]) -> Vec<Value> {
        let class = match args.first() {
            Some(&Value::Object(Some(class))) => self.object(class).name,
            _ => self.script.lookups.class_names.actor,
        };
        let vector_arg = |n: usize| vector_arg(args, n);
        let here = this.map_or(Vector::ZERO, |this| self.vector(this, Prop::Location));
        let (end, start, extent) = (vector_arg(3).unwrap_or(here), vector_arg(4).unwrap_or(here), vector_arg(5).unwrap_or(Vector::ZERO));
        self.line_check(start, end, extent, this, trace::ALL_COLLIDING)
            .into_iter()
            .map(|(actor, hit)| match actor {
                Some(actor) if !self.is_deleted(actor) && self.is_a(actor, class) => Value::Struct(vec![
                    Value::Object(Some(actor)),
                    Value::Vector(hit.location),
                    Value::Vector(hit.normal),
                ]),
                _ => Value::Map,
            })
            .collect()
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

fn brush_corners(model: &Model) -> impl Iterator<Item = [f32; 3]> + '_ {
    model.nodes.iter().flat_map(move |node| {
        (node.vert_pool..node.vert_pool + usize::from(node.num_vertices)).filter_map(move |i| model.verts.get(i).and_then(|v| model.points.get(v.point)).copied())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
