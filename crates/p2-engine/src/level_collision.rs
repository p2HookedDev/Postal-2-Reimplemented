use std::sync::Arc;

use bevy::ecs::prelude::Resource;
use p2_assets::{Model, Terrain};
use p2_collision::{BspHulls, Hit, MeshCollision, Sides, line_check, mesh_line_check, terrain_line_check};
use p2_math::Vector;

pub struct StaticCollision {
    pub model: Model,
    pub hulls: BspHulls,
    pub terrains: Vec<Terrain>,
    pub meshes: Vec<MeshCollision>,
}

impl StaticCollision {
    pub fn first_hit(&self, start: Vector, end: Vector) -> Option<Hit> {
        let bsp = line_check(&self.model, start, end);
        let terrain = self.terrains.iter().filter_map(|t| terrain_line_check(t, start, end, Sides::Both));
        let mesh = mesh_line_check(&self.meshes, start, end);
        bsp.into_iter().chain(terrain).chain(mesh).min_by(|a, b| a.time.total_cmp(&b.time))
    }
}

#[derive(Resource, Default)]
pub struct LevelCollision(pub Option<Arc<StaticCollision>>);

impl LevelCollision {
    pub fn model(&self) -> Option<&Model> {
        self.0.as_ref().map(|collision| &collision.model)
    }
}
