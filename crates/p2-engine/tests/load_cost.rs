mod common;

use common::game_dir;
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use bevy::MinimalPlugins;
use bevy::app::{App, Update};
use bevy::asset::{AssetApp, AssetPlugin, Assets};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::audio::AudioSource;
use bevy::ecs::prelude::{ResMut, Resource};
use bevy::image::Image;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use p2_engine::animation::Animated;
use p2_engine::map::{Fluids, Hud, MaterialSwitches, Particles, Projectors, SceneTargets, Session, SpawnedActors};
use p2_engine::mesh_view::{MeshViewPlugin, MeshViewSystems};
use p2_engine::play::{FrameTimes, Play};

#[derive(Resource)]
struct Holder {
    session: Session,
    begun: Option<Instant>,
    finish: Option<Duration>,
}

#[test]
#[ignore = "benchmark"]
fn monday_scene_build_cost() {
    let Some(root) = game_dir() else { return };
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_asset::<AudioSource>()
        .add_plugins(MeshViewPlugin)
        .init_resource::<Animated>()
        .init_resource::<Play>()
        .init_resource::<FrameTimes>()
        .init_resource::<Fluids>()
        .init_resource::<MaterialSwitches>()
        .init_resource::<Projectors>()
        .init_resource::<Particles>()
        .init_resource::<SpawnedActors>()
        .init_resource::<Hud>()
        .insert_resource(Holder { session: Session::new(root), begun: None, finish: None })
        .add_systems(Update, (|mut holder: ResMut<Holder>, mut t: SceneTargets| {
            let holder = &mut *holder;
            if holder.finish.is_some() {
                return;
            }
            let Some(begun) = holder.begun else {
                holder.begun = Some(Instant::now());
                let map = std::path::Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
                holder.session.begin_load(&map, None, &mut t).unwrap();
                return;
            };
            let started = Instant::now();
            if let Some(result) = holder.session.finish_load(&mut t) {
                let finish = started.elapsed();
                eprintln!("load {:?} in all, finish_load (main thread) {finish:?}", begun.elapsed());
                eprintln!("{:?}", result.map(|s| s.chars().take(400).collect::<String>()));
                holder.finish = Some(finish);
            }
        })
        .before(MeshViewSystems::Apply));
    loop {
        let started = Instant::now();
        app.update();
        if app.world().resource::<Holder>().finish.is_some() {
            eprintln!("frame finishing the load and spawning the scene {:?}", started.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    let started = Instant::now();
    app.update();
    eprintln!("next frame {:?}", started.elapsed());

    let world = app.world_mut();
    let mut query = world.query::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>)>();
    let (mut entities, mut meshes, mut materials) = (0, HashSet::new(), HashSet::new());
    for (mesh, material) in query.iter(world) {
        entities += 1;
        meshes.insert(mesh.id());
        materials.insert(material.id());
    }
    let all = world.entities().count_spawned();
    eprintln!(
        "{all} entities; {entities} with Mesh3d, {} distinct meshes, {} distinct materials; assets: {} meshes, {} materials",
        meshes.len(),
        materials.len(),
        world.resource::<Assets<Mesh>>().len(),
        world.resource::<Assets<StandardMaterial>>().len()
    );
    let mut by_format: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let (mut bytes, mut rgba) = (0, 0);
    let (mut seen, mut duplicates, mut duplicate_bytes) = (HashSet::new(), 0, 0);
    for (_, image) in world.resource::<Assets<Image>>().iter() {
        let size = image.data.as_ref().map_or(0, Vec::len);
        if !seen.insert(image.data.clone()) {
            (duplicates, duplicate_bytes) = (duplicates + 1, duplicate_bytes + size);
        }
        let entry = by_format.entry(format!("{:?}", image.texture_descriptor.format)).or_default();
        (entry.0, entry.1) = (entry.0 + 1, entry.1 + size);
        bytes += size;
        let (w, h) = (image.texture_descriptor.size.width as usize, image.texture_descriptor.size.height as usize);
        rgba += if image.texture_descriptor.mip_level_count > 1 { w * h * 4 * 4 / 3 } else { w * h * 4 };
    }
    eprintln!(
        "images: {} MB uploaded ({} MB as RGBA8), {duplicates} duplicates ({} MB); by format (count, bytes): {by_format:?}",
        bytes >> 20,
        rgba >> 20,
        duplicate_bytes >> 20
    );
}
