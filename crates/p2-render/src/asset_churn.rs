use bevy::app::{App, Plugin, Update};
use bevy::asset::{Asset, AssetEvent, Assets};
use bevy::ecs::prelude::{Local, MessageReader, Query, Res, With, resource_exists};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::mesh::Mesh;
use bevy::time::{Real, Time};
use p2_host::frame_times::{MESH_WRITERS, MESH_WRITES};

use crate::actor_light::ActorLitMaterial;
use crate::blended::TerrainLayerMaterial;
use crate::mesh_light::MeshLitMaterial;
use crate::terrain_material::TerrainMaterial;

pub struct AssetChurnPlugin;

impl Plugin for AssetChurnPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, log_asset_churn.run_if(resource_exists::<Assets<ActorLitMaterial>>).run_if(resource_exists::<Assets<MeshLitMaterial>>));
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn log_asset_churn(
    time: Res<Time<Real>>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    actor_materials: Res<Assets<ActorLitMaterial>>,
    mesh_materials: Res<Assets<MeshLitMaterial>>,
    terrain_passes: Option<Res<Assets<TerrainMaterial>>>,
    terrain_layers: Option<Res<Assets<TerrainLayerMaterial>>>,
    decals: Query<(), With<ClusteredDecal>>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    mut actor_events: MessageReader<AssetEvent<ActorLitMaterial>>,
    mut mesh_material_events: MessageReader<AssetEvent<MeshLitMaterial>>,
    mut counts: Local<[u32; 3]>,
    mut since: Local<f32>,
) {
    fn changed<A: Asset>(events: &mut MessageReader<AssetEvent<A>>) -> u32 {
        events.read().filter(|event| matches!(event, AssetEvent::Modified { .. })).count() as u32
    }
    counts[0] += changed(&mut mesh_events);
    counts[1] += changed(&mut actor_events);
    counts[2] += changed(&mut mesh_material_events);
    *since += time.delta_secs();
    if *since < 1.0 {
        return;
    }
    let per_second = 1.0 / std::mem::take(&mut *since);
    let [mesh_writes, actor_writes, material_writes] = std::mem::take(&mut *counts).map(|count| (count as f32 * per_second) as u32);
    bevy::log::info!(
        "assets, changed a second: meshes {} ({mesh_writes}), images {}, actor materials {} ({actor_writes}), mesh materials {} ({material_writes}); one-pass terrains {}, terrain layer passes {}, decals {}",
        meshes.len(),
        images.len(),
        actor_materials.len(),
        mesh_materials.len(),
        terrain_passes.map_or(0, |store| store.len()),
        terrain_layers.map_or(0, |store| store.len()),
        decals.iter().count()
    );
    let by: Vec<String> = MESH_WRITERS
        .iter()
        .zip(&MESH_WRITES)
        .map(|(name, count)| format!("{name} {}", (count.swap(0, std::sync::atomic::Ordering::Relaxed) as f32 * per_second) as u32))
        .collect();
    bevy::log::info!("mesh writes a second by writer: {}", by.join(", "));
}
