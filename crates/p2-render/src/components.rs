use bevy::camera::visibility::RenderLayers;
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::prelude::{Component, Entity, Resource};
use bevy::ecs::world::DeferredWorld;
use bevy::math::{Mat4, Quat, Vec3};
use bevy::transform::components::Transform;
use p2_bevy_assets::{rotator_matrix, unreal_mesh_matrix, unreal_to_bevy, unreal_transform};
use p2_math::{Rotator, Vector};
use p2_vm::ObjectId;

pub const SKY_LAYER: usize = 2;

pub const OVERLAY_LAYERS: [usize; 4] = [1, 4, 5, 6];

/// The layer the view camera adds while modern graphics are on, shared by the sun and fills so the view sees them
/// while the sun keeps its own casters (Bevy: a light's casters are the meshes sharing a layer with it). No mesh
/// is on it. Layers 0 to 6 are the view, overlay, sky and gamma ones (`render/view.rs`, `gamma.rs`).
pub const SUN_VIEW_LAYER: usize = 7;

pub const CASTER_LAYER: usize = 8;

pub const MOVER_LAYER: usize = 9;

pub const STATIC_CASTER_LAYER: usize = 10;

/// The layer it adds to a placed mesh that stands still. Its lamp shadow is drawn with the movers': the lamps'
/// atlas holds only what can never move.
pub const RESTING_PROP_LAYER: usize = 11;

/// Where a draw waits, in place of layer 0, while its actor is kept only to cast the sun's shadow from a zone the
/// eye cannot see. No camera sees it.
pub const UNSEEN_LAYER: usize = 12;

#[derive(Component)]
pub struct SunShadow;

#[derive(Component, Clone, Copy)]
pub struct FollowActor {
    pub actor: ObjectId,
    pub scale: Vector,
    pub pre_pivot: Vector,
    pub skeletal: Option<(Vector, Vector, Rotator)>,
}

impl FollowActor {
    pub fn mesh_matrix(&self, location: Vector, rotation: Rotator) -> Option<Mat4> {
        let (mesh_scale, origin, rot_origin) = self.skeletal?;
        Some(unreal_mesh_matrix(location, rotator_matrix(rotation), self.scale, self.pre_pivot, mesh_scale, origin, rot_origin))
    }

    pub fn transform(&self, location: Vector, rotation: Rotator) -> Transform {
        match self.mesh_matrix(location, rotation) {
            Some(matrix) => unreal_to_bevy(matrix),
            None => unreal_transform(location, rotation, self.scale, self.pre_pivot),
        }
    }
}

#[derive(Component, Clone, Copy)]
pub struct CanvasDrawn(pub usize);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
#[component(on_insert = follow_overlay_slot)]
#[require(RenderLayers)]
pub struct OverlaySlot(pub Option<usize>);

fn follow_overlay_slot(mut world: DeferredWorld, context: HookContext) {
    let Some(&OverlaySlot(slot)) = world.get::<OverlaySlot>(context.entity) else { return };
    if let Some(mut layers) = world.get_mut::<RenderLayers>(context.entity) {
        *layers = slot.and_then(|slot| OVERLAY_LAYERS.get(slot)).map_or_else(RenderLayers::default, |&layer| RenderLayers::layer(layer));
    }
}

#[derive(Component, Clone, Copy)]
pub struct FollowBone {
    pub actor: ObjectId,
    pub base: ObjectId,
    pub bone: usize,
    pub body: Mat4,
    pub base_follow: FollowActor,
    pub relative_location: Vector,
    pub relative_rotation: Rotator,
    pub scale: Vector,
    pub pre_pivot: Vector,
    pub mesh_scale: Vector,
    pub origin: Vector,
    pub rot_origin: Rotator,
}

#[derive(Component, Clone, Copy, Default)]
pub struct SceneView {
    pub position: Vec3,
    pub rotation: Quat,
}

#[derive(Resource, Default)]
pub struct OverlayLook(pub Option<Transform>);

#[derive(Component)]
pub(crate) struct CanvasLayer(pub(crate) usize);

#[derive(Component, Clone, Copy)]
pub struct ActorModel(pub ObjectId);

#[derive(Component)]
pub struct BaseModel(pub Option<Entity>);

#[derive(Component)]
pub struct MapScoped;

#[derive(Component)]
pub struct LeftCanvasActor;
