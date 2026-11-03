use bevy::app::{App, Plugin, Update};
use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::prelude::{Added, Commands, Entity, Query, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::input::ButtonInput;
use bevy::input::keyboard::KeyCode;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseButton, MouseScrollUnit};
use bevy::light::DirectionalLight;
use bevy::math::{EulerRot, Quat, Vec3};
use bevy::time::Time;
use bevy::transform::components::Transform;
use p2_bevy_assets::to_bevy;
use p2_engine::map::LevelCollision;
use p2_engine::mesh_view::{MeshViewSystems, SceneView, ShownScene};
use p2_math::Vector;

use crate::walk::{Body, EYE_HEIGHT, step};

const LOOK_RATE: f32 = 0.005;
const PITCH_LIMIT: f32 = 1.55;
const ZOOM_STEP: f32 = 0.85;
const SPEED_STEP: f32 = 1.2;
const MAP_SPEED: f32 = 800.0;
const MIN_MESH_SPEED: f32 = 100.0;
const WALK_BOOST: f32 = 2.0;
const FLY_BOOST: f32 = 4.0;
const MAX_WALK_DELTA: f32 = 0.1;

pub struct ViewerControlsPlugin;

impl Plugin for ViewerControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewerInput>()
            .init_resource::<Walking>()
            .init_resource::<Rig>()
            .add_systems(Update, (headlight, control.in_set(MeshViewSystems::Control)));
    }
}

/// Light-mapped surfaces keep only their light map. The game has none.
fn headlight(mut commands: Commands, cameras: Query<Entity, Added<SceneView>>) {
    for camera in &cameras {
        commands.entity(camera).with_child((
            DirectionalLight { affects_lightmapped_mesh_diffuse: false, ..Default::default() },
            Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.5, -0.5, 0.0)),
        ));
    }
}

#[derive(Resource, Default)]
pub struct ViewerInput {
    pub hovered: bool,
    pub keyboard: bool,
}

#[derive(Resource, Default)]
struct Rig {
    orbit: bool,
    yaw: f32,
    pitch: f32,
    distance: f32,
    position: Vec3,
    speed: f32,
}

#[derive(Resource, Default)]
struct Walking {
    on: bool,
    body: Body,
}

fn orbit_start(radius: f32) -> SceneView {
    let rotation = Quat::from_euler(EulerRot::YXZ, 0.6, -0.35, 0.0);
    SceneView { position: rotation * Vec3::Z * radius * 2.5, rotation }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system: each parameter is state it reads or changes")]
fn control(
    input: Res<ViewerInput>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    time: Res<Time>,
    shown: Res<ShownScene>,
    collision: Res<LevelCollision>,
    mut walking: ResMut<Walking>,
    mut rig: ResMut<Rig>,
    mut views: Query<&mut SceneView>,
) {
    if shown.is_changed() {
        *walking = Walking::default();
        let start = shown.start_view().unwrap_or_else(|| orbit_start(shown.radius));
        let (yaw, pitch, _) = start.rotation.to_euler(EulerRot::YXZ);
        let speed = if shown.start.is_some() { MAP_SPEED } else { (shown.radius / 8.0).max(MIN_MESH_SPEED) };
        *rig = Rig { orbit: collision.0.is_none(), yaw, pitch, distance: start.position.length(), position: start.position, speed };
    }
    let lines = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 100.0,
    };
    let camera = &mut *rig;
    let look = if camera.orbit { MouseButton::Left } else { MouseButton::Right };
    if input.hovered {
        if buttons.pressed(look) {
            camera.yaw -= motion.delta.x * LOOK_RATE;
            camera.pitch = (camera.pitch - motion.delta.y * LOOK_RATE).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        }
        if camera.orbit {
            camera.distance *= ZOOM_STEP.powf(lines);
        } else {
            camera.speed *= SPEED_STEP.powf(lines);
        }
    }
    if !camera.orbit && input.keyboard {
        if keys.just_pressed(KeyCode::KeyG) && collision.0.is_some() {
            walking.on = !walking.on;
            let eye = to_bevy((camera.position - shown.origin).to_array());
            walking.body = Body { center: Vector::new(eye[0], eye[1], eye[2] - EYE_HEIGHT), ..Body::default() };
        }
        if let (true, Some(collision)) = (walking.on, &collision.0) {
            let rotation = Quat::from_rotation_y(camera.yaw);
            let (forward, right) = (rotation * Vec3::NEG_Z, rotation * Vec3::X);
            let axes = [(KeyCode::KeyW, forward), (KeyCode::KeyS, -forward), (KeyCode::KeyD, right), (KeyCode::KeyA, -right)];
            let direction: Vec3 = axes.iter().filter(|(key, _)| keys.pressed(*key)).map(|(_, axis)| *axis).sum();
            let direction = direction.normalize_or_zero();
            let speed = if keys.pressed(KeyCode::ShiftLeft) { WALK_BOOST } else { 1.0 };
            let jump = keys.just_pressed(KeyCode::Space);
            step(collision, &mut walking.body, [direction.x, direction.z], speed, jump, time.delta_secs().min(MAX_WALK_DELTA));
            let eye = walking.body.center + Vector::new(0.0, 0.0, EYE_HEIGHT);
            camera.position = Vec3::from(to_bevy(eye)) + shown.origin;
        } else {
            let rotation = Quat::from_euler(EulerRot::YXZ, camera.yaw, camera.pitch, 0.0);
            let axes = [
                (KeyCode::KeyW, rotation * Vec3::NEG_Z),
                (KeyCode::KeyS, rotation * Vec3::Z),
                (KeyCode::KeyD, rotation * Vec3::X),
                (KeyCode::KeyA, rotation * Vec3::NEG_X),
                (KeyCode::KeyE, Vec3::Y),
                (KeyCode::KeyQ, Vec3::NEG_Y),
            ];
            let direction: Vec3 = axes.iter().filter(|(key, _)| keys.pressed(*key)).map(|(_, axis)| *axis).sum();
            let boost = if keys.pressed(KeyCode::ShiftLeft) { FLY_BOOST } else { 1.0 };
            let step = direction.normalize_or_zero() * camera.speed * boost * time.delta_secs();
            camera.position += step;
        }
    }
    let rotation = Quat::from_euler(EulerRot::YXZ, camera.yaw, camera.pitch, 0.0);
    let position = if camera.orbit { rotation * Vec3::Z * camera.distance } else { camera.position };
    for mut view in &mut views {
        *view = SceneView { position, rotation };
    }
}
