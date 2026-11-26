use p2_assets::{Model, Terrain};
use p2_collision::{BspHulls, MeshCollision, MeshSet};

use crate::world::level_collision::{LevelAlbedos, MaskedCaster, StaticCollision};
use crate::world::scene::Scene;

mod assets;
mod bsp;
mod materials;
mod mesh_sections;
mod static_meshes;
mod switches;
mod terrain;

pub use assets::{ActorTexture, PendingAssets, SceneAssets};
pub use bsp::{BspParts, bsp_groups, bsp_models};
pub use materials::{MaterialCache, SwitchImage, mark_restyled, material_images, projector_image, switched_images};
pub use mesh_sections::{SectionChunks, sections_of_static_mesh, skeletal_draw_meshes, skeletal_mesh_sections, skeletal_sections};
pub use switches::{MaterialSwitches, switch_materials};
pub use static_meshes::{MeshPlacing, Placement, StaticLevel, StaticParts, antiportal_actors, resolve_mesh_paths, static_models};
pub use terrain::{TerrainParts, TerrainZones, terrain_models};

pub fn static_collision(
    model: Model,
    terrains: Vec<Terrain>,
    meshes: Vec<MeshCollision>,
    lamp_casters: Vec<[[f32; 3]; 3]>,
    lamp_masked: Vec<MaskedCaster>,
    albedos: LevelAlbedos,
) -> StaticCollision {
    StaticCollision {
        hulls: BspHulls::new(&model),
        model,
        terrains,
        mesh_set: MeshSet::new(&meshes),
        meshes,
        lamp_casters,
        lamp_masked,
        albedos,
    }
}

pub fn bake_lamps(scene: &mut Scene) -> Option<crate::lighting::lamps::LoadBake> {
    let (Some(collision), Some(environment)) = (&scene.collision, &scene.lighting) else { return None };
    let (bakes, how) = crate::lighting::lamps::bake_for_load(environment, collision, &scene.shadow_surfaces, -scene.center)?;
    scene.light_bakes = Some(bakes);
    Some(how)
}

#[cfg(test)]
mod tests;
