use bevy::app::{Last, TaskPoolPlugin};
use bevy::asset::{AssetApp, AssetEvent, AssetPlugin, RenderAssetUsages};
use bevy::ecs::prelude::{MessageReader, World};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};

use super::*;
use super::sound::distance_gain;

#[test]
fn gain_is_full_inside_the_radius_then_inverse() {
    assert_eq!(distance_gain(0.0, 200.0), 1.0);
    assert_eq!(distance_gain(200.0, 200.0), 1.0);
    assert!((distance_gain(400.0, 200.0) - 1.0 / 1.9).abs() < 1e-6);
}

#[test]
fn ambient_gain_takes_the_sound_volume_setting_too() {
    assert!((ambient_gain(1.0, 1.0, 0.1) - 0.1).abs() < 1e-6);
    assert_eq!(ambient_gain(1.0, 2.0, 1.0), 1.0);
}

#[test]
fn the_left_channel_ear_sits_on_the_cameras_right() {
    let ears = sound_listener();
    assert_eq!(ears.left_ear_offset, Vec3::X * 2.0);
    assert_eq!(ears.right_ear_offset, Vec3::X * -2.0);
}

#[test]
fn only_the_cameras_draw_never_the_sun_alone() {
    let e = |n| Entity::from_raw_u32(n).unwrap();
    let (mut camera, overlay) = ([e(1), e(3), e(5)], [e(7)]);
    camera.sort_unstable();
    let seen: [&[Entity]; 2] = [&camera, &overlay];
    let shadow_only = HashSet::from([e(20)]);
    assert!(camera_drew(&seen, &shadow_only, e(3), Some(e(10))));
    assert!(camera_drew(&seen, &shadow_only, e(7), None));
    assert!(!camera_drew(&seen, &shadow_only, e(4), Some(e(10))));
    assert!(!camera_drew(&seen, &shadow_only, e(5), Some(e(20))));
    assert!(!camera_drew(&[], &HashSet::new(), e(1), None));
}

#[derive(Resource, Default)]
struct Seen(Vec<usize>);

#[derive(Resource)]
struct Particles(Handle<Mesh>);

fn mesh_changes_per_frame(mut app: App) -> Vec<usize> {
    app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default())).init_asset::<Mesh>().init_resource::<Seen>();
    let mesh = app.world_mut().resource_mut::<Assets<Mesh>>().add(Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default()));
    app.insert_resource(Particles(mesh));
    let rewrite = |world: &mut World| {
        let handle = world.resource::<Particles>().0.clone();
        if let Some(mut mesh) = world.resource_mut::<Assets<Mesh>>().get_mut(&handle) {
            mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
        }
    };
    app.add_systems(PostUpdate, rewrite.in_set(PlaySystems::Present));
    app.add_systems(Last, |mut events: MessageReader<AssetEvent<Mesh>>, mut seen: ResMut<Seen>| {
        seen.0.push(events.read().filter(|event| matches!(event, AssetEvent::Modified { .. })).count());
    });
    app.finish();
    app.cleanup();
    for _ in 0..10 {
        app.world_mut().run_schedule(PostUpdate);
        app.world_mut().run_schedule(Last);
    }
    std::mem::take(&mut app.world_mut().resource_mut::<Seen>().0)
}

#[test]
fn weapon_moved_in_the_pass_is_drawn_there_that_frame() {
    use bevy::transform::components::GlobalTransform;
    let mut app = App::new();
    app.add_plugins((TaskPoolPlugin::default(), bevy::transform::TransformPlugin, PlayPlugin));
    let weapon = app.world_mut().spawn(Transform::default()).id();
    app.add_systems(PostUpdate, (move |mut moved: Query<&mut Transform>| {
        if let Ok(mut transform) = moved.get_mut(weapon) {
            transform.translation.x += 1.0;
        }
    })
    .in_set(PlaySystems::Present));
    app.finish();
    app.cleanup();
    for frame in 1..=5 {
        app.world_mut().run_schedule(PostUpdate);
        let global = app.world().get::<GlobalTransform>(weapon).map(|g| g.translation().x);
        assert_eq!(global, Some(frame as f32));
    }
}

#[test]
fn particle_meshes_reach_the_renderer_in_the_frame_they_change() {
    let mut app = App::new();
    app.add_plugins(PlayPlugin);
    assert_eq!(mesh_changes_per_frame(app), vec![1; 10]);
    let mut app = App::new();
    app.configure_sets(PostUpdate, PlaySystems::Present.after(AssetEventSystems));
    assert_eq!(mesh_changes_per_frame(app)[0], 0);
}

#[test]
fn steps_are_even_over_alternating_frame_times() {
    let mut clock = StepClock::default();
    let (short, long) = (3.0 / 240.0, 4.0 / 240.0);
    let steps: Vec<f32> = (0..60).map(|i| clock.step(if i % 2 == 0 { short } else { long })).collect();
    for &step in &steps[STEP_FRAMES..] {
        assert!((step - (short + long) / 2.0).abs() < 1e-6, "{step}");
    }
    let mut clock = StepClock::default();
    let (mut real, mut game) = (0.0f32, 0.0f32);
    for i in 0..1000 {
        let delta = 0.010 + 0.004 * ((i * 7919) % 13) as f32 / 12.0;
        real += delta;
        game += clock.step(delta);
        assert!((real - game).abs() < STEP_FRAMES as f32 * 0.014, "frame {i}: real {real}, game {game}");
    }
    assert_eq!(StepClock::default().step(1.0), MAX_STEP);
}

#[test]
fn the_vm_can_move_to_its_thread() {
    fn send<T: Send>() {}
    send::<Simulation>();
    send::<Job>();
    send::<VmFrame>();
}
