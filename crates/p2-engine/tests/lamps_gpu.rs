mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::{App, Update};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, ClearColorConfig, RenderTarget};
use bevy::color::Color;
use bevy::ecs::observer::On;
use bevy::ecs::query::With;
use bevy::image::Image;
use bevy::light::{AmbientLight, CascadeShadowConfigBuilder, DirectionalLight};
use bevy::math::Vec3;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_assets::{BspNode, Model, Zone};
use p2_collision::{BspHulls, MeshSet};
use p2_engine::display::{MODERN_GRAPHICS, MODERN_INDIRECT, MODERN_LAMP_SHADOWS, Settings};
use p2_engine::mesh_view::{LevelAlbedos, LevelCollision, SceneView, ShownScene, StaticCollision};
use p2_engine::render::actor_light::LightEnvironment;
use p2_engine::render::lamps::{Lamp, LampsPlugin};
use p2_engine::render::mesh_light::{DynamicLit, MeshLightPlugin, MeshLitMaterial};
use p2_engine::render::modern::{MOVER_LAYER, SUN_VIEW_LAYER, view_ambient};
use p2_engine::render::probes::ProbesPlugin;
use p2_math::Vector;
use p2_render_cpu::MeshLight;
use rustc_hash::FxHashMap;

const SIZE: u32 = 64;

struct Room {
    lamp_shadows: &'static str,
    indirect: &'static str,
    lamp: Vec3,
    lamp_radius: f32,
    casters: Vec<[[f32; 3]; 3]>,
    camera_z: f32,
    ground_half: f32,
    ambients: [f32; 2],
}

fn unreal(point: [f32; 3]) -> [f32; 3] {
    p2_bevy_assets::to_bevy(point)
}

fn zones_split_below_the_eye(eye_zone: u8, floor_zone: u8) -> Model {
    let node = BspNode {
        plane: [0.0, 1.0, 0.0, 200.0],
        node_flags: 0,
        vert_pool: 0,
        surface: 0,
        back: None,
        front: None,
        coplanar: None,
        collision_bound: None,
        render_bound: None,
        zones: [floor_zone, eye_zone],
        num_vertices: 0,
        leaves: [-1, -1],
        section: None,
        first_vertex: 0,
        light_map: None,
    };
    Model { nodes: vec![node], zones: vec![Zone { actor: 0, last_render_time: 0.0 }; 3], root_outside: true, ..Model::default() }
}

fn ready(app: &mut App, room: &Room, ground: &Handle<MeshLitMaterial>) -> bool {
    let world = app.world_mut();
    let lamps = world.query_filtered::<(), With<Lamp>>().iter(world).count();
    let Some(material) = world.resource::<Assets<MeshLitMaterial>>().get(ground) else { return false };
    lamps > 0
        && (room.lamp_shadows == "0" || material.base.occlusion_texture.is_some())
        && (room.indirect == "0" || material.base.metallic_roughness_texture.is_some())
}

fn render(room: &Room) -> Vec<u8> {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((MeshLightPlugin, LampsPlugin, ProbesPlugin));
    app.add_systems(Update, view_ambient);
    app.insert_resource(Settings::read(|query| match query {
        MODERN_GRAPHICS => "True".to_owned(),
        MODERN_LAMP_SHADOWS => room.lamp_shadows.to_owned(),
        MODERN_INDIRECT => room.indirect.to_owned(),
        _ => String::new(),
    }));
    let lamp = MeshLight { location: Vector::from(unreal(room.lamp.to_array())), brightness: 128.0, radius: room.lamp_radius, ..MeshLight::default() };
    let mut lights = FxHashMap::default();
    lights.insert(0, lamp);
    let mut shown = ShownScene::default();
    let [eye, floor] = room.ambients;
    shown.lighting = Some(Arc::new(LightEnvironment::new(lights, vec![[0.0; 3], [floor; 3], [eye; 3]], 1.0)));
    app.insert_resource(shown);
    let collision = StaticCollision {
        model: zones_split_below_the_eye(2, 1),
        hulls: BspHulls::default(),
        terrains: Vec::new(),
        meshes: Vec::new(),
        mesh_set: MeshSet::default(),
        lamp_casters: room.casters.iter().map(|triangle| triangle.map(unreal)).collect(),
        lamp_masked: Vec::new(),
        albedos: LevelAlbedos::default(),
    };
    app.insert_resource(LevelCollision(Some(Arc::new(collision))));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let frames = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").push(event.data.clone());
    });
    world.spawn((
        Camera3d::default(),
        Camera { clear_color: ClearColorConfig::Custom(Color::srgb(0.0, 0.0, 1.0)), ..Camera::default() },
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::from_xyz(0.0, 0.0, room.camera_z).looking_at(Vec3::ZERO, Vec3::Y),
        RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]),
        SceneView { position: Vec3::new(0.0, 0.0, room.camera_z), ..SceneView::default() },
        AmbientLight { brightness: 0.0, ..AmbientLight::default() },
    ));
    let mesh = world.resource_mut::<Assets<Mesh>>().add(common::quad(room.ground_half, 0.0, [1.0; 4]));
    let mut extension = DynamicLit::default();
    extension.layers.detail.z = 7.0;
    let texture = world.resource_mut::<Assets<Image>>().add(Image::new(
        Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        TextureDimension::D2,
        vec![255; 64],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    ));
    let base = StandardMaterial { base_color_texture: Some(texture), ..StandardMaterial::default() };
    let ground = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base, extension });
    world.spawn((Mesh3d(mesh), MeshMaterial3d(ground.clone()), Transform::default()));
    let no_sun = DirectionalLight { color: Color::BLACK, illuminance: 1.0, shadow_maps_enabled: false, affects_lightmapped_mesh_diffuse: false, ..DirectionalLight::default() };
    let cascades = CascadeShadowConfigBuilder { overlap_proportion: 0.1, ..CascadeShadowConfigBuilder::default() }.build();
    world.spawn((no_sun, cascades, Transform::default().looking_to(Vec3::NEG_Z, Vec3::Y), RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER])));

    let start = Instant::now();
    let mut settled = None;
    for frame in 0.. {
        app.update();
        let centre = frames.lock().expect("lock").last().map(|last| {
            let at = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
            [last[at], last[at + 1], last[at + 2], last[at + 3]]
        });
        let drawn = centre.is_some_and(|[r, _, b, _]| r == b) && frames.lock().expect("lock").len() > 3;
        let is_ready = ready(&mut app, room, &ground);
        assert!(start.elapsed() < Duration::from_secs(60), "the scene never settled: centre {centre:?}, ready {is_ready}, compiling {}", common::compiling(&app));
        if settled.is_none() && drawn && is_ready {
            settled = Some(frame);
        }
        if settled.is_some_and(|settled| frame > settled + 40) && !common::compiling(&app) {
            break;
        }
    }
    let frames = frames.lock().expect("lock");
    frames.last().expect("no frame read back").clone()
}

fn red(frame: &[u8], x: u32, y: u32) -> u32 {
    u32::from(frame[((y * SIZE + x) * 4) as usize])
}

fn blocked_floor(lamp_shadows: &'static str) -> Room {
    let (near, far, height) = (-10.0, -400.0, 50.0);
    Room {
        lamp_shadows,
        indirect: "0",
        lamp: Vec3::new(0.0, 0.0, 100.0),
        lamp_radius: 11.0,
        casters: vec![
            [[far, far, height], [near, far, height], [near, -far, height]],
            [[far, far, height], [near, -far, height], [far, -far, height]],
        ],
        camera_z: 400.0,
        ground_half: 400.0,
        ambients: [0.0; 2],
    }
}

fn closed_box(half: f32) -> Vec<[[f32; 3]; 3]> {
    let mut triangles = Vec::new();
    for axis in 0..3 {
        for side in [-half, half] {
            let corner = |u: f32, v: f32| {
                let mut point = [0.0; 3];
                point[axis] = side;
                point[(axis + 1) % 3] = u;
                point[(axis + 2) % 3] = v;
                point
            };
            triangles.push([corner(-half, -half), corner(half, -half), corner(half, half)]);
            triangles.push([corner(-half, -half), corner(half, half), corner(-half, half)]);
        }
    }
    triangles
}

fn lit_room(indirect: &'static str) -> Room {
    Room { lamp_shadows: "3", indirect, lamp: Vec3::new(0.0, 0.0, -300.0), lamp_radius: 39.0, casters: closed_box(520.0), camera_z: 400.0, ground_half: 100.0, ambients: [0.0; 2] }
}

fn ambient_room(indirect: &'static str, ambients: [f32; 2]) -> Room {
    Room { lamp_shadows: "0", indirect, lamp: Vec3::new(0.0, 0.0, -3000.0), lamp_radius: 1.0, casters: closed_box(520.0), camera_z: 400.0, ground_half: 100.0, ambients }
}

#[test]
#[ignore = "needs a GPU"]
fn a_lamp_lights_the_floor_and_its_atlas_shadows_the_blocked_side() {
    let frame = render(&blocked_floor("3"));
    let (under, open, blocked) = (red(&frame, SIZE / 2, SIZE / 2), red(&frame, 48, SIZE / 2), red(&frame, 15, SIZE / 2));
    assert!(under > 40, "the floor under the lamp is not lit: {under}");
    assert!(blocked + 30 < open, "no atlas shadow behind the blocker: blocked {blocked}, open {open}, under {under}");
}

#[test]
#[ignore = "needs a GPU"]
fn without_lamp_shadows_the_blocked_side_is_as_bright_as_the_open_side() {
    let frame = render(&blocked_floor("0"));
    let (open, blocked) = (red(&frame, 48, SIZE / 2), red(&frame, 15, SIZE / 2));
    assert!(open > 20, "the open floor is not lit: {open}");
    assert!(blocked.abs_diff(open) <= 3, "a lamp shadow with lamp shadows off: blocked {blocked}, open {open}");
}

#[test]
#[ignore = "needs a GPU"]
fn bounce_light_reaches_a_surface_the_lamp_does_not_face() {
    let with = red(&render(&lit_room("2")), SIZE / 2, SIZE / 2);
    let without = red(&render(&lit_room("0")), SIZE / 2, SIZE / 2);
    assert!(with > without + 15, "no bounce light from the probes: with {with}, without {without}");
}

#[test]
#[ignore = "needs a GPU"]
fn probes_light_a_surface_with_its_own_zones_ambient() {
    let centre = |room: &Room| red(&render(room), SIZE / 2, SIZE / 2);
    let (dim, bright) = (0.2, 0.45);
    let view = centre(&ambient_room("0", [dim, dim]));
    let probed = centre(&ambient_room("2", [dim, dim]));
    assert!(view > 10 && probed.abs_diff(view) <= 4, "probes change a surface lit by its zone alone: probes {probed}, view {view}");
    let brighter_view = centre(&ambient_room("0", [bright, bright]));
    let brighter_zone = centre(&ambient_room("2", [dim, bright]));
    assert!(brighter_view > view + 15, "the brighter ambient is not brighter: {brighter_view}, {view}");
    assert!(brighter_zone.abs_diff(brighter_view) <= 4, "the floor does not take its own zone's ambient: probes {brighter_zone}, its zone through the view {brighter_view}, the eye's {view}");
}
