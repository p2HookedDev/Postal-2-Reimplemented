use std::sync::Arc;

use rustc_hash::FxHashMap;

use p2_assets::{Model, read_model, read_static_mesh};
use p2_collision::{BspHulls, Hit, MeshCollision, model_triangles, static_mesh_collision};

use p2_math::{HEX_ROW_SPACING, Rotator, Vector};
use crate::enums::EPhysics;
use crate::{Kind, LoaderRef, ObjectId, Prop, Vm};

type Triangles = Vec<[[f32; 3]; 3]>;

struct LocalShape {
    boxes: Triangles,
    lines: Option<Triangles>,
    box_slots: Arc<[u32]>,
    line_slots: Arc<[u32]>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    pub pre_pivot: Vector,
}

#[derive(Clone)]
pub(crate) struct PlacedShape {
    pub boxes: Arc<MeshCollision>,
    pub lines: Arc<MeshCollision>,
    pub model: bool,
}

#[derive(Default)]
pub(crate) struct Primitives {
    local: FxHashMap<ObjectId, Option<Arc<LocalShape>>>,
    placed: FxHashMap<ObjectId, (Placement, PlacedShape)>,
    models: FxHashMap<ObjectId, Option<Arc<Model>>>,
    hulls: FxHashMap<ObjectId, BspHulls>,
    pub(crate) physics_volumes: Option<Arc<[ObjectId]>>,
    pub(crate) volume_table: Vec<crate::volumes::VolumeBox>,
    pub(crate) volume_table_frame: Option<u64>,
    pub(crate) volumes: FxHashMap<ObjectId, crate::volumes::VolumePlace>,
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn fill_shape(&mut self, actor: ObjectId) -> Option<Placement> {
        let (primitive, brush, placement) = self.primitive_placement(actor)?;
        if self.placed_shape(actor, &placement).is_some() {
            return Some(placement);
        }
        let local = self.local_shape(primitive, brush)?;
        let phase = self.profile_start();
        let place = |triangles: &Triangles, slots: &Arc<[u32]>| {
            Arc::new(MeshCollision::placed(triangles, placement.location, placement.rotation, placement.scale, placement.pre_pivot).with_slots(slots.clone()))
        };
        let boxes = place(&local.boxes, &local.box_slots);
        let lines = local.lines.as_ref().map_or_else(|| boxes.clone(), |lines| place(lines, &local.line_slots));
        let shape = PlacedShape { boxes, lines, model: brush };
        self.world.primitives.placed.insert(actor, (placement, shape));
        self.profile_phase("collision shape placed", phase);
        Some(placement)
    }

    /// The collision triangles in the actor's own frame (pre-pivot and scale applied), as pawns collide with them.
    pub(crate) fn body_triangles(&mut self, actor: ObjectId) -> Option<(Triangles, Placement)> {
        let (primitive, brush, placement) = self.primitive_placement(actor)?;
        let local = self.local_shape(primitive, brush)?;
        let body = |p: [f32; 3]| [0, 1, 2].map(|i| (p[i] - placement.pre_pivot[i]) * placement.scale[i]);
        Some((local.boxes.iter().map(|t| t.map(body)).collect(), placement))
    }

    fn primitive_placement(&self, actor: ObjectId) -> Option<(ObjectId, bool, Placement)> {
        let brush = self.store.is_a(actor, Kind::Brush);
        let primitive = if brush {
            self.store.object_of(actor, Prop::Brush)?
        } else {
            if self.store.object_of(actor, Prop::Mesh).is_some() || self.store.flag_of(actor, Prop::bUseCylinderCollision) {
                return None;
            }
            self.store.object_of(actor, Prop::StaticMesh)?
        };
        Some((primitive, brush, self.placement(actor)))
    }

    /// For collision and volumes: a missing `DrawScale` reads 0.
    pub(crate) fn placement(&self, actor: ObjectId) -> Placement {
        self.placement_scaled(actor, self.store.float_of(actor, Prop::DrawScale))
    }

    /// As drawn (meshes, Karma shapes): a missing `DrawScale` reads 1.
    pub(crate) fn drawn_placement(&self, actor: ObjectId) -> Placement {
        self.placement_scaled(actor, self.store.get(actor, Prop::DrawScale).as_ref().map_or(1.0, crate::interp::float))
    }

    fn placement_scaled(&self, actor: ObjectId, draw_scale: f32) -> Placement {
        Placement {
            location: self.store.vector(actor, Prop::Location),
            rotation: self.store.rotator_of(actor, Prop::Rotation),
            scale: self.store.vector(actor, Prop::DrawScale3D) * draw_scale,
            pre_pivot: self.store.vector(actor, Prop::PrePivot),
        }
    }

    pub(crate) fn placed_shape(&self, actor: ObjectId, placement: &Placement) -> Option<&PlacedShape> {
        self.world.primitives.placed.get(&actor).filter(|(placed, _)| placed == placement).map(|(_, shape)| shape)
    }

    pub(crate) fn forget_placed_shape(&mut self, actor: ObjectId) {
        self.world.primitives.placed.remove(&actor);
    }

    /// A mover or a volume can reach far beyond its cylinder, and its pivot can be anywhere.
    pub(crate) fn collision_reach(&mut self, actor: ObjectId) -> f32 {
        let placement = self.fill_shape(actor);
        self.reach_of(actor, placement.and_then(|placement| self.placed_shape(actor, &placement)))
    }

    pub(crate) fn placed_reach(&self, actor: ObjectId) -> Option<f32> {
        match self.primitive_placement(actor) {
            None => Some(self.reach_of(actor, None)),
            Some((_, _, placement)) => self.placed_shape(actor, &placement).map(|shape| self.reach_of(actor, Some(shape))),
        }
    }

    fn reach_of(&self, actor: ObjectId, shape: Option<&PlacedShape>) -> f32 {
        let radius = self.store.float_of(actor, Prop::CollisionRadius);
        let Some(shape) = shape else {
            if self.store.is_a(actor, self.script.lookups.class_names.fluid_surface_info) {
                return fluid_reach(self.fluid_bounds(actor)).max(radius);
            }
            return radius;
        };
        let (min, max) = shape.boxes.bounds();
        if min[0] > max[0] {
            return radius;
        }
        let at = self.store.vector(actor, Prop::Location);
        let reach = |i: usize| (min[i] - at[i]).abs().max((max[i] - at[i]).abs());
        reach(0).hypot(reach(1)).max(radius)
    }

    pub(crate) fn brush_model(&mut self, model: ObjectId) -> Option<Arc<Model>> {
        if let Some(found) = self.world.primitives.models.get(&model) {
            return found.clone();
        }
        let loader = self.loader.clone();
        let read = loader.loaded_export_by_path_of_class(&self.store.object(model).path, "Model").and_then(|(package, export)| {
            let source = loader.package(package);
            read_model(&source.package, &source.data, &source.package.exports[export]).ok().map(Arc::new)
        });
        self.world.primitives.models.insert(model, read.clone());
        read
    }

    fn local_shape(&mut self, primitive: ObjectId, model: bool) -> Option<Arc<LocalShape>> {
        if let Some(found) = self.world.primitives.local.get(&primitive) {
            return found.clone();
        }
        let phase = self.profile_start();
        let read = if model {
            self.brush_model(primitive).map(|model| LocalShape { boxes: model_triangles(&model), lines: None, box_slots: Arc::default(), line_slots: Arc::default() })
        } else {
            let loader = self.loader.clone();
            let path = &self.store.object(primitive).path;
            loader.loaded_export_by_path_of_class(path, "StaticMesh").and_then(|(package, export)| {
                let source = loader.package(package);
                let mesh = read_static_mesh(&source.package, &source.data, &source.package.exports[export]).ok()?;
                let collision_model = loader.resolve_loaded(package, mesh.collision_model).and_then(|found| {
                    let source = loader.package(found.package);
                    read_model(&source.package, &source.data, &source.package.exports[found.export?]).ok()
                });
                let (boxes, box_slots) = static_mesh_collision(&mesh, collision_model.as_ref(), false);
                let (lines, line_slots) = static_mesh_collision(&mesh, collision_model.as_ref(), true);
                Some(LocalShape { lines: (lines != boxes).then_some(lines), boxes, box_slots: box_slots.into(), line_slots: line_slots.into() })
            })
        };
        let read = read.map(Arc::new);
        self.world.primitives.local.insert(primitive, read.clone());
        self.profile_phase("load: collision shape", phase);
        read
    }

    pub(crate) fn is_player_pawn(&self, actor: ObjectId) -> bool {
        self.store.is_a(actor, Kind::Pawn)
            && self.store.object_of(actor, Prop::Controller).is_some_and(|c| self.store.is_a(c, Kind::PlayerController) && self.store.object_of(c, Prop::Player).is_some())
    }

    /// Retail does not test `bCollideActors` here.
    pub(crate) fn is_encroacher(&self, actor: ObjectId) -> bool {
        let physics = self.store.physics_of(actor);
        (self.store.object_of(actor, Prop::StaticMesh).is_some() && self.store.is_a(actor, Kind::Mover))
            || physics == Some(EPhysics::KarmaRagDoll)
            || (physics == Some(EPhysics::Karma) && !self.store.is_a(actor, Kind::KConstraint))
    }

    pub(crate) fn is_blocked_by(&self, actor: ObjectId, other: ObjectId) -> bool {
        self.blocking(actor).blocked_by(self.blocking(other))
    }

    pub(crate) fn blocking(&self, actor: ObjectId) -> Blocking {
        let player = self.is_player_pawn(actor);
        Blocking {
            world_geometry: self.store.flag_of(actor, Prop::bWorldGeometry),
            collide_world: self.store.flag_of(actor, Prop::bCollideWorld),
            brush_or_encroacher: (self.store.object_of(actor, Prop::Brush).is_some() && self.store.is_a(actor, Kind::Brush)) || self.is_encroacher(actor),
            player,
            fast: player || self.store.is_a(actor, Kind::Projectile),
            block_actors: self.store.flag_of(actor, Prop::bBlockActors),
            block_players: self.store.flag_of(actor, Prop::bBlockPlayers),
        }
    }

    /// `bAllowOnlySpecifiedClasses` is read as retail; its comment suggests the reverse (unverified).
    pub(crate) fn class_blocks(&self, volume: ObjectId, actor: ObjectId) -> bool {
        if !self.store.is_a(volume, Kind::BlockingVolume) || !self.store.flag_of(volume, Prop::bClassBlocker) {
            return true;
        }
        let exact = self.store.flag_of(volume, Prop::bExactClassesOnly);
        let class = self.store.object(actor).class;
        let listed = match self.store.get(volume, Prop::BlockedClasses) {
            Some(crate::Value::Array(classes)) => classes.iter().any(|c| match c {
                &crate::Value::Object(Some(blocked)) => {
                    let name = self.store.object(blocked).path.rsplit('.').next().unwrap_or_default();
                    if exact { class.is_some_and(|c| self.store.name(self.store.class(c).name).eq_ignore_ascii_case(name)) } else { self.store.is_a(actor, name) }
                }
                _ => false,
            }),
            _ => false,
        };
        listed || self.store.flag_of(volume, Prop::bAllowOnlySpecifiedClasses)
    }

    /// Retail picks the actor whose primitive is tested in this order: `a` a projector, `b` a projector,
    /// `a` a volume or fluid, `b` the same, `a` an encroacher, `b` an encroacher.
    pub(crate) fn shapes_overlap(&mut self, a: ObjectId, b: ObjectId) -> bool {
        let projector = |vm: &Self, actor: ObjectId| vm.store.is_a(actor, Kind::Projector);
        let tested = if projector(self, a) {
            Some((a, b))
        } else if projector(self, b) {
            Some((b, a))
        } else if self.volume_or_fluid(a) {
            Some((a, b))
        } else if self.volume_or_fluid(b) {
            Some((b, a))
        } else if self.is_encroacher(a) {
            Some((a, b))
        } else if self.is_encroacher(b) {
            Some((b, a))
        } else {
            None
        };
        let Some((primitive, other)) = tested else { return self.cylinders_overlap(a, b) };
        let (center, extent) = self.encroach_box(other);
        self.primitive_meets_box(primitive, center, extent)
    }

    pub(crate) fn fluid_bounds(&self, actor: ObjectId) -> [Vector; 2] {
        fluid_box(
            self.store.vector(actor, Prop::Location),
            [self.store.float_of(actor, Prop::FluidXSize), self.store.float_of(actor, Prop::FluidYSize)],
            self.store.float_of(actor, Prop::FluidGridSpacing),
            self.store.byte_of(actor, Prop::FluidGridType) == FGT_HEXAGONAL,
        )
    }

    fn volume_or_fluid(&self, actor: ObjectId) -> bool {
        (self.store.object_of(actor, Prop::Brush).is_some() && self.store.is_a(actor, Kind::Volume)) || self.store.is_a(actor, self.script.lookups.class_names.fluid_surface_info)
    }

    pub(crate) fn overlaps_by_primitive(&self, actor: ObjectId) -> bool {
        self.store.is_a(actor, Kind::Projector) || self.volume_or_fluid(actor) || self.is_encroacher(actor)
    }

    /// A static mesh uses its placed collision triangles' bounds (retail's placed box can be larger).
    /// A skeletal mesh, a ragdoll too, keeps the cylinder, not its Karma bodies.
    pub(crate) fn encroach_box(&mut self, actor: ObjectId) -> (Vector, Vector) {
        if let Some(shape) = self.fill_shape(actor).and_then(|placement| self.placed_shape(actor, &placement)).filter(|shape| !shape.model) {
            let (min, max) = shape.boxes.bounds();
            if min[0] <= max[0] {
                return ([0, 1, 2].map(|i| (min[i] + max[i]) * 0.5).into(), [0, 1, 2].map(|i| (max[i] - min[i]) * 0.5).into());
            }
        }
        let (radius, height) = self.store.collision_size(actor);
        (self.store.vector(actor, Prop::Location), [radius, radius, height].into())
    }

    fn primitive_meets_box(&mut self, actor: ObjectId, center: Vector, extent: Vector) -> bool {
        let cylinder = |vm: &Self| {
            let (radius, height) = vm.store.collision_size(actor);
            cylinder_meets_box(vm.store.vector(actor, Prop::Location), radius, height, center, extent)
        };
        if self.store.is_a(actor, Kind::Projector) {
            return self.projector_meets_box(actor, center, extent).unwrap_or_else(|| cylinder(self));
        }
        if self.store.is_a(actor, self.script.lookups.class_names.fluid_surface_info) {
            return boxes_meet(self.fluid_bounds(actor), center, extent);
        }
        if self.store.is_a(actor, Kind::Brush)
            && let Some((model, _, Placement { location, rotation, scale, pre_pivot })) = self.primitive_placement(actor)
        {
            let Some(brush) = self.brush_model(model) else { return false };
            let axes = rotation.axes();
            let Some(local) = p2_collision::world_to_local(center, location, &axes, scale, pre_pivot) else { return false };
            let hulls = self.world.primitives.hulls.entry(model).or_insert_with(|| BspHulls::new(&brush));
            return p2_collision::point_check(&brush, hulls, local, local_extent(extent, &axes, scale)).is_some();
        }
        match self.fill_shape(actor).and_then(|placement| self.placed_shape(actor, &placement)) {
            Some(shape) => p2_collision::mesh_box_overlaps([&*shape.boxes], center, extent),
            None => cylinder(self),
        }
    }
}

impl<L: LoaderRef> Vm<L> {
    /// Without `ProjTexture` retail's planes stay zero and meet every box; ours use current properties.
    /// `None`: the caller keeps the cylinder.
    fn projector_meets_box(&mut self, projector: ObjectId, center: Vector, extent: Vector) -> Option<bool> {
        let Some(texture) = self.store.object_of(projector, Prop::ProjTexture) else { return Some(true) };
        let frustum = self.projector_frustum(projector, texture)?;
        Some(crate::projector::frustum_meets_box(&frustum.planes, center, extent))
    }

    pub(crate) fn projector_frustum(&mut self, projector: ObjectId, texture: ObjectId) -> Option<crate::projector::Frustum> {
        let base = self.base_texture(texture);
        let [u, v] = self.texture_size(base);
        if u <= 0 || v <= 0 {
            return None;
        }
        let int = |vm: &Self, name: Prop| match vm.store.get(projector, name) {
            Some(crate::Value::Int(value)) => value,
            _ => 0,
        };
        let shape = crate::projector::ProjectorShape {
            location: self.store.vector(projector, Prop::Location),
            rotation: self.store.rotator_of(projector, Prop::Rotation),
            texels: [u as f32, v as f32],
            draw_scale: self.store.float_of(projector, Prop::DrawScale),
            fov: int(self, Prop::FOV),
            max_trace_distance: int(self, Prop::MaxTraceDistance),
            rotating: self.store.physics_of(projector) == Some(EPhysics::Rotating),
        };
        Some(crate::projector::frustum(&shape))
    }

    /// A material is as large as the texture under it: a `FinalBlend` or other modifier's, a shader's diffuse, a
    /// combiner's first (unverified). Splats project such materials, which have no size of their own.
    fn base_texture(&mut self, mut material: ObjectId) -> ObjectId {
        for _ in 0..MAX_MATERIAL_DEPTH {
            self.load_stub(material);
            let Some(next) = MATERIAL_SOURCES.iter().find(|(class, _)| self.store.is_a(material, *class)).and_then(|&(_, source)| self.store.object_of(material, source)) else { break };
            material = next;
        }
        material
    }
}

const MATERIAL_SOURCES: [(&str, &str); 3] = [("Modifier", "Material"), ("Shader", "Diffuse"), ("Combiner", "Material1")];
const MAX_MATERIAL_DEPTH: usize = 8;

/// The box's x half size adds to the radius and its z half size to the height.
fn cylinder_meets_box(at: Vector, radius: f32, height: f32, center: Vector, extent: Vector) -> bool {
    crate::actors::cylinders_meet(at, center, radius + extent[0], height + extent[2])
}

const FGT_HEXAGONAL: u8 = 1;

/// The actor's rotation is not used. Retail grows an empty box by the two corners,
/// so negative half sizes (a count of 0) still give min <= max.
fn fluid_box(location: Vector, counts: [f32; 2], spacing: f32, hexagonal: bool) -> [Vector; 2] {
    let half = |count: f32| (count.clamp(0.0, 256.0) - 1.0) * 0.5 * spacing;
    let (x, y) = (half(counts[0]), half(counts[1]) * if hexagonal { HEX_ROW_SPACING } else { 1.0 });
    let low = [location[0] - x, location[1] - y, location[2] - 5.0];
    let high = [location[0] + x, location[1] + y, location[2] + 5.0];
    [[0, 1, 2].map(|i| low[i].min(high[i])).into(), [0, 1, 2].map(|i| low[i].max(high[i])).into()]
}

fn boxes_meet(bounds: [Vector; 2], center: Vector, extent: Vector) -> bool {
    (0..3).all(|i| bounds[0][i] <= center[i] + extent[i] && center[i] - extent[i] <= bounds[1][i])
}

fn fluid_reach(bounds: [Vector; 2]) -> f32 {
    (bounds[1][0] - bounds[0][0]).hypot(bounds[1][1] - bounds[0][1]) * 0.5
}


pub(crate) fn fluid_line_check(bounds: [Vector; 2], height: f32, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
    let d = end - start;
    if extent == Vector::ZERO {
        let time = (height - start[2]) / d[2];
        if !(0.0..=1.0).contains(&time) {
            return None;
        }
        let location = start + d * time;
        let inside = (0..2).all(|i| bounds[0][i] <= location[i] && location[i] <= bounds[1][i]);
        return inside.then(|| Hit { time, location, normal: Vector::new(0.0, 0.0, 1.0) });
    }
    let (mut enter, mut exit) = (0.0_f32, 1.0_f32);
    let mut normal = Vector::new(0.0, 0.0, 1.0);
    for i in 0..3 {
        let (low, high) = (bounds[0][i] - extent[i], bounds[1][i] + extent[i]);
        if d[i].abs() < 1e-6 {
            if start[i] < low || start[i] > high {
                return None;
            }
            continue;
        }
        let (near, far, side) = if d[i] > 0.0 { ((low - start[i]) / d[i], (high - start[i]) / d[i], -1.0) } else { ((high - start[i]) / d[i], (low - start[i]) / d[i], 1.0) };
        if near > enter {
            enter = near;
            let mut axis = [0.0_f32; 3];
            axis[i] = side;
            normal = axis.into();
        }
        exit = exit.min(far);
        if enter > exit {
            return None;
        }
    }
    Some(Hit { time: enter, location: start + d * enter, normal })
}

/// Half size, in the primitive's space, of the smallest box holding a world box of half size `extent`.
/// `scale` must have no zero component.
fn local_extent(extent: Vector, axes: &[Vector; 3], scale: Vector) -> Vector {
    [0, 1, 2].map(|k| (0..3).map(|c| (axes[k][c] * extent[c]).abs()).sum::<f32>() / scale[k].abs()).into()
}

#[derive(Debug, Clone, Copy)]
#[expect(clippy::struct_excessive_bools, reason = "the flags retail's IsBlockedBy reads")]
pub(crate) struct Blocking {
    world_geometry: bool,
    collide_world: bool,
    brush_or_encroacher: bool,
    player: bool,
    fast: bool,
    block_actors: bool,
    block_players: bool,
}

impl Blocking {
    pub(crate) fn blocked_by(self, other: Self) -> bool {
        let blocks = |a: Self, for_player: bool| if for_player { a.block_players } else { a.block_actors };
        if other.world_geometry {
            return self.collide_world;
        }
        if other.brush_or_encroacher {
            return self.collide_world && blocks(other, self.player);
        }
        if self.brush_or_encroacher {
            return other.collide_world && blocks(self, other.player);
        }
        blocks(other, self.fast) && blocks(self, other.fast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNROTATED: [Vector; 3] = [Vector::new(1.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(0.0, 0.0, 1.0)];

    #[test]
    fn cylinder_meets_box_takes_x_as_radius_and_z_as_height() {
        let at = Vector::new(0.0, 0.0, 0.0);
        let extent = Vector::new(10.0, 500.0, 20.0);
        assert!(cylinder_meets_box(at, 25.0, 40.0, Vector::new(34.0, 0.0, 59.0), extent));
        assert!(!cylinder_meets_box(at, 25.0, 40.0, Vector::new(35.0, 0.0, 0.0), extent));
        assert!(!cylinder_meets_box(at, 25.0, 40.0, Vector::new(0.0, 0.0, 60.0), extent));
        assert!(!cylinder_meets_box(at, 25.0, 40.0, Vector::new(0.0, 36.0, 0.0), extent));
    }

    #[test]
    fn a_line_hits_the_fluid_plane_anywhere_on_the_grid_from_both_sides() {
        let bounds = fluid_box(Vector::new(100.0, 0.0, 50.0), [33.0, 33.0], 32.0, false);
        for (from, to) in [(60.0, 40.0), (40.0, 60.0)] {
            let hit = fluid_line_check(bounds, 50.0, Vector::new(600.0, 300.0, from), Vector::new(600.0, 300.0, to), Vector::ZERO).expect("a hit");
            assert_eq!((hit.time, hit.location, hit.normal), (0.5, Vector::new(600.0, 300.0, 50.0), Vector::new(0.0, 0.0, 1.0)));
        }
        assert!(fluid_line_check(bounds, 50.0, Vector::new(700.0, 0.0, 60.0), Vector::new(700.0, 0.0, 40.0), Vector::ZERO).is_none());
        assert!(fluid_line_check(bounds, 50.0, Vector::new(600.0, 0.0, 90.0), Vector::new(600.0, 0.0, 60.0), Vector::ZERO).is_none());
        assert!(fluid_line_check(bounds, 50.0, Vector::new(600.0, 0.0, 60.0), Vector::new(800.0, 0.0, 60.0), Vector::ZERO).is_none());
    }

    #[test]
    fn a_box_sweep_hits_the_fluid_box_where_the_extent_first_reaches_it() {
        let bounds = fluid_box(Vector::ZERO, [11.0, 11.0], 10.0, false);
        let extent = Vector::new(10.0, 10.0, 10.0);
        let hit = fluid_line_check(bounds, 0.0, Vector::new(100.0, 0.0, 0.0), Vector::new(0.0, 0.0, 0.0), extent).expect("a hit");
        assert_eq!((hit.time, hit.normal), (0.4, Vector::new(1.0, 0.0, 0.0)));
        assert!(fluid_line_check(bounds, 0.0, Vector::new(100.0, 100.0, 0.0), Vector::new(70.0, 100.0, 0.0), extent).is_none());
    }

    #[test]
    fn fluid_reach_covers_the_grid_corners() {
        let bounds = fluid_box(Vector::ZERO, [33.0, 41.0], 32.0, true);
        let corner = bounds[1][0].hypot(bounds[1][1]);
        assert!((fluid_reach(bounds) - corner).abs() < 1e-3);
        assert!(fluid_reach(bounds) > 500.0);
    }

    #[test]
    fn fluid_box_spans_the_grid_and_five_units_of_height() {
        let at = Vector::new(100.0, 200.0, 300.0);
        let [min, max] = fluid_box(at, [11.0, 5.0], 20.0, false);
        assert_eq!((min, max), (Vector::new(0.0, 160.0, 295.0), Vector::new(200.0, 240.0, 305.0)));
        let [min, max] = fluid_box(at, [300.0, 5.0], 2.0, true);
        assert_eq!((min[0], max[0]), (-155.0, 355.0));
        assert!((max[1] - 200.0 - 4.0 * 0.866_025).abs() < 1e-3);
        let [min, max] = fluid_box(at, [0.0, 1.0], 20.0, false);
        assert_eq!((min[0], max[0], min[1], max[1]), (90.0, 110.0, 200.0, 200.0));
    }

    #[test]
    fn boxes_meet_counts_touching_edges() {
        let bounds = [Vector::new(0.0, 0.0, 0.0), Vector::new(10.0, 10.0, 10.0)];
        let extent = Vector::new(2.0, 2.0, 2.0);
        assert!(boxes_meet(bounds, Vector::new(12.0, 5.0, 5.0), extent));
        assert!(boxes_meet(bounds, Vector::new(5.0, -2.0, 12.0), extent));
        assert!(!boxes_meet(bounds, Vector::new(12.5, 5.0, 5.0), extent));
        assert!(!boxes_meet(bounds, Vector::new(5.0, 5.0, -2.5), extent));
    }

    #[test]
    fn local_extent_unrotated_unscaled_is_the_box() {
        let extent = Vector::new(10.0, 20.0, 30.0);
        assert_eq!(local_extent(extent, &UNROTATED, Vector::new(1.0, 1.0, 1.0)), extent);
    }

    #[test]
    fn local_extent_follows_rotation_and_scale() {
        let axes = [Vector::new(0.0, 1.0, 0.0), Vector::new(-1.0, 0.0, 0.0), Vector::new(0.0, 0.0, 1.0)];
        let extent = Vector::new(10.0, 20.0, 30.0);
        assert_eq!(local_extent(extent, &axes, Vector::new(1.0, 1.0, 1.0)), Vector::new(20.0, 10.0, 30.0));
        assert_eq!(local_extent(extent, &UNROTATED, Vector::new(2.0, -4.0, 0.5)), Vector::new(5.0, 5.0, 60.0));
    }
}
