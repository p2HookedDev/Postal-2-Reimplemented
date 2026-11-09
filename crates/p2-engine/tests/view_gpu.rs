mod common;

use std::sync::{Arc, Mutex};

use bevy::app::App;
use bevy::asset::{Assets, RenderAssetUsages};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::Color;
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::ecs::observer::On;
use bevy::image::Image;
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::occlusion_culling::OcclusionCulling;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_engine::render::particle_material::{ParticleMaterial, ParticlePlugin};

const SIZE: u32 = 64;

fn app() -> (App, Arc<Mutex<Vec<Vec<u8>>>>, bevy::asset::Handle<Image>) {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((
        ParticlePlugin,
        p2_engine::render::blended::BlendedPlugin,
        p2_engine::render::lightmapped::LightmappedPlugin,
        p2_engine::render::actor_light::ActorLightPlugin,
        p2_engine::render::mesh_light::MeshLightPlugin,
    ));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let frames = Arc::new(Mutex::new(Vec::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").push(event.data.clone());
    });
    (app, frames, target)
}

fn quad(half: f32, z: f32) -> Mesh {
    common::quad(half, z, [1.0; 4])
}

fn red(frame: &[u8], x: u32, y: u32) -> u8 {
    frame[((y * SIZE + x) * 4) as usize]
}

fn frames_without_the_sprite(occlusion: bool) -> (usize, usize) {
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    let mut camera = world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
        DepthPrepass,
    ));
    if occlusion {
        camera.insert(OcclusionCulling);
    }
    let camera = camera.id();
    let wall_mesh = world.resource_mut::<Assets<Mesh>>().add(quad(100.0, -20.0));
    let wall = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial {
        base_color: Color::srgb(0.25, 0.25, 0.25),
        unlit: true,
        ..Default::default()
    });
    world.spawn((Mesh3d(wall_mesh), MeshMaterial3d(wall), Transform::default()));
    let texel = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let texture = world.resource_mut::<Assets<Image>>().add(texel);
    let props = p2_particles::EmitterProperties { draw_style: p2_particles::DrawStyle::Translucent, ..Default::default() };
    let material = world.resource_mut::<Assets<ParticleMaterial>>().add(ParticleMaterial::new(texture, &props));
    let sprite = world.resource_mut::<Assets<Mesh>>().add(quad(1.0, -5.0));
    world.spawn((Mesh3d(sprite.clone()), MeshMaterial3d(material), Transform::default(), NoFrustumCulling));
    let mut first = None;
    for frame in 0..600 {
        if let Some(mut mesh) = app.world_mut().resource_mut::<Assets<Mesh>>().get_mut(&sprite) {
            *mesh = quad(1.0 + (frame % 3) as f32 * 0.05, -5.0);
        }
        if let Some(mut transform) = app.world_mut().get_mut::<Transform>(camera) {
            transform.rotation = bevy::math::Quat::from_rotation_y(((frame % 20) as f32 - 10.0) * 0.002);
        }
        app.update();
        let shown = frames.lock().expect("lock").iter().position(|f| red(f, SIZE / 2, SIZE / 2) > 200);
        if first.is_none() && shown.is_some() {
            first = shown;
        }
        if first.is_some_and(|first| frames.lock().expect("lock").len() > first + 120) {
            break;
        }
    }
    let first = first.expect("the sprite never showed");
    let frames = frames.lock().expect("lock");
    let after = &frames[first..];
    (after.iter().filter(|f| red(f, SIZE / 2, SIZE / 2) <= 200).count(), after.len())
}

mod monday {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    use bevy::app::PostUpdate;
    use bevy::ecs::prelude::{IntoScheduleConfigs, ResMut, Resource};
    use bevy::math::UVec2;
    use bevy::time::TimeUpdateStrategy;
    use p2_engine::animation::AnimationPlugin;
    use p2_engine::map::{SceneTargets, Session};
    use p2_engine::mesh_view::{MeshView, MeshViewPlugin, SceneView};
    use p2_engine::play::{PlayPlugin, PlaySystems, PlayerInput};

    pub const W: u32 = 640;
    pub const H: u32 = 360;

    #[derive(Resource)]
    struct Holder {
        session: Session,
        frame: u32,
    }

    pub struct Run {
        pub frames: Vec<Vec<u8>>,
        pub cameras: Vec<Transform>,
    }

    pub fn run(occlusion: bool, warmup: usize, count: usize, turn: f32) -> Option<Run> {
        run_with(occlusion, warmup, count, |_| (Vec::new(), [turn, 0.0]))
    }

    pub fn run_with(occlusion: bool, warmup: usize, count: usize, input: impl Fn(usize) -> (Vec<&'static str>, [f32; 2])) -> Option<Run> {
        let root = common::game_dir()?;
        let mut app = App::new();
        common::headless_renderer(&mut app);
        app.add_plugins((MeshViewPlugin, PlayPlugin, AnimationPlugin))
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(1.0 / 60.0)))
        .insert_resource(Holder { session: Session::new(root), frame: 0 })
        .add_systems(
            PostUpdate,
            (|mut holder: ResMut<Holder>, mut t: SceneTargets| {
                t.view.viewport = Some((UVec2::ZERO, UVec2::new(W, H)));
                if holder.frame == 0 {
                    let map = Path::new("Maps").join("Suburbs-3.fuk").display().to_string();
                    if let Err(error) = holder.session.load_map(&map, &mut t) {
                        panic!("{error}");
                    }
                } else {
                    let items = t.play.world().map(|world| world.hud_items().to_vec()).unwrap_or_default();
                    t.play.set_canvas_size([W as f32, H as f32]);
                    holder.session.present(&items, &mut t);
                }
                holder.frame += 1;
            })
            .in_set(PlaySystems::Present),
        );
        app.finish();
        app.cleanup();
        let world = app.world_mut();
        let mut target = Image::new_target_texture(W, H, TextureFormat::Rgba8UnormSrgb, None);
        target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        let target = world.resource_mut::<Assets<Image>>().add(target);
        world.resource_mut::<MeshView>().occlusion_culling = occlusion;
        world.resource_mut::<PlayerInput>().playing = true;
        let frames = Arc::new(Mutex::new(Vec::new()));
        let store = frames.clone();
        world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
            store.lock().expect("lock").push(event.data.clone());
        });
        app.update();
        let world = app.world_mut();
        let cameras: Vec<_> = world.query_filtered::<bevy::ecs::entity::Entity, bevy::ecs::query::With<Camera3d>>().iter(world).collect();
        for camera in cameras {
            world.entity_mut(camera).insert(RenderTarget::Image(target.clone().into()));
        }
        for _ in 0..warmup {
            app.update();
        }
        frames.lock().expect("lock").clear();
        let mut placed = Vec::new();
        for frame in 0..count {
            let (held, mouse) = input(frame);
            let mut player = app.world_mut().resource_mut::<PlayerInput>();
            (player.held, player.mouse) = (held, mouse);
            app.update();
            let world = app.world_mut();
            let camera = world.query_filtered::<&Transform, bevy::ecs::query::With<SceneView>>().iter(world).next().copied();
            placed.push(camera.unwrap_or_default());
        }
        let mut player = app.world_mut().resource_mut::<PlayerInput>();
        (player.held, player.mouse) = (Vec::new(), [0.0; 2]);
        for _ in 0..4 {
            app.update();
        }
        let world = app.world_mut();
        let culling = world.query_filtered::<bevy::ecs::query::Has<OcclusionCulling>, bevy::ecs::query::With<SceneView>>().iter(world).any(|on| on);
        assert_eq!(culling, occlusion, "occlusion culling not as asked");
        let mut frames = std::mem::take(&mut *frames.lock().expect("lock"));
        frames.truncate(count);
        Some(Run { frames, cameras: placed })
    }

    pub fn differs(a: &[u8], b: &[u8], i: usize, threshold: u8) -> bool {
        (0..3).any(|c| a[i * 4 + c].abs_diff(b[i * 4 + c]) > threshold)
    }

    pub fn flickering(frames: &[Vec<u8>]) -> Vec<usize> {
        let pixels = (W * H) as usize;
        frames
            .windows(3)
            .map(|w| (0..pixels).filter(|&i| differs(&w[1], &w[0], i, 40) && differs(&w[1], &w[2], i, 40) && !differs(&w[0], &w[2], i, 12)).count())
            .collect()
    }
}

#[test]
#[ignore = "needs a GPU and the game"]
fn monday_still_view_does_not_flicker() {
    let mut counts = Vec::new();
    for occlusion in [false, true] {
        let Some(run) = monday::run(occlusion, 240, 90, 0.0) else { return };
        let flicker = monday::flickering(&run.frames);
        counts.push(flicker.iter().sum::<usize>());
    }
    assert!(counts[1] <= counts[0] + 50, "occlusion culling flickers: {counts:?}");
}

#[test]
#[ignore = "needs a GPU and the game"]
fn monday_occlusion_culling_hides_only_hidden_things() {
    let Some(culled) = monday::run(true, 240, 60, 6.0) else { return };
    let Some(all) = monday::run(false, 240, 60, 6.0) else { return };
    assert_eq!(culled.cameras, all.cameras, "the runs differ: not comparable");
    let pixels = (monday::W * monday::H) as usize;
    let differing: Vec<usize> =
        culled.frames.iter().zip(&all.frames).map(|(a, b)| (0..pixels).filter(|&i| monday::differs(a, b, i, 24)).count()).collect();
    assert!(differing.iter().all(|&n| n < pixels / 200), "{differing:?}");
}

#[test]
#[ignore = "survey: prints only; needs a GPU and the game"]
fn monday_peeing_flicker_report() {
    let input = |frame: usize| -> (Vec<&'static str>, [f32; 2]) {
        match frame {
            0 => (vec!["R"], [0.0; 2]),
            1..90 => (Vec::new(), [0.0, if frame < 20 { -2.0 } else { 0.0 }]),
            _ => (vec!["LeftMouse"], [0.0; 2]),
        }
    };
    for occlusion in [false, true] {
        let Some(run) = monday::run_with(occlusion, 600, 270, input) else { return };
        let flicker = monday::flickering(&run.frames[150..]);
        eprintln!("peeing, occlusion {occlusion}: flickering pixels per frame {flicker:?}");
    }
}

fn sprite_in_fog(fogged: bool) -> [u8; 3] {
    use bevy::pbr::{DistanceFog, FogFalloff};
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
        DistanceFog { color: Color::srgb(0.0, 0.0, 1.0), falloff: FogFalloff::Linear { start: 1.0, end: 10.0 }, ..Default::default() },
    ));
    let texel = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let texture = world.resource_mut::<Assets<Image>>().add(texel);
    let props = p2_particles::EmitterProperties { draw_style: p2_particles::DrawStyle::AlphaBlend, disable_fogging: !fogged, ..Default::default() };
    let material = world.resource_mut::<Assets<ParticleMaterial>>().add(ParticleMaterial::new(texture, &props));
    let sprite = world.resource_mut::<Assets<Mesh>>().add(quad(30.0, -20.0));
    world.spawn((Mesh3d(sprite), MeshMaterial3d(material), Transform::default(), NoFrustumCulling));
    let pixel = || frames.lock().expect("lock").last().map(|f| [0, 1, 2].map(|c| f[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize + c]));
    let mut last = None;
    let mut same = 0;
    for _ in 0..2000 {
        app.update();
        if common::compiling(&app) {
            same = 0;
            continue;
        }
        let Some(now) = pixel().filter(|&p| p != [43, 44, 47]) else { continue };
        same = if last == Some(now) { same + 1 } else { 0 };
        last = Some(now);
        if same >= 30 {
            break;
        }
    }
    last.expect("no read back")
}

#[test]
#[ignore = "needs a GPU"]
fn particles_take_the_scene_fog() {
    let fogged = sprite_in_fog(true);
    let clear = sprite_in_fog(false);
    assert!(fogged[0] < 8 && fogged[1] < 8 && fogged[2] > 240, "fogged {fogged:?}");
    assert!(clear.iter().all(|&c| c > 240), "DisableFogging {clear:?}");
}

fn blended_layer_under_a_transparent_decal(blended: bool) -> ([u8; 3], [u8; 3]) {
    use bevy::light::ClusteredDecal;
    use bevy::material::AlphaMode;
    use p2_engine::render::blended::{BlendedMaterial, OpaqueDecals};
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    let floor_mesh = world.resource_mut::<Assets<Mesh>>().add(quad(100.0, -20.0));
    let floor = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial {
        base_color: Color::srgb_u8(128, 128, 128),
        unlit: true,
        ..Default::default()
    });
    world.spawn((Mesh3d(floor_mesh), MeshMaterial3d(floor), Transform::default()));
    let layer_mesh = world.resource_mut::<Assets<Mesh>>().add(quad(100.0, -19.0));
    let layer = StandardMaterial { base_color: Color::srgba(0.9, 0.9, 0.9, 0.5), alpha_mode: AlphaMode::Blend, unlit: true, ..Default::default() };
    if blended {
        let material = world.resource_mut::<Assets<BlendedMaterial>>().add(BlendedMaterial { base: layer, extension: OpaqueDecals::default() });
        world.spawn((Mesh3d(layer_mesh), MeshMaterial3d(material), Transform::default()));
    } else {
        let material = world.resource_mut::<Assets<StandardMaterial>>().add(layer);
        world.spawn((Mesh3d(layer_mesh), MeshMaterial3d(material), Transform::default()));
    }
    let image = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let texture = world.resource_mut::<Assets<Image>>().add(image);
    world.spawn((ClusteredDecal { base_color_texture: Some(texture), ..Default::default() }, Transform::from_xyz(0.0, 0.0, -20.0).with_scale(bevy::math::Vec3::splat(6.0))));
    let pixel = |f: &[u8], x: u32, y: u32| [0, 1, 2].map(|c| f[((y * SIZE + x) * 4) as usize + c]);
    let read = || frames.lock().expect("lock").last().map(|f| (pixel(f, SIZE / 2, SIZE / 2), pixel(f, 1, 1)));
    let mut last = None;
    let mut same = 0;
    let drawn = |p: &[u8; 3]| *p != [43, 44, 47] && *p != [128, 128, 128];
    for _ in 0..2000 {
        app.update();
        if common::compiling(&app) {
            same = 0;
            continue;
        }
        let Some(now) = read().filter(|(middle, corner)| drawn(middle) && drawn(corner)) else { continue };
        same = if last == Some(now) { same + 1 } else { 0 };
        last = Some(now);
        if same >= 30 {
            break;
        }
    }
    last.expect("no read back")
}

#[test]
#[ignore = "needs a GPU"]
fn blended_layers_have_no_decal_squares() {
    let (middle, corner) = blended_layer_under_a_transparent_decal(false);
    assert!((0..3).any(|c| middle[c] + 4 < corner[c]), "Bevy's darker decal box no longer shows: {middle:?} {corner:?}");
    let (middle, corner) = blended_layer_under_a_transparent_decal(true);
    assert!((0..3).all(|c| middle[c].abs_diff(corner[c]) <= 1), "middle {middle:?}, corner {corner:?}");
}

fn projected_floor(vertex_lit: bool, floor: u8, texel: [u8; 3]) -> [u8; 3] {
    use bevy::light::ClusteredDecal;
    use p2_engine::render::blended::{BlendedMaterial, OpaqueDecals};
    use p2_engine::render::mesh_light::{DynamicLit, MeshLitMaterial};
    use p2_engine::render::shader_layers::Layers;
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    let floor_mesh = world.resource_mut::<Assets<Mesh>>().add(quad(100.0, -20.0));
    let base = StandardMaterial { base_color: Color::srgb_u8(floor, floor, floor), unlit: true, ..Default::default() };
    if vertex_lit {
        let extension = DynamicLit::new(&Layers::new(None, 1.0, None, Vec::new(), false));
        let material = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base, extension });
        world.spawn((Mesh3d(floor_mesh), MeshMaterial3d(material), Transform::default()));
    } else {
        let material = world.resource_mut::<Assets<BlendedMaterial>>().add(BlendedMaterial { base, extension: OpaqueDecals::default() });
        world.spawn((Mesh3d(floor_mesh), MeshMaterial3d(material), Transform::default()));
    }
    let image = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[texel[0], texel[1], texel[2], 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let texture = world.resource_mut::<Assets<Image>>().add(image);
    let tag = 0x8000_0000 | 0xc_0000 | 1;
    world.spawn((ClusteredDecal { base_color_texture: Some(texture), tag, ..Default::default() }, Transform::from_xyz(0.0, 0.0, -20.0).with_scale(bevy::math::Vec3::splat(30.0))));
    let pixel = |f: &[u8]| [0, 1, 2].map(|c| f[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize + c]);
    let mut last = None;
    let mut same = 0;
    for _ in 0..2000 {
        app.update();
        if common::compiling(&app) {
            same = 0;
            continue;
        }
        let Some(now) = frames.lock().expect("lock").last().map(|f| pixel(f)).filter(|p| *p != [43, 44, 47]) else { continue };
        same = if last == Some(now) { same + 1 } else { 0 };
        last = Some(now);
        if same >= 30 {
            break;
        }
    }
    last.expect("no read back")
}

#[test]
#[ignore = "needs a GPU"]
fn projectors_draw_on_standard_and_vertex_lit_floors() {
    for vertex_lit in [false, true] {
        for (floor, texel, expected) in [(128u8, [95u8, 0, 0], [95u8, 0, 0]), (128, [128; 3], [128; 3]), (64, [128; 3], [64; 3])] {
            let drawn = projected_floor(vertex_lit, floor, texel);
            assert!((0..3).all(|c| drawn[c].abs_diff(expected[c]) <= 6), "texel {texel:?} on {floor}: drawn {drawn:?}, expected {expected:?}");
        }
    }
}

fn floor_sprite_hidden_share(lift: f32) -> f32 {
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    let camera = world
        .spawn((
            Camera3d::default(),
            Camera::default(),
            RenderTarget::Image(target.into()),
            Tonemapping::None,
            DebandDither::Disabled,
            Msaa::Off,
            bevy::camera::Projection::Perspective(bevy::camera::PerspectiveProjection { near: 1.0, far: 500_000.0, ..Default::default() }),
            Transform::from_xyz(0.0, 60.0, 0.0).looking_at(bevy::math::Vec3::new(0.0, 0.0, -600.0), bevy::math::Vec3::Y),
            DepthPrepass,
        ))
        .id();
    let flat = |half: f32, y: f32, z: f32, flip: bool| {
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        let corners = [[-half, y, z - half], [half, y, z - half], [half, y, z + half], [-half, y, z + half]];
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners.to_vec());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.5, 0.5]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.5, 0.5]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0; 4]; 4]);
        mesh.insert_indices(Indices::U32(if flip { vec![0, 3, 1, 1, 3, 2] } else { vec![0, 2, 1, 0, 3, 2] }));
        mesh
    };
    let floor_mesh = world.resource_mut::<Assets<Mesh>>().add(flat(5000.0, 0.0, -1000.0, false));
    let floor = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial {
        base_color: Color::srgb(0.25, 0.25, 0.25),
        unlit: true,
        cull_mode: None,
        ..Default::default()
    });
    world.spawn((Mesh3d(floor_mesh), MeshMaterial3d(floor), Transform::default()));
    let texel = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let texture = world.resource_mut::<Assets<Image>>().add(texel);
    let props = p2_particles::EmitterProperties { draw_style: p2_particles::DrawStyle::Translucent, ..Default::default() };
    let material = world.resource_mut::<Assets<ParticleMaterial>>().add(ParticleMaterial::new(texture, &props));
    let sprite = world.resource_mut::<Assets<Mesh>>().add(flat(100.0, lift, -600.0, true));
    world.spawn((Mesh3d(sprite), MeshMaterial3d(material.clone()), Transform::default(), NoFrustumCulling));
    let marker = world.resource_mut::<Assets<Mesh>>().add(flat(30.0, 20.0, -400.0, true));
    world.spawn((Mesh3d(marker), MeshMaterial3d(material), Transform::from_xyz(-100.0, 0.0, 0.0), NoFrustumCulling));
    let window: Vec<(u32, u32)> = (SIZE / 2 - 4..SIZE / 2 + 4).flat_map(|x| (SIZE / 2 - 1..SIZE / 2 + 2).map(move |y| (x, y))).collect();
    let lit = |frame: &[u8], &(x, y): &(u32, u32)| red(frame, x, y) > 200;
    let mut first = None;
    for frame in 0..900 {
        if let Some(mut transform) = app.world_mut().get_mut::<Transform>(camera) {
            let sway = (frame as f32 * 0.37).sin() * 3.0;
            *transform = Transform::from_xyz(sway, 60.0 + (frame as f32 * 0.21).cos(), 0.0).looking_at(bevy::math::Vec3::new(0.0, 0.0, -600.0), bevy::math::Vec3::Y);
        }
        app.update();
        let frames = frames.lock().expect("lock");
        if first.is_none() {
            let marked = |f: &Vec<u8>| (0..SIZE / 3).any(|x| (0..SIZE).any(|y| red(f, x, y) > 200)) && red(f, SIZE / 2, SIZE - 1) != 43;
            first = frames.iter().position(marked);
        }
        if first.is_some_and(|first| frames.len() > first + 200) {
            break;
        }
    }
    let first = first.expect("the sprite never showed");
    let frames = frames.lock().expect("lock");
    let after = &frames[first..];
    let hidden: usize = after.iter().map(|f| window.iter().filter(|p| !lit(f, p)).count()).sum();
    hidden as f32 / (after.len() * window.len()) as f32
}

#[test]
#[ignore = "needs a GPU"]
fn floor_sprites_do_not_z_fight() {
    for lift in [0.0, -5.0] {
        let hidden = floor_sprite_hidden_share(lift);
        if lift == 0.0 {
            assert!(hidden < 0.01, "z-fighting: {hidden}");
        }
        if lift == -5.0 {
            assert!(hidden > 0.99, "shows through the floor: {hidden}");
        }
    }
}

/// Particle meshes have no bounds (`NoFrustumCulling`): Bevy's GPU occlusion culling gives them an infinite box, which
/// its culling shader turns into NaN screen bounds; whether such a mesh is drawn is then up to the GPU.
#[test]
#[ignore = "needs a GPU"]
fn particles_drawn_every_frame_with_occlusion_culling() {
    let (missing, _) = frames_without_the_sprite(false);
    assert_eq!(missing, 0);
    let (missing, _) = frames_without_the_sprite(true);
    assert_eq!(missing, 0);
}

fn decal_frames_changing(gpu_clustering: bool, frames_wanted: usize) -> (usize, usize) {
    use bevy::light::ClusteredDecal;
    use bevy::light::cluster::GlobalClusterSettings;
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    if !gpu_clustering {
        world.resource_mut::<GlobalClusterSettings>().gpu_clustering = None;
    }
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    let floor_mesh = world.resource_mut::<Assets<Mesh>>().add(quad(100.0, -20.0));
    let floor = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial {
        base_color: Color::srgb_u8(128, 128, 128),
        unlit: true,
        ..Default::default()
    });
    world.spawn((Mesh3d(floor_mesh), MeshMaterial3d(floor), Transform::default()));
    let colours: Vec<_> = [[200u8, 20, 20, 128], [20, 200, 20, 128], [20, 20, 200, 128], [220, 220, 30, 128], [30, 220, 220, 128], [10, 10, 10, 160]]
        .into_iter()
        .map(|texel| {
            let image = Image::new_fill(
                Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                TextureDimension::D2,
                &texel,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            world.resource_mut::<Assets<Image>>().add(image)
        })
        .collect();
    for i in 0..300 {
        let (x, y) = (((i * 37) % 41) as f32 - 20.0, ((i * 53) % 43) as f32 - 21.0);
        let texture = colours[i % colours.len()].clone();
        world.spawn((
            ClusteredDecal { base_color_texture: Some(texture), ..Default::default() },
            Transform::from_xyz(x * 0.4, y * 0.4, -20.0).with_scale(bevy::math::Vec3::new(8.0, 8.0, 4.0)),
        ));
    }
    let centre = |f: &Vec<u8>| [0, 1, 2].map(|c| f[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize + c]);
    let mut first = None;
    for _ in 0..3000 {
        app.update();
        let frames = frames.lock().expect("lock");
        if first.is_none() {
            first = frames.iter().position(|f| centre(f) != [43, 44, 47] && centre(f) != [128, 128, 128]).map(|k| k + 30);
        }
        if first.is_some_and(|first| frames.len() > first + frames_wanted) {
            break;
        }
    }
    let first = first.expect("decals never showed");
    let frames = frames.lock().expect("lock");
    let after = &frames[first..first + frames_wanted];
    (after.windows(2).filter(|pair| pair[0] != pair[1]).count(), after.len() - 1)
}

#[test]
#[ignore = "needs a GPU"]
fn overlapping_decals_are_steady() {
    let (changed, of) = decal_frames_changing(true, 120);
    eprintln!("GPU clustering: {changed} of {of} frames differ from the one before");
    let (changed, of) = decal_frames_changing(false, 120);
    eprintln!("CPU clustering: {changed} of {of} frames differ from the one before");
    assert_eq!(changed, 0);
}

fn overlapping_emitters_centre(actor_order: bool) -> [u8; 2] {
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    let texel = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &[255, 255, 255, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    let texture = world.resource_mut::<Assets<Image>>().add(texel);
    let props = p2_particles::EmitterProperties { draw_style: p2_particles::DrawStyle::AlphaBlend, ..Default::default() };
    let quads = |at: &[(f32, f32)], color: [f32; 4]| {
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        let positions: Vec<[f32; 3]> = at.iter().flat_map(|&(x, z)| [[x - 4.0, -4.0, z], [x + 4.0, -4.0, z], [x + 4.0, 4.0, z], [x - 4.0, 4.0, z]]).collect();
        let n = positions.len();
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.5, 0.5]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.5, 0.5]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![color; n]);
        mesh.insert_indices(Indices::U32((0..n as u32 / 4).flat_map(|q| [0, 1, 2, 0, 2, 3].map(|i| q * 4 + i)).collect()));
        mesh
    };
    let mut meshes = [quads(&[(0.0, -10.0)], [1.0, 0.0, 0.0, 0.5]), quads(&[(0.0, -10.0), (1000.0, -300.0)], [0.0, 1.0, 0.0, 0.5])];
    if actor_order {
        use bevy::camera::primitives::MeshAabb;
        let bounds: Vec<_> = meshes.iter().filter_map(|mesh| mesh.get_aabb()).collect();
        let min = bounds.iter().map(|b| bevy::math::Vec3::from(b.min())).reduce(bevy::math::Vec3::min).expect("bounds");
        let max = bounds.iter().map(|b| bevy::math::Vec3::from(b.max())).reduce(bevy::math::Vec3::max).expect("bounds");
        for mesh in &mut meshes {
            p2_engine::map::pad_bounds(mesh, min, max);
        }
    }
    let actor = world.spawn_empty().id();
    for (index, mesh) in meshes.into_iter().enumerate() {
        let material = world.resource_mut::<Assets<ParticleMaterial>>().add(ParticleMaterial::new(texture.clone(), &props));
        let mesh = world.resource_mut::<Assets<Mesh>>().add(mesh);
        let mut drawn = world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default(), NoFrustumCulling));
        if actor_order {
            drawn.insert(p2_engine::render::translucent_order::TranslucentDraw::Part { actor, order: index as u32 });
        }
    }
    let pixel = || frames.lock().expect("lock").last().map(|f| [0, 1, 2].map(|c| f[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize + c]));
    let (mut last, mut same) = (None, 0);
    for _ in 0..2000 {
        app.update();
        if common::compiling(&app) {
            same = 0;
            continue;
        }
        let Some(now) = pixel().filter(|p| p[0] > 60 && p[1] > 60) else { continue };
        same = if last == Some(now) { same + 1 } else { 0 };
        last = Some(now);
        if same >= 30 {
            break;
        }
    }
    let [r, g, _] = last.expect("no read back");
    [r, g]
}

#[test]
#[ignore = "needs a GPU"]
fn emitters_of_an_actor_draw_in_array_order() {
    let [r, g] = overlapping_emitters_centre(false);
    eprintln!("own centres: red {r}, green {g}");
    let [r, g] = overlapping_emitters_centre(true);
    eprintln!("actor bounds and array order: red {r}, green {g}");
    assert!(g > r + 20, "emitter 1 (green) not on top: red {r}, green {g}");
}
fn floor_decal_box(x: f32, z: f32, side: f32, top: f32, trace: f32) -> Transform {
    use bevy::math::{Mat4, Vec4};
    let centre = Vec4::new(x, top - trace / 2.0, z, 1.0);
    Transform::from_matrix(Mat4::from_cols(Vec4::X * side, Vec4::Z * side, Vec4::Y * trace, centre))
}

struct DecalCheck {
    missing: usize,
    extra: usize,
    unstable: usize,
    checked: usize,
}

fn spawn_floor_and_decals(world: &mut bevy::ecs::world::World, decal_box: impl Fn(f32, f32, f32) -> Transform) -> Vec<Transform> {
    use bevy::light::ClusteredDecal;
    let mut floor = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    let half = 5000.0;
    floor.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[-half, 0.0, -half], [half, 0.0, -half], [half, 0.0, half], [-half, 0.0, half]]);
    floor.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4]);
    floor.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.5, 0.5]; 4]);
    floor.insert_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]));
    let floor = world.resource_mut::<Assets<Mesh>>().add(floor);
    let grey = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial {
        base_color: Color::srgb_u8(128, 128, 128),
        unlit: true,
        ..Default::default()
    });
    world.spawn((Mesh3d(floor), MeshMaterial3d(grey), Transform::default()));
    let colours: Vec<_> = [[200u8, 20, 20, 255], [20, 200, 20, 255], [20, 20, 200, 255], [220, 220, 30, 255], [30, 220, 220, 255], [10, 10, 10, 255]]
        .into_iter()
        .map(|texel| {
            let image = Image::new_fill(
                Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                TextureDimension::D2,
                &texel,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            world.resource_mut::<Assets<Image>>().add(image)
        })
        .collect();
    let boxes: Vec<Transform> = (0..50)
        .map(|i| decal_box(((i * 37) % 23) as f32 * 25.0 - 275.0, -150.0 - ((i * 53) % 19) as f32 * 25.0, 60.0 + (i % 4) as f32 * 10.0))
        .collect();
    for (i, transform) in boxes.iter().enumerate() {
        world.spawn((ClusteredDecal { base_color_texture: Some(colours[i % colours.len()].clone()), ..Default::default() }, *transform));
    }
    boxes
}

fn turning_over_decals(gpu_clustering: bool, decal_box: impl Fn(f32, f32, f32) -> Transform) -> DecalCheck {
    use bevy::light::cluster::GlobalClusterSettings;
    use bevy::math::{EulerRot, Quat, Vec3};
    const PERIOD: usize = 120;
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    if !gpu_clustering {
        world.resource_mut::<GlobalClusterSettings>().gpu_clustering = None;
    }
    let eye = Vec3::new(0.0, 60.0, 0.0);
    let pose = |k: usize| {
        let phase = (k % PERIOD) as f32 / PERIOD as f32;
        let yaw = 1.1 * (4.0 * (phase - 0.5).abs() - 1.0);
        Transform::from_translation(eye).with_rotation(Quat::from_euler(EulerRot::YXZ, yaw, -0.3, 0.0))
    };
    let camera = world
        .spawn((
            Camera3d::default(),
            Camera::default(),
            RenderTarget::Image(target.into()),
            Tonemapping::None,
            DebandDither::Disabled,
            Msaa::Off,
            bevy::camera::Projection::Perspective(bevy::camera::PerspectiveProjection { near: 1.0, far: 500_000.0, ..Default::default() }),
            pose(0),
            DepthPrepass,
        ))
        .id();
    let boxes = spawn_floor_and_decals(world, decal_box);
    let inverses: Vec<_> = boxes.iter().map(|t| t.to_matrix().inverse()).collect();
    let tan = (std::f32::consts::FRAC_PI_4 / 2.0).tan();
    let expected = |k: usize, x: u32, y: u32| -> Option<bool> {
        let ndc = [(x as f32 + 0.5) / SIZE as f32 * 2.0 - 1.0, 1.0 - (y as f32 + 0.5) / SIZE as f32 * 2.0];
        let dir = pose(k).rotation * Vec3::new(ndc[0] * tan, ndc[1] * tan, -1.0);
        let along = -eye.y / dir.y;
        if dir.y >= 0.0 || along * dir.length() > 2500.0 {
            return None;
        }
        let hit = eye + dir * along;
        let locals: Vec<Vec3> = inverses.iter().map(|m| m.transform_point3(hit)).collect();
        if locals.iter().any(|l| l.x.abs() <= 0.45 && l.y.abs() <= 0.45) {
            Some(true)
        } else if locals.iter().all(|l| l.x.abs() > 0.55 || l.y.abs() > 0.55) {
            Some(false)
        } else {
            None
        }
    };
    let bare = |f: &[u8], x: u32, y: u32| (0..3).all(|c| f[((y * SIZE + x) * 4) as usize + c].abs_diff(128) <= 3);
    let mut shown = 0;
    for tries in 0.. {
        assert!(tries < 4000, "the floor never showed");
        if shown == 30 {
            break;
        }
        app.update();
        if shown > 0 || frames.lock().expect("lock").last().is_some_and(|f| (0..SIZE).any(|x| bare(f, x, SIZE - 1))) {
            shown += 1;
        }
    }
    let first = frames.lock().expect("lock").len();
    for k in 0..3 * PERIOD + 4 {
        if let Some(mut transform) = app.world_mut().get_mut::<Transform>(camera) {
            *transform = pose(k);
        }
        app.update();
    }
    let frames = frames.lock().expect("lock");
    let mismatches = |lag: usize, k: usize| {
        let f = &frames[first + k + lag];
        let (mut missing, mut extra, mut checked) = (0, 0, 0);
        for y in 0..SIZE {
            for x in 0..SIZE {
                match expected(k, x, y) {
                    Some(true) if bare(f, x, y) => missing += 1,
                    Some(false) if !bare(f, x, y) => extra += 1,
                    _ => {}
                }
                checked += usize::from(expected(k, x, y).is_some());
            }
        }
        (missing, extra, checked)
    };
    let poses = PERIOD..3 * PERIOD;
    let lag = (0..3).min_by_key(|&lag| poses.clone().map(|k| mismatches(lag, k).0 + mismatches(lag, k).1).sum::<usize>()).unwrap_or(0);
    let (mut missing, mut extra, mut checked) = (0, 0, 0);
    for k in poses.clone() {
        let (m, e, c) = mismatches(lag, k);
        (missing, extra, checked) = (missing + m, extra + e, checked + c);
    }
    let unstable = poses
        .take(PERIOD)
        .map(|k| {
            let (a, b) = (&frames[first + k + lag], &frames[first + k + PERIOD + lag]);
            (0..SIZE * SIZE).filter(|&i| (0..3).any(|c| a[i as usize * 4 + c] != b[i as usize * 4 + c])).count()
        })
        .sum();
    DecalCheck { missing, extra, unstable, checked }
}

#[test]
#[ignore = "needs a GPU"]
fn decals_stay_while_the_camera_turns() {
    let splat = |x: f32, z: f32, side: f32| p2_engine::map::projector_box([x, z, 0.0].into(), [-16384, 0, 0].into(), side, 32.0);
    let at_the_face = |x: f32, z: f32, side: f32| floor_decal_box(x, z, side, 0.0, 32.0);
    for (name, gpu, make) in [("GPU clustering", true, &splat as &dyn Fn(f32, f32, f32) -> Transform), ("box face on the floor", false, &at_the_face), ("CPU clustering", false, &splat)] {
        let DecalCheck { missing, extra, unstable, checked } = turning_over_decals(gpu, make);
        eprintln!("{name}: {missing} splat pixels bare, {extra} bare pixels with a splat, {unstable} unstable, of {checked}");
        if name == "CPU clustering" {
            assert_eq!((missing, extra, unstable), (0, 0, 0));
        }
    }
}

fn skinned_lit_quad(toward: Option<bool>) -> [u8; 3] {
    use p2_engine::render::actor_light::{ActorLitMaterial, HardwareLightUniform, HardwareLighting, SkinnedLighting};
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    let mut lighting = SkinnedLighting { ambient: bevy::math::Vec3::splat(0.25), ..Default::default() };
    if let Some(toward) = toward {
        lighting.count = 1;
        lighting.lights[0] = HardwareLightUniform {
            direction: bevy::math::Vec3::new(0.0, 0.0, if toward { -1.0 } else { 1.0 }),
            kind: 2,
            color: bevy::math::Vec4::new(0.25, 0.25, 0.25, 0.0),
            ..Default::default()
        };
    }
    let base = StandardMaterial { unlit: true, ..Default::default() };
    let material = world.resource_mut::<Assets<ActorLitMaterial>>().add(ActorLitMaterial { base, extension: HardwareLighting { lighting, ..Default::default() } });
    let mesh = world.resource_mut::<Assets<Mesh>>().add(quad(100.0, -20.0));
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default()));
    let pixel = |f: &[u8]| [0, 1, 2].map(|c| f[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize + c]);
    let mut last = None;
    let mut same = 0;
    for _ in 0..2000 {
        app.update();
        if common::compiling(&app) {
            same = 0;
            continue;
        }
        let Some(now) = frames.lock().expect("lock").last().map(|f| pixel(f)).filter(|p| *p != [43, 44, 47]) else { continue };
        same = if last == Some(now) { same + 1 } else { 0 };
        last = Some(now);
        if same >= 30 {
            break;
        }
    }
    last.expect("no read back")
}

#[test]
#[ignore = "needs a GPU"]
fn skinned_actors_lit_like_the_driver() {
    let ambient = skinned_lit_quad(None);
    let front = skinned_lit_quad(Some(true));
    let back = skinned_lit_quad(Some(false));
    assert!(ambient.iter().all(|&c| c.abs_diff(128) <= 2), "{ambient:?}");
    assert!(front.iter().all(|&c| c >= 253), "{front:?}");
    assert_eq!(back, ambient);
}

fn bsp_lit_quad(light_z: Option<f32>) -> [u8; 3] {
    use p2_engine::render::lightmapped::{BakedLight, LightmappedMaterial};
    dynamic_lit_quad(light_z, quad(100.0, -20.0), |world, mesh| {
        let material = world.resource_mut::<Assets<LightmappedMaterial>>().add(LightmappedMaterial { base: StandardMaterial::default(), extension: BakedLight::default() });
        world.spawn((mesh, MeshMaterial3d(material), Transform::default()));
    })
}

fn dynamic_lit_quad(light_z: Option<f32>, mesh: Mesh, surface: impl FnOnce(&mut bevy::ecs::world::World, Mesh3d)) -> [u8; 3] {
    use p2_engine::render::bsp_light::DynamicLight;
    let (mut app, frames, target) = app();
    let world = app.world_mut();
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    if let Some(z) = light_z {
        let light = bevy::light::PointLight {
            color: Color::LinearRgba(bevy::color::LinearRgba::rgb(0.25, 0.25, 0.25)),
            intensity: 4.0 * std::f32::consts::PI,
            range: 100.0,
            shadow_maps_enabled: false,
            ..Default::default()
        };
        world.spawn((DynamicLight, light, Transform::from_xyz(0.0, 0.0, z)));
    }
    let mesh = world.resource_mut::<Assets<Mesh>>().add(mesh);
    surface(world, Mesh3d(mesh));
    let pixel = |f: &[u8]| [0, 1, 2].map(|c| f[((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize + c]);
    let mut last = None;
    let mut same = 0;
    for _ in 0..2000 {
        app.update();
        if common::compiling(&app) {
            same = 0;
            continue;
        }
        let Some(now) = frames.lock().expect("lock").last().map(|f| pixel(f)).filter(|p| *p != [43, 44, 47]) else { continue };
        same = if last == Some(now) { same + 1 } else { 0 };
        last = Some(now);
        if same >= 30 {
            break;
        }
    }
    last.expect("no read back")
}

#[test]
#[ignore = "needs a GPU"]
fn bsp_lit_by_dynamic_lights_like_retail() {
    let dark = bsp_lit_quad(None);
    let front = bsp_lit_quad(Some(-10.0));
    let back = bsp_lit_quad(Some(-30.0));
    assert!(dark.iter().all(|&c| c <= 2), "{dark:?}");
    assert!(front.iter().all(|&c| c.abs_diff(58) <= 3), "{front:?}");
    assert_eq!(back, front);
}

#[test]
#[ignore = "needs a GPU"]
fn vertex_lit_meshes_take_dynamic_lights() {
    use p2_engine::render::mesh_light::{DynamicLit, MeshLitMaterial};
    let lit_quad = |light_z| {
        let mut mesh = quad(100.0, -20.0);
        let baked = 0.5f32.powf(2.2);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[baked, baked, baked, 1.0]; 4]);
        dynamic_lit_quad(light_z, mesh, |world, mesh| {
            let base = StandardMaterial { unlit: true, ..StandardMaterial::default() };
            let material = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base, extension: DynamicLit::default() });
            world.spawn((mesh, MeshMaterial3d(material), Transform::default()));
        })
    };
    let dark = lit_quad(None);
    let front = lit_quad(Some(-10.0));
    let back = lit_quad(Some(-30.0));
    assert!(dark.iter().all(|&c| c.abs_diff(128) <= 2), "{dark:?}");
    assert!(front.iter().all(|&c| c.abs_diff(251) <= 3), "{front:?}");
    assert_eq!(back, dark);
}

#[test]
#[ignore = "needs a GPU"]
fn shader_layers_like_the_driver() {
    use p2_engine::render::mesh_light::{DynamicLit, MeshLitMaterial};
    use p2_engine::render::shader_layers::{Layers, SpecularCoords, SpecularMode, SpecularParams};
    let texel = |world: &mut bevy::ecs::world::World, rgba: [u8; 4]| {
        let size = Extent3d { width: 1, height: 1, depth_or_array_layers: 1 };
        let image = Image::new_fill(size, TextureDimension::D2, &rgba, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
        world.resource_mut::<Assets<Image>>().add(image)
    };
    let layered_quad = |detail: bool, glow: bool, specular: Option<SpecularCoords>, premultiplied: bool| {
        let mut mesh = quad(100.0, -20.0);
        let baked = 0.5f32.powf(2.2);
        let alpha = if premultiplied { 0.5 } else { 1.0 };
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[baked, baked, baked, alpha]; 4]);
        dynamic_lit_quad(None, mesh, |world, mesh| {
            let detail = detail.then(|| texel(world, [64, 64, 64, 255]));
            let glow = glow.then(|| texel(world, [255, 0, 0, 255]));
            let emissive = if glow.is_some() { bevy::color::LinearRgba::WHITE } else { bevy::color::LinearRgba::BLACK };
            let alpha_mode = if premultiplied { bevy::material::AlphaMode::Blend } else { bevy::material::AlphaMode::Opaque };
            let base = StandardMaterial { unlit: true, emissive, emissive_texture: glow, alpha_mode, ..StandardMaterial::default() };
            let face: &[[u8; 4]] = &[[64, 64, 64, 255]];
            let image = specular.map(|coords| {
                let image = if coords == SpecularCoords::Cube { world.resource_mut::<Assets<Image>>().add(p2_bevy_assets::cube_image([(1, 1, face); 6])) } else { texel(world, face[0]) };
                (image, SpecularParams { mode: SpecularMode::Add, world: false, constant: 1.0, coords })
            });
            let layers = Layers::new(detail, 1.0, image, Vec::new(), premultiplied);
            let (base, extension) = (layers.lit(base), DynamicLit::new(&layers));
            let material = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base, extension });
            world.spawn((mesh, MeshMaterial3d(material), Transform::default()));
        })
    };
    let plain = layered_quad(false, false, None, false);
    let detailed = layered_quad(true, false, None, false);
    let glowing = layered_quad(false, true, None, false);
    let shiny = layered_quad(false, false, Some(SpecularCoords::Cube), false);
    let flat_env = layered_quad(false, false, Some(SpecularCoords::EnvMap), false);
    let flat_uv = layered_quad(false, false, Some(SpecularCoords::BaseUv), false);
    let glass = layered_quad(false, false, Some(SpecularCoords::Cube), true);
    assert!(plain.iter().all(|&c| c.abs_diff(128) <= 2), "{plain:?}");
    assert!(detailed.iter().all(|&c| c.abs_diff(64) <= 3), "{detailed:?}");
    assert!(glowing[0] >= 253 && glowing[1..].iter().all(|&c| c.abs_diff(128) <= 2), "{glowing:?}");
    assert!(shiny.iter().all(|&c| c.abs_diff(192) <= 5), "{shiny:?}");
    assert_eq!(flat_env, shiny);
    assert_eq!(flat_uv, shiny);
    assert!(glass.iter().all(|&c| c.abs_diff(163) <= 5), "{glass:?}");
}
