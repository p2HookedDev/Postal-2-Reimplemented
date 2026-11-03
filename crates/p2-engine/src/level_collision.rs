use std::sync::Arc;

use bevy::ecs::prelude::Resource;
use p2_assets::{Model, Terrain};
use p2_collision::MeshCollision;

pub struct StaticCollision {
    pub model: Model,
    pub terrains: Vec<Terrain>,
    pub meshes: Vec<MeshCollision>,
}

#[derive(Resource, Default)]
pub struct LevelCollision(pub Option<Arc<StaticCollision>>);

impl LevelCollision {
    pub fn model(&self) -> Option<&Model> {
        self.0.as_ref().map(|collision| &collision.model)
    }
}
