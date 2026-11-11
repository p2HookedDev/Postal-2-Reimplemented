use bevy::ecs::prelude::{Component, Resource};
use bevy::math::{Mat4, Quat, Vec3};
use bevy::transform::components::Transform;
use p2_bevy_assets::{rotator_matrix, unreal_mesh_matrix, unreal_to_bevy, unreal_transform};
use p2_math::{Rotator, Vector};
use p2_vm::ObjectId;

pub const SKY_LAYER: usize = 2;

pub const OVERLAY_LAYERS: [usize; 4] = [1, 4, 5, 6];

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

/// The view the game last gave, while the scene's camera has turned on with the mouse and glides between ticks: what
/// the game draws over the scene (the held weapon) was placed for this view, so its cameras keep it.
#[derive(Resource, Default)]
pub struct OverlayLook(pub Option<Transform>);

/// An egui context on a camera ordered just before the slot's `OverlayCamera`, sharing its target texture, so the
/// `DrawActor` call draws over the 2D items painted before it, as in retail.
#[derive(Component)]
pub(crate) struct CanvasLayer(pub(crate) usize);

#[derive(Component, Clone, Copy)]
pub struct ActorModel(pub ObjectId);

/// On the root of what is made for the map being played and the scene does not own.
#[derive(Component)]
pub(crate) struct MapScoped;

/// A HUD-drawn model of the map left stays on the load screen, as the engine's frame buffer keeps it.
#[derive(Component)]
pub(crate) struct LeftCanvasActor;
