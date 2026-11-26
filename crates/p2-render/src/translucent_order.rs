//! The engine's back-to-front list of translucent draws: BSP nodes in BSP order from the eye, and each
//! translucent actor placed by distance to its centre against actors and by plane side against nodes. Replaces
//! Bevy's depth sort.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d};
use bevy::core_pipeline::core_3d::{Transparent3d, TransparentSortingInfo3d};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::prelude::{Changed, Component, Entity, Local, Query, RemovedComponents, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Quat, Vec3, Vec4};
use bevy::pbr::{MeshInputUniform, MeshUniform, RenderMeshInstances, get_mesh_instance_world_from_local};
use bevy::render::batching::gpu_preprocessing::BatchedInstanceBuffers;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::mesh::RenderMesh;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_phase::{SortedPhaseItem, SortedRenderPhase, ViewSortedRenderPhases, sort_phase_system};
use bevy::render::sync_world::MainEntity;
use bevy::render::view::ExtractedView;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::transform::TransformSystems;
use bevy::transform::components::GlobalTransform;
use p2_assets::Model;
use p2_bevy_assets::to_bevy;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::level_collision::LevelCollision;
use crate::view::{SKY_LAYER, ShownScene};

#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub struct SortedBspNode {
    node: usize,
    plane: Vec4,
}

impl SortedBspNode {
    pub fn new(node: usize, plane: [f32; 4], center: Vec3) -> Self {
        let normal = Vec3::from(to_bevy([plane[0], plane[1], plane[2]]));
        Self { node, plane: normal.extend(plane[3] - normal.dot(center)) }
    }

    fn side(&self, point: Vec3) -> f32 {
        self.plane.truncate().dot(point) - self.plane.w
    }
}

#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub enum TranslucentDraw {
    Terrain(u32),
    Part { actor: Entity, order: u32 },
    AfterScene,
}

#[derive(Resource, Clone, Default, PartialEq, ExtractResource)]
#[extract_app(RenderApp)]
struct TranslucentOrder {
    ranks: FxHashMap<Entity, FxHashMap<usize, usize>>,
    nodes: FxHashMap<Entity, SortedBspNode>,
    draws: FxHashMap<Entity, TranslucentDraw>,
    sky: FxHashSet<Entity>,
    sky_ranks: Option<FxHashMap<usize, usize>>,
    sky_place: Option<(Vec3, Quat)>,
}

pub struct TranslucentOrderPlugin;

impl Plugin for TranslucentOrderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TranslucentOrder>()
            .init_resource::<ShownScene>()
            .init_resource::<LevelCollision>()
            .add_plugins(ExtractResourcePlugin::<TranslucentOrder>::default())
            .add_systems(PostUpdate, rank_sorted_nodes.after(TransformSystems::Propagate));
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, engine_translucent_order.in_set(RenderSystems::PhaseSort).after(sort_phase_system::<Transparent3d>));
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn rank_sorted_nodes(
    shown: Res<ShownScene>,
    level: Res<LevelCollision>,
    cameras: Query<(Entity, &Camera, &GlobalTransform, Option<&RenderLayers>), With<Camera3d>>,
    sections: Query<(Entity, &SortedBspNode)>,
    draws: Query<(Entity, &TranslucentDraw)>,
    changed_sections: Query<(), Changed<SortedBspNode>>,
    changed_draws: Query<(), Changed<TranslucentDraw>>,
    mut removed_sections: RemovedComponents<SortedBspNode>,
    mut removed_draws: RemovedComponents<TranslucentDraw>,
    sky_layers: Query<(Entity, &RenderLayers), Changed<RenderLayers>>,
    mut removed_layers: RemovedComponents<RenderLayers>,
    mut order: ResMut<TranslucentOrder>,
    mut cached: Local<Option<(usize, WantedNodes)>>,
) {
    let nodes = (removed_sections.read().count() > 0 || !changed_sections.is_empty())
        .then(|| sections.iter().map(|(entity, node)| (entity, *node)).collect::<FxHashMap<Entity, SortedBspNode>>());
    let draws = (removed_draws.read().count() > 0 || !changed_draws.is_empty())
        .then(|| draws.iter().map(|(entity, draw)| (entity, *draw)).collect::<FxHashMap<Entity, TranslucentDraw>>());
    let kept = nodes.as_ref().unwrap_or(&order.nodes);
    let (ranks, sky_ranks): (FxHashMap<Entity, FxHashMap<usize, usize>>, Option<FxHashMap<usize, usize>>) = match level.model().filter(|_| !kept.is_empty()) {
        Some(model) => {
            let wanted: FxHashSet<usize> = kept.values().map(|section| section.node).collect();
            let key = model.nodes.as_ptr() as usize;
            if cached.as_ref().is_none_or(|(at, known)| *at != key || known.wanted != wanted) {
                *cached = Some((key, WantedNodes::new(model, wanted)));
            }
            let Some((_, wanted)) = cached.as_ref() else { return };
            let rank = |eye: Vec3| -> FxHashMap<usize, usize> {
                let eye = to_bevy((eye - shown.origin).to_array());
                back_to_front(model, eye, wanted).into_iter().enumerate().map(|(rank, node)| (node, rank)).collect()
            };
            let views = cameras
                .iter()
                .filter(|(_, camera, _, layers)| camera.is_active && layers.is_none_or(|layers| *layers != RenderLayers::none()))
                .map(|(entity, _, transform, _)| (entity, rank(transform.translation())))
                .collect();
            (views, shown.sky_place.map(|(eye, _)| rank(eye)))
        }
        None => (FxHashMap::default(), None),
    };
    let held = order.bypass_change_detection();
    let mut changed = held.ranks != ranks;
    held.ranks = ranks;
    changed |= held.sky_ranks != sky_ranks || held.sky_place != shown.sky_place;
    held.sky_ranks = sky_ranks;
    held.sky_place = shown.sky_place;
    let sky_layer = RenderLayers::layer(SKY_LAYER);
    for entity in removed_layers.read() {
        changed |= held.sky.remove(&entity);
    }
    for (entity, layers) in &sky_layers {
        changed |= if layers.intersects(&sky_layer) { held.sky.insert(entity) } else { held.sky.remove(&entity) };
    }
    if let Some(nodes) = nodes {
        changed |= held.nodes != nodes;
        held.nodes = nodes;
    }
    if let Some(draws) = draws {
        changed |= held.draws != draws;
        held.draws = draws;
    }
    if changed {
        order.set_changed();
    }
}

/// The wanted nodes in the engine's list order: it pushes each onto the head while walking the tree front to
/// back, so the list runs back to front, a node's coplanar nodes ahead of it.
pub(crate) fn back_to_front(model: &Model, eye: [f32; 3], wanted: &WantedNodes) -> Vec<usize> {
    enum Step {
        Visit(usize),
        Emit(usize),
    }
    let mut out = Vec::new();
    let mut stack = if model.nodes.is_empty() { Vec::new() } else { vec![Step::Visit(0)] };
    let mut visits = 0;
    while let Some(step) = stack.pop().filter(|_| visits <= 2 * model.nodes.len()) {
        visits += 1;
        match step {
            Step::Visit(n) => {
                let Some(node) = model.nodes.get(n).filter(|_| wanted.holding.get(n).copied().unwrap_or(false)) else { continue };
                let [x, y, z, w] = node.plane;
                let (near, far) = if x * eye[0] + y * eye[1] + z * eye[2] - w > 0.0 { (node.front, node.back) } else { (node.back, node.front) };
                stack.extend(near.map(Step::Visit));
                stack.push(Step::Emit(n));
                stack.extend(far.map(Step::Visit));
            }
            Step::Emit(n) => {
                let start = out.len();
                out.extend(std::iter::successors(Some(n), |&m| model.nodes.get(m)?.coplanar).take(model.nodes.len()).filter(|m| wanted.wanted.contains(m)));
                out[start..].reverse();
            }
        }
    }
    out
}

pub(crate) struct WantedNodes {
    wanted: FxHashSet<usize>,
    holding: Vec<bool>,
}

impl WantedNodes {
    pub(crate) fn new(model: &Model, wanted: FxHashSet<usize>) -> Self {
        let mut parent = vec![None; model.nodes.len()];
        for (n, node) in model.nodes.iter().enumerate() {
            for child in [node.front, node.back, node.coplanar].into_iter().flatten() {
                if let Some(slot) = parent.get_mut(child) {
                    *slot = Some(n);
                }
            }
        }
        let mut holding = vec![false; model.nodes.len()];
        for &node in &wanted {
            let mut at = Some(node);
            while let Some(n) = at.filter(|&n| holding.get(n) == Some(&false)) {
                holding[n] = true;
                at = parent[n];
            }
        }
        Self { wanted, holding }
    }
}

enum Entry {
    Node(SortedBspNode),
    Actor { distance: f32 },
}

/// Where the engine inserts a translucent actor: before the first actor as near as it or nearer, or before the
/// first node whose plane has the actor on the far side from the eye; else at the near end.
fn insert_at(list: &[Entry], eye: Vec3, center: Vec3, distance: f32) -> usize {
    list.iter()
        .position(|entry| match entry {
            Entry::Actor { distance: other, .. } => *other <= distance,
            Entry::Node(node) => node.side(eye) * node.side(center) <= 0.0,
        })
        .unwrap_or(list.len())
}

type ItemKey = (Entity, MainEntity);

#[derive(Default)]
struct Partition {
    sky: Vec<(ItemKey, TransparentSortingInfo3d)>,
    level: Vec<(ItemKey, TransparentSortingInfo3d)>,
}

/// Bevy keeps a sorted item from frame to frame with the centre it had when queued, and a moved transform does not
/// queue it again: the centre is read anew, or a draw that moves (all of the sky, which follows the eye) sorts by
/// where it once was.
fn engine_translucent_order(
    views: Query<&ExtractedView>,
    mut phases: ResMut<ViewSortedRenderPhases<Transparent3d>>,
    order: Res<TranslucentOrder>,
    instances: Res<RenderMeshInstances>,
    buffers: Option<Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>>,
    meshes: Res<RenderAssets<RenderMesh>>,
    mut partition: Local<Partition>,
) {
    let center_now = |entity: MainEntity| {
        let queued = instances.render_mesh_queue_data(entity)?;
        let mesh = meshes.get(queued.mesh_asset_id())?;
        Some(get_mesh_instance_world_from_local(entity, queued.current_uniform_index, &instances, buffers.as_deref()).transform_point3(mesh.aabb_center))
    };
    for view in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else { continue };
        let ranks = order.ranks.get(&view.retained_view_entity.main_entity.id());
        order_phase(phase, &order, view.world_from_view.translation(), ranks, &center_now, &mut partition);
    }
}

fn order_phase(
    phase: &mut SortedRenderPhase<Transparent3d>,
    order: &TranslucentOrder,
    eye: Vec3,
    ranks: Option<&FxHashMap<usize, usize>>,
    center_now: &impl Fn(MainEntity) -> Option<Vec3>,
    partition: &mut Partition,
) {
    if phase.items.is_empty() {
        return;
    }
    for (&key, item) in &phase.items {
        let side = if order.sky.contains(&key.1.id()) { &mut partition.sky } else { &mut partition.level };
        let sorting = match item.sorting_info {
            TransparentSortingInfo3d::Sorted { mesh_center, depth_bias } => {
                TransparentSortingInfo3d::Sorted { mesh_center: center_now(key.1).unwrap_or(mesh_center), depth_bias }
            }
            other => other,
        };
        side.push((key, sorting));
    }
    let sky = match order.sky_place {
        Some((sky_eye, turn)) => engine_list(order, partition.sky.drain(..), order.sky_ranks.as_ref(), sky_eye, |center| sky_eye + turn * (center - eye)),
        None => engine_list(order, partition.sky.drain(..), ranks, eye, |center| center),
    };
    let listed = sky.into_iter().chain(engine_list(order, partition.level.drain(..), ranks, eye, |center| center));
    for (place, key) in listed.enumerate() {
        if let Some(item) = phase.items.get_mut(&key) {
            item.distance = place as f32;
        }
    }
    phase.items.sort_by_key(|_, item| item.sort_key());
}

fn engine_list(
    known: &TranslucentOrder,
    items: impl IntoIterator<Item = (ItemKey, TransparentSortingInfo3d)>,
    ranks: Option<&FxHashMap<usize, usize>>,
    eye: Vec3,
    true_center: impl Fn(Vec3) -> Vec3,
) -> Vec<ItemKey> {
    let (mut terrain, mut nodes, mut after) = (Vec::new(), Vec::new(), Vec::new());
    let mut actors: FxHashMap<Entity, (Vec3, Vec<(u32, ItemKey)>)> = FxHashMap::default();
    for (key, sorting) in items {
        let section = known.nodes.get(&key.1.id()).filter(|section| ranks.is_some_and(|ranks| ranks.contains_key(&section.node)));
        match (section, known.draws.get(&key.1.id()), sorting) {
            (Some(section), ..) => nodes.push((ranks.and_then(|ranks| ranks.get(&section.node)).copied().unwrap_or_default(), key, *section)),
            (None, Some(TranslucentDraw::Terrain(layer)), _) => terrain.push((*layer, key)),
            (None, Some(TranslucentDraw::AfterScene), _) | (None, _, TransparentSortingInfo3d::AlwaysOnTop) => after.push(key),
            (None, part, TransparentSortingInfo3d::Sorted { mesh_center, .. }) => {
                let (actor, order) = match part {
                    Some(&TranslucentDraw::Part { actor, order }) => (actor, order),
                    _ => (key.1.id(), 0),
                };
                actors.entry(actor).or_insert_with(|| (true_center(mesh_center), Vec::new())).1.push((order, key));
            }
        }
    }
    terrain.sort_by_key(|(layer, _)| *layer);
    nodes.sort_by_key(|(rank, ..)| *rank);
    let mut actors: Vec<_> = actors.into_values().map(|(center, parts)| (center.distance(eye), center, parts)).collect();
    actors.sort_by(|a, b| b.0.total_cmp(&a.0));
    let (mut list, mut keys): (Vec<Entry>, Vec<Vec<_>>) = nodes.into_iter().map(|(_, key, node)| (Entry::Node(node), vec![key])).unzip();
    for (distance, center, mut parts) in actors {
        parts.sort_by_key(|(order, _)| *order);
        let at = insert_at(&list, eye, center, distance);
        list.insert(at, Entry::Actor { distance });
        keys.insert(at, parts.into_iter().map(|(_, key)| key).collect());
    }
    terrain.into_iter().map(|(_, key)| key).chain(keys.into_iter().flatten()).chain(after).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(plane: Vec4) -> Entry {
        Entry::Node(SortedBspNode { node: 0, plane })
    }

    #[test]
    fn an_actor_goes_before_nearer_actors_and_nodes_it_is_behind() {
        let eye = Vec3::ZERO;
        let far_wall = node(Vec4::new(0.0, 0.0, 1.0, 100.0));
        let list = [Entry::Actor { distance: 300.0 }, far_wall, Entry::Actor { distance: 50.0 }];
        assert_eq!(insert_at(&list, eye, Vec3::Z * 500.0, 500.0), 0);
        assert_eq!(insert_at(&list, eye, Vec3::Z * 200.0, 200.0), 1, "behind the wall plane: before it");
        assert_eq!(insert_at(&list, eye, Vec3::Z * 80.0, 80.0), 2, "in front of the wall, farther than the near actor");
        assert_eq!(insert_at(&list, eye, Vec3::Z * 10.0, 10.0), 3);
    }

    #[test]
    fn the_walk_lists_the_far_side_first() {
        let mut model = crate::view::two_zone_model();
        model.nodes[0].coplanar = None;
        let wanted = WantedNodes::new(&model, FxHashSet::from_iter([0]));
        assert_eq!(back_to_front(&model, [0.0, 0.0, 10.0], &wanted), [0]);
        model.nodes.push(p2_assets::BspNode { plane: [0.0, 0.0, 1.0, 50.0], ..model.nodes[0] });
        model.nodes[0].front = Some(1);
        let wanted = WantedNodes::new(&model, FxHashSet::from_iter([0, 1]));
        assert_eq!(back_to_front(&model, [0.0, 0.0, -10.0], &wanted), [1, 0], "eye below both planes: the upper node is farther");
        assert_eq!(back_to_front(&model, [0.0, 0.0, 100.0], &wanted), [0, 1], "eye above both: the lower node is farther");
    }

    fn item(id: u32, sorting_info: TransparentSortingInfo3d) -> (ItemKey, Transparent3d) {
        let entity = Entity::from_raw_u32(id).unwrap();
        let key = (entity, MainEntity::from(entity));
        let item = Transparent3d {
            sorting_info,
            distance: -1.0,
            pipeline: bevy::render::render_resource::CachedRenderPipelineId::INVALID,
            entity: key,
            draw_function: bevy::render::render_phase::DrawFunctionId(0),
            batch_range: 0..1,
            extra_index: bevy::render::render_phase::PhaseItemExtraIndex::None,
            indexed: false,
        };
        (key, item)
    }

    #[test]
    fn an_empty_view_is_skipped_and_the_next_view_is_ordered() {
        let order = TranslucentOrder::default();
        let mut partition = Partition::default();
        let mut empty = SortedRenderPhase::<Transparent3d>::default();
        order_phase(&mut empty, &order, Vec3::ZERO, None, &|_| None, &mut partition);
        assert!(empty.items.is_empty());

        let sorted = |z: f32| TransparentSortingInfo3d::Sorted { mesh_center: Vec3::Z * z, depth_bias: 0.0 };
        let mut phase = SortedRenderPhase::<Transparent3d>::default();
        phase.items.extend([item(1, sorted(10.0)), item(2, TransparentSortingInfo3d::AlwaysOnTop), item(3, sorted(500.0))]);
        order_phase(&mut phase, &order, Vec3::ZERO, None, &|_| None, &mut partition);
        let drawn: Vec<_> = phase.items.values().map(|item| (item.entity.0.index_u32(), item.distance)).collect();
        assert_eq!(drawn, [(3, 0.0), (1, 1.0), (2, 2.0)]);
        assert!(partition.sky.is_empty() && partition.level.is_empty());
    }

    #[test]
    fn a_draw_sorts_by_where_it_is_now_not_where_it_was_queued() {
        let order = TranslucentOrder::default();
        let mut partition = Partition::default();
        let sorted = |z: f32| TransparentSortingInfo3d::Sorted { mesh_center: Vec3::Z * z, depth_bias: 0.0 };
        let mut phase = SortedRenderPhase::<Transparent3d>::default();
        phase.items.extend([item(1, sorted(10.0)), item(2, sorted(500.0))]);
        let moved = |entity: MainEntity| (entity.id().index_u32() == 1).then_some(Vec3::Z * 900.0);
        order_phase(&mut phase, &order, Vec3::ZERO, None, &moved, &mut partition);
        let drawn: Vec<_> = phase.items.values().map(|item| item.entity.0.index_u32()).collect();
        assert_eq!(drawn, [1, 2]);
    }
}
