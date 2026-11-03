mod common;

use common::game_dir;
use std::path::Path;

use bevy::app::{App, PostUpdate};
use bevy::asset::Assets;
use bevy::camera::visibility::Visibility;
use bevy::ecs::prelude::{IntoScheduleConfigs, ResMut, Resource};
use bevy::math::{UVec2, Vec3};
use bevy::mesh::{Mesh, Mesh3d, VertexAttributeValues};
use bevy::pbr::MeshMaterial3d;
use bevy::transform::components::{GlobalTransform, Transform};
use p2_engine::map::{SceneTargets, Session};
use p2_engine::render::particle_material::ParticleMaterial;
use p2_engine::mesh_view::SceneView;
use p2_engine::play::{PlaySystems, PlayerInput};

#[derive(Resource)]
struct Holder {
    session: Session,
    frame: u32,
}

fn played(root: &Path) -> App {
    let mut app = common::played_app();
    app.insert_resource(Holder { session: Session::new(root.to_path_buf()), frame: 0 })
        .add_systems(
            PostUpdate,
            (|mut holder: ResMut<Holder>, mut t: SceneTargets| {
                t.view.viewport = Some((UVec2::ZERO, UVec2::new(1024, 768)));
                if holder.frame == 0 {
                    let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
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
    app.world_mut().resource_mut::<PlayerInput>().playing = true;
    app
}

#[test]
fn urine_stream_drawn_ahead_of_the_view() {
    let Some(root) = game_dir() else { return };
    let mut app = played(&root);
    let step = |app: &mut App, held: Vec<&'static str>, mouse: [f32; 2]| {
        let mut input = app.world_mut().resource_mut::<PlayerInput>();
        (input.held, input.mouse) = (held, mouse);
        app.update();
    };
    for _ in 0..600 {
        step(&mut app, Vec::new(), [0.0; 2]);
    }
    for turn in [0.0, 400.0, 700.0] {
        for _ in 0..10 {
            step(&mut app, Vec::new(), [turn / 10.0, 0.0]);
        }
        if turn == 0.0 {
            step(&mut app, vec!["R"], [0.0; 2]);
            for _ in 0..90 {
                step(&mut app, Vec::new(), [0.0; 2]);
            }
        }
        for _ in 0..45 {
            step(&mut app, vec!["LeftMouse"], [0.0; 2]);
        }
        let world = app.world_mut();
        let camera = world.query_filtered::<&Transform, bevy::ecs::query::With<SceneView>>().iter(world).next().copied().expect("camera");
        let forward = camera.rotation * Vec3::NEG_Z;
        let mut drawn = Vec::new();
        let mut query = world.query::<(&Mesh3d, &MeshMaterial3d<ParticleMaterial>, &Visibility, &GlobalTransform)>();
        for (mesh, _, visibility, placed) in query.iter(world) {
            if *visibility == Visibility::Hidden {
                continue;
            }
            let Some(mesh) = world.resource::<Assets<Mesh>>().get(&mesh.0) else { continue };
            let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else {
                continue;
            };
            let Some(indices) = mesh.indices() else { continue };
            let points: Vec<Vec3> = indices.iter().map(|i| placed.transform_point(Vec3::from(positions[i]))).collect();
            drawn.push(points);
        }
        let spread = |points: &[Vec3]| points.iter().flat_map(|a| points.iter().map(move |b| a.distance(*b))).fold(0.0f32, f32::max);
        let close = |points: &&Vec<Vec3>| points.iter().any(|p| p.distance(camera.translation) < 120.0);
        let lowest = |points: &Vec<Vec3>| points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        let stream = drawn
            .iter()
            .filter(close)
            .filter(|points| lowest(points) > camera.translation.y - 150.0)
            .filter(|points| points.iter().any(|p| p.distance(camera.translation) < 80.0))
            .max_by(|a, b| spread(a).total_cmp(&spread(b)))
            .expect("no particles drawn at the dude");
        let near = stream.iter().min_by(|a, b| a.distance(camera.translation).total_cmp(&b.distance(camera.translation))).copied().expect("points");
        let far = stream.iter().max_by(|a, b| a.distance(camera.translation).total_cmp(&b.distance(camera.translation))).copied().expect("points");
        let out = Vec3::new(far.x - near.x, 0.0, far.z - near.z);
        let ahead = Vec3::new(forward.x, 0.0, forward.z).normalize();
        assert!(out.length() > 30.0 && out.dot(ahead) > 0.8 * out.length(), "turn {turn}: the stream runs {out:?}, the view looks {ahead:?}");
        let top = stream.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        assert!(far.y < top, "turn {turn}: the stream rises");
    }
}
