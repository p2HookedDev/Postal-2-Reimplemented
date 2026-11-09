//! The engine's back-to-front list of translucent draws: BSP nodes whose surfaces need sorting in BSP order from the
//! eye, and each translucent actor placed by the distance to its centre against actors and by plane side against
//! nodes. It replaces Bevy's sort by depth along the view.

use bevy::app::{App, Plugin, PostUpdate};
use bevy::camera::Camera3d;
use bevy::core_pipeline::core_3d::{Transparent3d, TransparentSortingInfo3d};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::prelude::{Component, Entity, Query, Res, ResMut, Resource, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Vec3, Vec4};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_phase::{SortedPhaseItem, ViewSortedRenderPhases, sort_phase_system};
use bevy::render::view::ExtractedView;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::transform::TransformSystems;
use bevy::transform::components::GlobalTransform;
use p2_assets::Model;
use p2_bevy_assets::to_bevy;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::level_collision::LevelCollision;
use crate::shown_scene::ShownScene;

/// A translucent section holding one BSP node's surfaces, with the node's plane in the drawn space: `xyz · p - w`.
#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub struct SortedBspNode {
    node: usize,
    plane: Vec4,
}

impl SortedBspNode {
    /// `plane` is the node's plane in the level's own coordinates; the drawn level is moved by `-center`.
    pub fn new(node: usize, plane: [f32; 4], center: Vec3) -> Self {
        let normal = Vec3::from(to_bevy([plane[0], plane[1], plane[2]]));
        Self { node, plane: normal.extend(plane[3] - normal.dot(center)) }
    }

    fn side(&self, point: Vec3) -> f32 {
        self.plane.truncate().dot(point) - self.plane.w
    }
}

/// How a blended draw that is not a BSP node joins the engine's list.
#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub enum TranslucentDraw {
    /// A terrain layer: drawn with the world, before the list, in layer order.
    Terrain(u32),
    /// One of an actor's draws (an emitter): the actor enters the list once, its draws follow in this order.
    Part { actor: Entity, order: u32 },
    /// A corona: drawn after the scene.
    AfterScene,
}

#[derive(Resource, Clone, Default, PartialEq, ExtractResource)]
#[extract_app(RenderApp)]
struct TranslucentOrder {
    /// Per camera: each sorted node's place in the back-to-front BSP walk from that camera's eye.
    ranks: FxHashMap<Entity, FxHashMap<usize, usize>>,
    nodes: FxHashMap<Entity, SortedBspNode>,
    draws: FxHashMap<Entity, TranslucentDraw>,
}

pub struct TranslucentOrderPlugin;

impl Plugin for TranslucentOrderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TranslucentOrder>()
            .add_plugins(ExtractResourcePlugin::<TranslucentOrder>::default())
            .add_systems(PostUpdate, rank_sorted_nodes.after(TransformSystems::Propagate));
        if let Some(render) = app.get_sub_app_mut(RenderApp) {
            render.add_systems(Render, engine_translucent_order.in_set(RenderSystems::PhaseSort).after(sort_phase_system::<Transparent3d>));
        }
    }
}

fn rank_sorted_nodes(
    shown: Res<ShownScene>,
    level: Res<LevelCollision>,
    cameras: Query<(Entity, &GlobalTransform), With<Camera3d>>,
    sections: Query<(Entity, &SortedBspNode)>,
    draws: Query<(Entity, &TranslucentDraw)>,
    mut order: ResMut<TranslucentOrder>,
) {
    let nodes: FxHashMap<Entity, SortedBspNode> = sections.iter().map(|(entity, node)| (entity, *node)).collect();
    let ranks = match level.model().filter(|_| !nodes.is_empty()) {
        Some(model) => {
            let wanted: FxHashSet<usize> = nodes.values().map(|section| section.node).collect();
            let rank = |eye: Vec3| {
                let eye = to_bevy((eye - shown.origin).to_array());
                back_to_front(model, eye, &wanted).into_iter().enumerate().map(|(rank, node)| (node, rank)).collect()
            };
            cameras.iter().map(|(camera, transform)| (camera, rank(transform.translation()))).collect()
        }
        None => FxHashMap::default(),
    };
    let draws = draws.iter().map(|(entity, draw)| (entity, *draw)).collect();
    order.set_if_neq(TranslucentOrder { ranks, nodes, draws });
}

/// The wanted nodes in the order the engine's list holds them: it pushes each onto the head while walking the tree
/// front to back, so the list runs back to front, a node's coplanar nodes ahead of it.
pub(crate) fn back_to_front(model: &Model, eye: [f32; 3], wanted: &FxHashSet<usize>) -> Vec<usize> {
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
                let Some(node) = model.nodes.get(n) else { continue };
                let [x, y, z, w] = node.plane;
                let (near, far) = if x * eye[0] + y * eye[1] + z * eye[2] - w > 0.0 { (node.front, node.back) } else { (node.back, node.front) };
                stack.extend(near.map(Step::Visit));
                stack.push(Step::Emit(n));
                stack.extend(far.map(Step::Visit));
            }
            Step::Emit(n) => {
                let start = out.len();
                out.extend(std::iter::successors(Some(n), |&m| model.nodes.get(m)?.coplanar).take(model.nodes.len()).filter(|m| wanted.contains(m)));
                out[start..].reverse();
            }
        }
    }
    out
}

enum Entry {
    Node(SortedBspNode),
    Actor { distance: f32 },
}

/// Where the engine inserts a translucent actor: before the first actor as near as it or nearer, or before the first
/// node whose plane has the actor on the far side from the eye; else at the near end.
fn insert_at(list: &[Entry], eye: Vec3, center: Vec3, distance: f32) -> usize {
    list.iter()
        .position(|entry| match entry {
            Entry::Actor { distance: other, .. } => *other <= distance,
            Entry::Node(node) => node.side(eye) * node.side(center) <= 0.0,
        })
        .unwrap_or(list.len())
}

fn engine_translucent_order(views: Query<&ExtractedView>, mut phases: ResMut<ViewSortedRenderPhases<Transparent3d>>, order: Res<TranslucentOrder>) {
    for view in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else { continue };
        let eye = view.world_from_view.translation();
        let ranks = order.ranks.get(&view.retained_view_entity.main_entity.id());
        let (mut terrain, mut nodes, mut after) = (Vec::new(), Vec::new(), Vec::new());
        let mut actors: FxHashMap<Entity, (Vec3, Vec<(u32, _)>)> = FxHashMap::default();
        for (&key, item) in &phase.items {
            let section = order.nodes.get(&key.1.id()).filter(|section| ranks.is_some_and(|ranks| ranks.contains_key(&section.node)));
            match (section, order.draws.get(&key.1.id()), item.sorting_info) {
                (Some(section), ..) => nodes.push((ranks.and_then(|ranks| ranks.get(&section.node)).copied().unwrap_or_default(), key, *section)),
                (None, Some(TranslucentDraw::Terrain(layer)), _) => terrain.push((*layer, key)),
                (None, Some(TranslucentDraw::AfterScene), _) | (None, _, TransparentSortingInfo3d::AlwaysOnTop) => after.push(key),
                (None, part, TransparentSortingInfo3d::Sorted { mesh_center, .. }) => {
                    let (actor, order) = match part {
                        Some(&TranslucentDraw::Part { actor, order }) => (actor, order),
                        _ => (key.1.id(), 0),
                    };
                    actors.entry(actor).or_insert_with(|| (mesh_center, Vec::new())).1.push((order, key));
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
        let listed = keys.into_iter().flatten();
        let place: FxHashMap<_, usize> = terrain.into_iter().map(|(_, key)| key).chain(listed).chain(after).enumerate().map(|(place, key)| (key, place)).collect();
        for (key, item) in phase.items.iter_mut() {
            item.distance = place.get(key).copied().unwrap_or_default() as f32;
        }
        phase.items.sort_by_key(|_, item| item.sort_key());
    }
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
        let mut model = crate::mesh_view::tests::two_zones();
        model.nodes[0].coplanar = None;
        let wanted = FxHashSet::from_iter([0]);
        assert_eq!(back_to_front(&model, [0.0, 0.0, 10.0], &wanted), [0]);
        model.nodes.push(p2_assets::BspNode { plane: [0.0, 0.0, 1.0, 50.0], ..model.nodes[0] });
        model.nodes[0].front = Some(1);
        let wanted = FxHashSet::from_iter([0, 1]);
        assert_eq!(back_to_front(&model, [0.0, 0.0, -10.0], &wanted), [1, 0], "eye below both planes: the upper node is farther");
        assert_eq!(back_to_front(&model, [0.0, 0.0, 100.0], &wanted), [0, 1], "eye above both: the lower node is farther");
    }
}
