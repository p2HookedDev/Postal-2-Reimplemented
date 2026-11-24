mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::{App, Update};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, ClearColorConfig, RenderTarget};
use bevy::color::Color;
use bevy::diagnostic::DiagnosticsStore;
use bevy::ecs::observer::On;
use bevy::ecs::query::With;
use bevy::image::Image;
use bevy::light::{AmbientLight, CascadeShadowConfigBuilder, DirectionalLight};
use bevy::math::{Mat4, Vec3};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Mesh, Mesh3d, VertexAttributeValues};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_assets::{BspNode, Model, Zone};
use p2_collision::{BspHulls, MeshSet};
use p2_engine::display::{MODERN_GRAPHICS, MODERN_INDIRECT, MODERN_LAMP_SHADOWS, Settings};
use p2_engine::mesh_view::{FollowActor, LevelAlbedos, LevelCollision, MOVER_LAYER, SUN_VIEW_LAYER, SceneView, ShownScene, StaticCollision};
use p2_engine::render::actor_light::LightEnvironment;
use p2_engine::render::lamps::{Lamp, LampAtlas, LampsPlugin};
use p2_engine::render::mesh_light::{DynamicLit, MeshLightPlugin, MeshLitMaterial};
use p2_engine::render::modern::view_ambient;
use p2_vm::ObjectId;
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
    /// The side of the picture in pixels.
    view: u32,
    /// A square that moves, this far from its middle to a side and this high over the floor: it casts and is not seen.
    mover: Option<(f32, f32, Hung)>,
    /// A second lamp, like the first.
    fill: Option<Vec3>,
}

/// What lifts the square off the floor: its own place, or the one joint it is skinned to.
#[derive(Clone, Copy)]
enum Hung {
    ByItsPlace,
    ByAJoint,
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
        && material.base.anisotropy_texture.is_some()
        && (room.lamp_shadows == "0" || material.base.occlusion_texture.is_some())
        && (room.indirect == "0" || material.base.metallic_roughness_texture.is_some())
}

fn render(room: &Room) -> Vec<u8> {
    render_with_faces(room).0
}

/// The picture and whether any lamp holds a detail face at its end.
fn render_with_faces(room: &Room) -> (Vec<u8>, bool) {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((MeshLightPlugin, LampsPlugin, ProbesPlugin, RenderDiagnosticsPlugin));
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
    if let Some(at) = room.fill {
        lights.insert(1, MeshLight { location: Vector::from(unreal(at.to_array())), ..lamp });
    }
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
    let mut target = Image::new_target_texture(room.view, room.view, TextureFormat::Rgba8UnormSrgb, None);
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
    if let Some((half, height, hung)) = room.mover {
        let mut mesh = common::quad(half, 0.0, [1.0; 4]);
        let corners = mesh.count_vertices();
        let actor = FollowActor { actor: ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
        let cast = RenderLayers::layer(MOVER_LAYER);
        match hung {
            Hung::ByItsPlace => {
                let mesh = world.resource_mut::<Assets<Mesh>>().add(mesh);
                world.spawn((Mesh3d(mesh), MeshMaterial3d(ground.clone()), Transform::from_xyz(0.0, 0.0, height), cast, actor));
            }
            Hung::ByAJoint => {
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(vec![[0; 4]; corners]));
                mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, vec![[1.0f32, 0.0, 0.0, 0.0]; corners]);
                let mesh = world.resource_mut::<Assets<Mesh>>().add(mesh);
                let joint = world.spawn(Transform::from_xyz(0.0, 0.0, height)).id();
                let inverse_bindposes = world.resource_mut::<Assets<SkinnedMeshInverseBindposes>>().add(SkinnedMeshInverseBindposes::from(vec![Mat4::IDENTITY]));
                world.spawn((Mesh3d(mesh), MeshMaterial3d(ground.clone()), Transform::default(), cast, actor, SkinnedMesh { inverse_bindposes, joints: vec![joint] }));
            }
        }
    }
    let no_sun = DirectionalLight { color: Color::BLACK, illuminance: 1.0, shadow_maps_enabled: false, affects_lightmapped_mesh_diffuse: false, ..DirectionalLight::default() };
    let cascades = CascadeShadowConfigBuilder { overlap_proportion: 0.1, ..CascadeShadowConfigBuilder::default() }.build();
    world.spawn((no_sun, cascades, Transform::default().looking_to(Vec3::NEG_Z, Vec3::Y), RenderLayers::from_layers(&[SUN_VIEW_LAYER, MOVER_LAYER])));

    let start = Instant::now();
    let mut settled = None;
    for frame in 0.. {
        app.update();
        let centre = frames.lock().expect("lock").last().map(|last| {
            let at = ((room.view / 2 * room.view + room.view / 2) * 4) as usize;
            [last[at], last[at + 1], last[at + 2], last[at + 3]]
        });
        let drawn = centre.is_some_and(|[r, _, b, _]| r == b) && frames.lock().expect("lock").len() > 3;
        let is_ready = ready(&mut app, room, &ground);
        assert!(start.elapsed() < Duration::from_secs(60), "the scene never settled: centre {centre:?}, ready {is_ready}, compiling {}", common::compiling(&app));
        if settled.is_none() && drawn && is_ready {
            settled = Some(frame);
        }
        // A mover's tile fades in over a third of a second.
        if settled.is_some_and(|settled| frame > settled + 40) && !common::compiling(&app) && start.elapsed() > Duration::from_millis(700) {
            break;
        }
    }
    let timed = app.world().resource::<DiagnosticsStore>().iter().any(|diagnostic| diagnostic.path().as_str() == "render/lamp_shadows/elapsed_gpu");
    assert!(timed || room.lamp_shadows == "0", "the lamp shadow passes left no GPU time");
    let frames = frames.lock().expect("lock");
    (frames.last().expect("no frame read back").clone(), app.world().resource::<LampAtlas>().has_detail_faces())
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
        view: SIZE,
        mover: None, fill: None,
    }
}

/// A lamp high over a floor that casts too, seen from close in a large picture: the plain tile's texels would show.
fn blocked_floor_seen_close() -> Room {
    let (near, far, height, floor) = (-10.0, -400.0, 150.0, 400.0);
    Room {
        lamp_shadows: "3",
        indirect: "0",
        lamp: Vec3::new(0.0, 0.0, 200.0),
        lamp_radius: 20.0,
        casters: vec![
            [[far, far, height], [near, far, height], [near, -far, height]],
            [[far, far, height], [near, -far, height], [far, -far, height]],
            [[-floor, -floor, 0.0], [floor, -floor, 0.0], [floor, floor, 0.0]],
            [[-floor, -floor, 0.0], [floor, floor, 0.0], [-floor, floor, 0.0]],
        ],
        camera_z: 140.0,
        ground_half: 400.0,
        ambients: [0.0; 2],
        view: 512,
        mover: None, fill: None,
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
    Room { lamp_shadows: "3", indirect, lamp: Vec3::new(0.0, 0.0, -300.0), lamp_radius: 39.0, casters: closed_box(520.0), camera_z: 400.0, ground_half: 100.0, ambients: [0.0; 2], view: SIZE, mover: None, fill: None }
}

fn ambient_room(indirect: &'static str, ambients: [f32; 2]) -> Room {
    Room { lamp_shadows: "0", indirect, lamp: Vec3::new(0.0, 0.0, -3000.0), lamp_radius: 1.0, casters: closed_box(520.0), camera_z: 400.0, ground_half: 100.0, ambients, view: SIZE, mover: None, fill: None }
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
fn a_face_seen_close_gets_a_detail_square_that_shades_the_blocked_side() {
    let room = blocked_floor_seen_close();
    let (frame, detailed) = render_with_faces(&room);
    let red = |x: u32| u32::from(frame[((room.view / 2 * room.view + x) * 4) as usize]);
    let (under, open, blocked) = (red(room.view / 2), red(140), red(20));
    assert!(detailed, "no detail face for a lamp whose plain tile would show its texels");
    assert!(under > 40, "the floor under the lamp is not lit: {under}");
    assert!(blocked + 30 < open, "no shadow from the detail face behind the blocker: blocked {blocked}, open {open}, under {under}");
}

/// The middle of a 512 pixel picture, in linear light.
fn linear_centre(frame: &[u8]) -> f32 {
    let byte = f32::from(frame[((256 * 512 + 256) * 4) as usize]) / 255.0;
    if byte <= 0.04045 { byte / 12.92 } else { ((byte + 0.055) / 1.055).powf(2.4) }
}

/// A lamp over an open floor with a square hanging between them.
fn floor_under_a_mover(mover: Option<(f32, f32, Hung)>) -> Room {
    let floor = 400.0;
    Room {
        lamp_shadows: "3",
        indirect: "0",
        lamp: Vec3::new(0.0, 0.0, 200.0),
        lamp_radius: 20.0,
        casters: vec![[[-floor, -floor, 0.0], [floor, -floor, 0.0], [floor, floor, 0.0]], [[-floor, -floor, 0.0], [floor, floor, 0.0], [-floor, floor, 0.0]]],
        camera_z: 140.0,
        ground_half: 400.0,
        ambients: [0.0; 2],
        view: 512,
        mover,
        fill: None,
    }
}

fn a_mover_shades_only_the_floor_under_it(hung: Hung) {
    let middle_row = |frame: &[u8], x: u32| u32::from(frame[((256 * 512 + x) * 4) as usize]);
    let open = render(&floor_under_a_mover(None));
    let shaded = render(&floor_under_a_mover(Some((20.0, 100.0, hung))));
    let (under, beside) = (middle_row(&shaded, 256), middle_row(&shaded, 480));
    assert!(middle_row(&open, 256) > 40, "the floor under the lamp is not lit: {}", middle_row(&open, 256));
    assert!(under + 30 < middle_row(&open, 256), "the mover casts no shadow under it: {under} against {} without it", middle_row(&open, 256));
    assert!(beside.abs_diff(middle_row(&open, 480)) <= 3, "the mover's shadow reaches the open floor: {beside} against {}", middle_row(&open, 480));
}

#[test]
#[ignore = "needs a GPU"]
fn a_mover_shades_the_floor_under_it_through_its_own_tile() {
    a_mover_shades_only_the_floor_under_it(Hung::ByItsPlace);
}

#[test]
#[ignore = "needs a GPU"]
fn a_skinned_mover_casts_from_where_its_joint_holds_it() {
    a_mover_shades_only_the_floor_under_it(Hung::ByAJoint);
}

#[test]
#[ignore = "needs a GPU"]
fn a_mover_near_the_floor_darkens_the_ambient_light_under_it() {
    for eye in [140.0, 1300.0] {
        let under = |height: f32| {
            let mut room = floor_under_a_mover(Some((20.0, height, Hung::ByItsPlace)));
            (room.ambients, room.camera_z) = ([0.5; 2], eye);
            linear_centre(&render(&room))
        };
        let (far, near) = (under(100.0), under(12.0));
        assert!(far > 0.02, "the ambient light does not reach the floor seen from {eye}: {far}");
        assert!(near < 0.9 * far && near > 0.2 * far, "a mover near the floor does not darken the ambient under it by its share, seen from {eye}: {near} against {far}");
    }
}

#[test]
#[ignore = "needs a GPU"]
fn a_shadow_takes_its_lamps_share_of_the_light_and_no_more() {
    let aside = Vec3::new(150.0, 0.0, 200.0);
    let mover = Some((20.0, 100.0, Hung::ByItsPlace));
    let alone = linear_centre(&render(&floor_under_a_mover(None)));
    let other = linear_centre(&render(&Room { lamp: aside, ..floor_under_a_mover(None) }));
    let both = linear_centre(&render(&Room { fill: Some(aside), ..floor_under_a_mover(None) }));
    let shaded = linear_centre(&render(&Room { fill: Some(aside), ..floor_under_a_mover(mover) }));
    let share = other / (alone + other);
    let summed = other / (alone.sqrt() + other.sqrt()).powi(2);
    assert!(alone > 0.02 && other > 0.01 && share - summed > 0.1, "the lamps do not tell the two rules apart: {alone}, {other}");
    assert!((shaded / both - share).abs() < 0.07, "the floor under the mover keeps {} of its light, its other lamp's share is {share} (summed in display light: {summed})", shaded / both);
}

#[test]
#[ignore = "needs a GPU"]
fn a_lamp_a_wall_hides_adds_no_light_to_what_another_lamp_lights() {
    let aside = Vec3::new(150.0, 0.0, 200.0);
    let (low, high, height) = ([55.0, -20.0], [95.0, 20.0], 100.0);
    let hiding = [[[low[0], low[1], height], [high[0], low[1], height], [high[0], high[1], height]], [[low[0], low[1], height], [high[0], high[1], height], [low[0], high[1], height]]];
    let walled = |fill: Option<Vec3>| {
        let mut room = Room { fill, ..floor_under_a_mover(None) };
        room.casters.extend(hiding);
        room
    };
    let alone = linear_centre(&render(&walled(None)));
    let open = linear_centre(&render(&Room { fill: Some(aside), ..floor_under_a_mover(None) }));
    let hidden = linear_centre(&render(&walled(Some(aside))));
    assert!(alone > 0.02 && open > 1.3 * alone, "the second lamp adds too little to tell: alone {alone}, both {open}");
    assert!((hidden / alone - 1.0).abs() < 0.05, "a lamp behind a wall changes the floor's light: {hidden} against {alone} without it ({open} with it in the open)");
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
