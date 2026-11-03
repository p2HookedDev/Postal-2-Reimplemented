mod common;

use common::game_dir;
use std::path::Path;

use bevy::app::{App, PostUpdate};
use bevy::asset::Assets;
use bevy::camera::visibility::RenderLayers;
use bevy::ecs::prelude::{IntoScheduleConfigs, ResMut, Resource};
use bevy::mesh::{Mesh, Mesh3d, VertexAttributeValues};
use bevy::pbr::MeshMaterial3d;
use p2_engine::render::actor_light::{ActorLit, ActorLitMaterial, Shading};
use p2_engine::map::{SceneTargets, Session};
use p2_engine::mesh_view::{MeshView, SKY_LAYER};
use p2_engine::play::PlaySystems;

#[derive(Resource)]
struct Holder {
    session: Session,
    map: String,
    frame: u32,
}

fn played(root: &Path, map: &str) -> App {
    let mut app = common::played_app();
    app.init_resource::<MeshView>()
        .insert_resource(Holder { session: Session::new(root.to_path_buf()), map: map.to_owned(), frame: 0 })
        .add_systems(
            PostUpdate,
            (|mut holder: ResMut<Holder>, mut t: SceneTargets| {
                if holder.frame == 0 {
                    let map = Path::new("Maps").join(format!("{}.fuk", holder.map)).display().to_string();
                    if let Err(error) = holder.session.load_map(&map, &mut t) {
                        panic!("{error}");
                    }
                } else {
                    let items = t.play.world().map(|world| world.hud_items().to_vec()).unwrap_or_default();
                    t.play.set_canvas_size([1024.0, 768.0]);
                    holder.session.present(&items, &mut t);
                }
                holder.frame += 1;
            })
            .in_set(PlaySystems::Present),
        );
    app
}

fn shown(linear: f32) -> f32 {
    (linear.max(0.0).powf(1.0 / 2.2) * 127.5).min(255.0)
}

#[test]
fn moving_actors_lit_where_they_are() {
    let Some(root) = game_dir() else { return };
    for map in ["Suburbs-3", "Church"] {
        let mut app = played(&root, map);
        for _ in 0..8 {
            app.update();
        }
        let world = app.world_mut();
        let mut query = world.query::<(&ActorLit, &Mesh3d, Option<&MeshMaterial3d<ActorLitMaterial>>)>();
        let lit: Vec<_> = query.iter(world).map(|(lit, mesh, material)| (lit.clone(), mesh.0.clone(), material.map(|m| m.0.clone()))).collect();
        let (meshes, materials) = (world.resource::<Assets<Mesh>>(), world.resource::<Assets<ActorLitMaterial>>());
        let (mut vertex, mut tint, mut unlit_yet, mut dark) = (0, 0, 0, 0);
        let mut brightness = Vec::new();
        for (lit, mesh, material) in &lit {
            if !lit.is_lit() {
                unlit_yet += 1;
                continue;
            }
            let value = match lit.shading {
                Shading::Vertices { .. } => {
                    vertex += 1;
                    let Some(VertexAttributeValues::Float32x4(colors)) = meshes.get(mesh).and_then(|m| m.attribute(Mesh::ATTRIBUTE_COLOR)) else {
                        panic!("{map}: a vertex-lit model without colours");
                    };
                    colors.iter().map(|c| (shown(c[0]) + shown(c[1]) + shown(c[2])) / 3.0).sum::<f32>() / colors.len().max(1) as f32
                }
                Shading::Tint => {
                    tint += 1;
                    let material = material.as_ref().and_then(|material| materials.get(material)).expect("tinted material");
                    let ambient = material.extension.lighting.ambient;
                    (shown(ambient.x) + shown(ambient.y) + shown(ambient.z)) / 3.0
                }
            };
            dark += usize::from(value < 16.0);
            brightness.push(value);
        }
        let mut sky = world.query::<(&RenderLayers, &Mesh3d)>();
        let sky_models = sky.iter(world).filter(|(layers, _)| layers.intersects(&RenderLayers::layer(SKY_LAYER))).count();
        assert!(vertex > 0 && tint > 0, "{vertex} {tint}");
        assert!(unlit_yet * 10 <= lit.len(), "{unlit_yet} of {} not lit after 8 frames", lit.len());
        assert!(dark * 20 <= brightness.len(), "{dark} of {} near black", brightness.len());
        assert!(sky_models > 0);
    }
}
