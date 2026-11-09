use std::sync::Arc;
use p2_assets::read_static_mesh_materials;
use p2_collision::{Hit, Primitive, Sides, box_check, line_check, line_check_node, mesh_box_check, mesh_line_check, mesh_line_check_at, pull_back, pull_back_units, segment_cylinder, terrain_box_check, terrain_line_check};
use p2_math::Vector;
use crate::native_id::Native;
use crate::natives::vector_arg;
use crate::{Error, Kind, LoaderRef, ObjectId, Prop, Value, Vm};
use super::trace;

const OWNER_CHAIN: usize = 64;

impl<L: LoaderRef> Vm<L> {
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
        let terrain = world.terrains.iter().filter_map(|t| terrain_line_check(t, start, end, Sides::FrontOnly)).map(|hit| pull_back(start, end, hit, 0.5));
        let mesh = self
            .with_near_lines(start, end, |near| mesh_line_check(near.iter().map(|&i| &*world.lines[i].shape), start, end))
            .map(|hit| pull_back(start, end, hit, pull_back_units(Primitive::StaticMesh, length)));
        bsp.into_iter().chain(terrain).chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))
    }

    /// Pulled back like retail: terrain by 0.5 units (unverified), static meshes by 0.1 of the trace,
    /// at least 0.1 and at most 1 unit, brushes 0.1 to 4 units.
    pub(crate) fn trace_boxes(&self, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
        let world = &self.world;
        let length = (end - start).size();
        let terrain = world.terrains.iter().filter_map(|t| terrain_box_check(t, start, end, extent)).map(|hit| pull_back(start, end, hit, 0.5));
        let (mesh, brush) = self.with_near_boxes(start, end, extent, |near| {
            let shapes = |model: bool| near.iter().map(|&i| &world.boxes[i]).filter(move |w| w.model == model).map(|w| &*w.shape);
            (mesh_box_check(shapes(false), start, end, extent), mesh_box_check(shapes(true), start, end, extent))
        });
        let mesh = mesh.map(|hit| pull_back(start, end, hit, pull_back_units(Primitive::StaticMesh, length)));
        let brush = brush.map(|hit| pull_back(start, end, hit, pull_back_units(Primitive::Brush, length)));
        terrain.chain(mesh).chain(brush).min_by(|a, b| a.time.total_cmp(&b.time))
    }

    pub(crate) fn trace_bsp(&self, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
        self.world.model.as_ref().and_then(|model| box_check(model, &self.world.hulls, start, end, extent))
    }

    /// A blocking volume's inside alone does not count. Terrain has no point check here yet,
    /// so a box sunk in terrain is not found.
    pub(crate) fn world_encroached(&self, location: Vector, extent: Vector) -> bool {
        let bsp = self.world.model.as_ref().is_some_and(|model| p2_collision::point_check(model, &self.world.hulls, location, extent).is_some());
        bsp || self.with_near_boxes(location, location, extent, |near| {
            p2_collision::mesh_box_overlaps(near.iter().map(|&i| &*self.world.boxes[i].shape), location, extent)
        })
    }

    pub(crate) fn trace_lines(&self, start: Vector, end: Vector) -> Option<Hit> {
        let world = &self.world;
        let terrain = world.terrains.iter().filter_map(|t| terrain_line_check(t, start, end, Sides::FrontOnly));
        let mesh = self.with_near_lines(start, end, |near| mesh_line_check(near.iter().map(|&i| &*world.lines[i].shape), start, end));
        terrain.chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))
    }

    fn with_near_boxes<R>(&self, start: Vector, end: Vector, extent: Vector, f: impl FnOnce(&[usize]) -> R) -> R {
        self.world.box_grid.with_near(start, end, extent.x.max(extent.y), f)
    }

    fn with_near_lines<R>(&self, start: Vector, end: Vector, f: impl FnOnce(&[usize]) -> R) -> R {
        self.world.line_grid.with_near(start, end, 0.0, f)
    }

    fn owner_chain(&self, actor: ObjectId) -> impl Iterator<Item = ObjectId> {
        std::iter::successors(Some(actor), |&at| self.store.object_of(at, Prop::Owner)).take(OWNER_CHAIN)
    }

    pub(crate) fn is_owned_by(&self, actor: ObjectId, owner: ObjectId) -> bool {
        self.owner_chain(actor).any(|at| at == owner)
    }

    pub(crate) fn should_trace(&self, actor: ObjectId, source: Option<ObjectId>, flags: u32) -> bool {
        use crate::{Kind, Prop};
        let source_not_pawn = source.is_some_and(|s| !self.store.is_a(s, Kind::Pawn));
        if self.store.is_a(actor, Kind::Pawn) {
            return !(self.store.flag_of(actor, Prop::bOnlyAffectPawns) && source_not_pawn) && flags & trace::PAWNS != 0;
        }
        if self.store.is_a(actor, Kind::Mover) {
            return flags & trace::MOVERS != 0;
        }
        if source.is_some_and(|s| self.store.flag_of(s, Prop::bOnlyAffectPawns)) || (self.store.flag_of(actor, Prop::bOnlyAffectPawns) && source_not_pawn) {
            return false;
        }
        let volume = self.store.is_a(actor, Kind::Volume);
        if volume && self.store.is_a(actor, Kind::BlockingVolume) && source.is_none_or(|s| !self.class_blocks(actor, s)) {
            return false;
        }
        if self.store.flag_of(actor, Prop::bWorldGeometry) && flags & trace::LEVEL_GEOMETRY != 0 {
            return true;
        }
        if flags & (if volume { trace::VOLUMES } else { trace::OTHERS }) == 0 || (!volume && self.store.flag_of(actor, Prop::bWorldGeometry)) {
            return false;
        }
        if flags & trace::ONLY_PROJ_ACTOR != 0 {
            self.store.flag_of(actor, Prop::bProjTarget) || (self.store.flag_of(actor, Prop::bBlockActors) && self.store.flag_of(actor, Prop::bBlockPlayers))
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
                let to = self.store.vector(object, Prop::Location);
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
        let mut source_owners = [None; OWNER_CHAIN];
        for (slot, owner) in source_owners.iter_mut().zip(source.into_iter().flat_map(|s| self.owner_chain(s))) {
            *slot = Some(owner);
        }
        for entry in &candidates {
            let actor = entry.actor;
            if self.world.is_static(actor)
                || Some(actor) == source
                || self.store.is_deleted(actor)
                || !self.store.flag_of(actor, Prop::bCollideActors)
                || !self.store.flag_of(actor, blocks_traces)
                || source_owners.iter().map_while(|&owner| owner).any(|owner| owner == actor)
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
            } else if let Some(shape) = self.fill_shape(actor).and_then(|placement| self.placed_shape(actor, &placement)) {
                let primitive = if shape.model { Primitive::Brush } else { Primitive::StaticMesh };
                let units = if zero_extent && shape.model { 0.5 } else { pull_back_units(primitive, length) };
                if zero_extent { mesh_line_check([&*shape.lines], start, end) } else { mesh_box_check([&*shape.boxes], start, end, extent) }
                    .map(|hit| pull_back(start, end, hit, units))
            } else if self.store.is_a(actor, self.script.lookups.class_names.fluid_surface_info) {
                let height = self.store.vector(actor, Prop::Location)[2];
                crate::collision::fluid_line_check(self.fluid_bounds(actor), height, start, end, extent)
            } else {
                let radius = self.store.float_of(actor, Prop::CollisionRadius) + extent.x.max(extent.y);
                let height = self.store.float_of(actor, Prop::CollisionHeight) + extent.z;
                segment_cylinder(start, end, self.store.vector(actor, Prop::Location), radius, height)
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
        let here = this.map_or(Vector::ZERO, |this| self.store.vector(this, Prop::Location));
        let source = this.map(|this| if self.store.is_a(this, Kind::Controller) { self.store.object_of(this, Prop::Pawn).unwrap_or(this) } else { this });
        if native == Native::FastTrace {
            let end = vector_arg(args, 0).unwrap_or(here);
            let start = vector_arg(args, 1).unwrap_or(here);
            return Ok(Some(Value::Bool(!self.any_hit(start, end, Vector::ZERO, source, trace::WORLD))));
        }
        let end = vector_arg(args, 2).unwrap_or(here);
        let start = vector_arg(args, 3).unwrap_or(here);
        let actors = match args.get(4) {
            None | Some(Value::Map) => this.is_some_and(|this| self.store.flag_of(this, Prop::bCollideActors)),
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
            let shape = self.fill_shape(actor).and_then(|placement| self.placed_shape(actor, &placement))?;
            let (_, _, triangle) = mesh_line_check_at([&*shape.lines], start, end)?;
            (actor, shape.lines.slot(triangle)?)
        };
        let slot = slot as usize;
        if let Some(Value::Array(skins)) = self.store.get(mesh_actor, Prop::Skins)
            && let Some(&Value::Object(Some(skin))) = skins.get(slot)
        {
            self.load_stub(skin);
            return Some(skin);
        }
        let mesh = self.store.object_of(mesh_actor, Prop::StaticMesh)?;
        let loader = self.loader.clone();
        let (package, materials) = match self.world.mesh_materials.get(&mesh) {
            Some(known) => known.clone()?,
            None => {
                let read = loader.loaded_export_by_path_of_class(&self.store.object(mesh).path, "StaticMesh").and_then(|(package, export)| {
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
            Some(&Value::Object(Some(class))) => self.store.object(class).name,
            _ => self.script.lookups.class_names.actor,
        };
        let here = this.map_or(Vector::ZERO, |this| self.store.vector(this, Prop::Location));
        let (end, start, extent) = (vector_arg(args, 3).unwrap_or(here), vector_arg(args, 4).unwrap_or(here), vector_arg(args, 5).unwrap_or(Vector::ZERO));
        self.line_check(start, end, extent, this, trace::ALL_COLLIDING)
            .into_iter()
            .map(|(actor, hit)| match actor {
                Some(actor) if !self.store.is_deleted(actor) && self.store.is_a(actor, class) => Value::Struct(vec![
                    Value::Object(Some(actor)),
                    Value::Vector(hit.location),
                    Value::Vector(hit.normal),
                ]),
                _ => Value::Map,
            })
            .collect()
    }
}
