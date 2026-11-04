mod common;

use std::sync::{Arc, Mutex};

use bevy::app::App;
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::Color;
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::ecs::observer::On;
use bevy::image::Image;
use bevy::math::{Affine3A, Vec3};
use bevy::mesh::Mesh;
use bevy::pbr::StandardMaterial;
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_engine::render::lightmapped::{BakedLight, LightmappedMaterial};
use p2_engine::render::world_pass::{WorldCamera, WorldDraws, WorldPassPlugin, WorldSeenZones, WorldSection, chunks_of};

const SIZE: u32 = 64;
const CLEAR: u8 = 43;

struct Scene {
    app: App,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
}

fn scene(depth_prepass: bool) -> Scene {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((p2_engine::render::lightmapped::LightmappedPlugin, WorldPassPlugin));
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
    let mut camera = world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::from_xyz(0.0, 0.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
        WorldCamera,
    ));
    if depth_prepass {
        camera.insert(DepthPrepass);
    }
    Scene { app, frames }
}

fn add_quad(scene: &mut Scene, half: f32) -> (Handle<Mesh>, Arc<[p2_engine::render::world_pass::Chunk]>) {
    let mut mesh = common::quad(half, 0.0, [1.0; 4]);
    let chunks = chunks_of(&mut mesh).expect("a triangle list");
    (scene.app.world_mut().resource_mut::<Assets<Mesh>>().add(mesh), chunks)
}

fn show(scene: &mut Scene, sections: Vec<WorldSection>) {
    *scene.app.world_mut().resource_mut::<WorldDraws>() = WorldDraws { sections: sections.into() };
}

fn run_until(scene: &mut Scene, seen: impl Fn(&dyn Fn(u32, u32) -> [u8; 4]) -> bool) -> Option<Vec<u8>> {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(90) {
        scene.app.update();
        let frames = scene.frames.lock().expect("lock");
        if let Some(last) = frames.last().filter(|last| !last.is_empty()) {
            let pixel = |x: u32, y: u32| {
                let at = ((y * SIZE + x) * 4) as usize;
                [last[at], last[at + 1], last[at + 2], last[at + 3]]
            };
            if seen(&pixel) {
                return Some(last.clone());
            }
        }
    }
    None
}

fn red_material(scene: &mut Scene) -> Handle<StandardMaterial> {
    let material = StandardMaterial { base_color: Color::srgb(1.0, 0.0, 0.0), unlit: true, ..StandardMaterial::default() };
    scene.app.world_mut().resource_mut::<Assets<StandardMaterial>>().add(material)
}

#[test]
#[ignore = "needs a GPU"]
fn a_section_draws_without_an_entity() {
    let mut scene = scene(false);
    let (mesh, chunks) = add_quad(&mut scene, 5.0);
    let material = red_material(&mut scene);
    let section = WorldSection { mesh: mesh.id(), material: material.id().untyped(), world_from_local: Affine3A::IDENTITY, lightmap: None, chunks, zone: None };
    show(&mut scene, vec![section]);
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] > 200);
    assert!(frame.is_some(), "the section never drew");
}

#[test]
#[ignore = "needs a GPU"]
fn the_depth_prepass_takes_the_section_too() {
    let mut scene = scene(true);
    let (mesh, chunks) = add_quad(&mut scene, 5.0);
    let material = red_material(&mut scene);
    let section = WorldSection { mesh: mesh.id(), material: material.id().untyped(), world_from_local: Affine3A::IDENTITY, lightmap: None, chunks, zone: None };
    show(&mut scene, vec![section]);
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] > 200);
    assert!(frame.is_some(), "the section never drew with a depth prepass");
}

#[test]
#[ignore = "needs a GPU"]
fn a_placed_section_moves_and_a_section_out_of_view_is_culled() {
    let mut scene = scene(false);
    let (mesh, chunks) = add_quad(&mut scene, 2.0);
    let material = red_material(&mut scene);
    let at = |x: f32| WorldSection {
        mesh: mesh.id(),
        material: material.id().untyped(),
        world_from_local: Affine3A::from_translation(Vec3::new(x, 0.0, 0.0)),
        lightmap: None,
        chunks: chunks.clone(),
        zone: None,
    };
    show(&mut scene, vec![at(5.0), at(-5000.0)]);
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2 + SIZE / 4, SIZE / 2)[0] > 200);
    let frame = frame.expect("the placed section never drew");
    let left = ((SIZE / 2 * SIZE + SIZE / 4) * 4) as usize;
    assert_eq!(frame[left], CLEAR, "drawn on the wrong side");
}

#[test]
#[ignore = "needs a GPU"]
fn a_light_map_reaches_the_section_in_the_retail_look() {
    let mut scene = scene(false);
    let (mesh, chunks) = add_quad(&mut scene, 5.0);
    let world = scene.app.world_mut();
    let image = |texels: [u8; 4]| {
        Image::new(Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, texels.to_vec(), TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default())
    };
    let white = world.resource_mut::<Assets<Image>>().add(image([255, 255, 255, 255]));
    let light = world.resource_mut::<Assets<Image>>().add(image([0, 255, 0, 255]));
    let base = StandardMaterial { base_color_texture: Some(white), lightmap_exposure: 1.0 / bevy::camera::Exposure::default().exposure(), ..StandardMaterial::default() };
    let material = world.resource_mut::<Assets<LightmappedMaterial>>().add(LightmappedMaterial { base, extension: BakedLight::default() });
    let section = WorldSection { mesh: mesh.id(), material: material.id().untyped(), world_from_local: Affine3A::IDENTITY, lightmap: Some(light.id()), chunks, zone: None };
    show(&mut scene, vec![section]);
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| {
        let [r, g, _, _] = pixel(SIZE / 2, SIZE / 2);
        g > 100 && r < 60
    });
    let last = scene.frames.lock().expect("lock").last().cloned().unwrap_or_default();
    let at = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    assert!(frame.is_some(), "the light map never showed: centre {:?}", last.get(at..at + 4));
    let swapped = image([255, 0, 0, 255]);
    *scene.app.world_mut().resource_mut::<Assets<Image>>().get_mut(&light).expect("the light map") = swapped;
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| {
        let [r, g, _, _] = pixel(SIZE / 2, SIZE / 2);
        r > 100 && g < 60
    });
    assert!(frame.is_some(), "a light map swapped under its handle never showed");
    drop((light, material));
}

#[test]
#[ignore = "needs a GPU"]
fn a_section_stays_drawn_while_other_meshes_and_its_own_are_uploaded_again() {
    let mut scene = scene(false);
    let (mesh, chunks) = add_quad(&mut scene, 5.0);
    let material = red_material(&mut scene);
    let section = WorldSection { mesh: mesh.id(), material: material.id().untyped(), world_from_local: Affine3A::IDENTITY, lightmap: None, chunks, zone: None };
    show(&mut scene, vec![section]);
    let red_centre = |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] > 200;
    assert!(run_until(&mut scene, red_centre).is_some(), "the section never drew");
    for round in 0..30 {
        {
            let mut meshes = scene.app.world_mut().resource_mut::<Assets<Mesh>>();
            drop(meshes.add(common::quad(1.0 + round as f32, 0.0, [1.0; 4])));
            let mut drawn = meshes.get_mut(&mesh).expect("kept in the main world");
            drawn.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0, 1.0, 1.0, 1.0]; 4]);
        }
        scene.app.update();
    }
    for _ in 0..5 {
        scene.app.update();
    }
    let frames = scene.frames.lock().expect("lock");
    let last = frames.last().expect("a frame");
    let at = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    assert!(last[at] > 200, "the section was lost after its mesh moved: {:?}", &last[at..at + 4]);
}

#[test]
#[ignore = "needs a GPU"]
fn a_section_shows_only_while_its_zone_is_seen() {
    let mut scene = scene(false);
    let (mesh, chunks) = add_quad(&mut scene, 5.0);
    let material = red_material(&mut scene);
    let section = WorldSection { mesh: mesh.id(), material: material.id().untyped(), world_from_local: Affine3A::IDENTITY, lightmap: None, chunks, zone: Some(40) };
    show(&mut scene, vec![section]);
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] > 200);
    assert!(frame.is_some(), "the section never drew with its zone seen");
    let mut seen = [u32::MAX; 8];
    seen[1] &= !(1 << 8);
    *scene.app.world_mut().resource_mut::<WorldSeenZones>() = WorldSeenZones(seen);
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] == CLEAR);
    assert!(frame.is_some(), "the section stayed with its zone unseen");
}

#[test]
#[ignore = "needs a GPU"]
fn a_section_comes_back_after_its_camera_was_off() {
    let mut scene = scene(false);
    let (mesh, chunks) = add_quad(&mut scene, 5.0);
    let material = red_material(&mut scene);
    let section = WorldSection { mesh: mesh.id(), material: material.id().untyped(), world_from_local: Affine3A::IDENTITY, lightmap: None, chunks, zone: None };
    show(&mut scene, vec![section]);
    assert!(run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] > 200).is_some(), "the section never drew");
    for active in [false, true] {
        let world = scene.app.world_mut();
        for mut camera in world.query::<&mut Camera>().iter_mut(world) {
            camera.is_active = active;
        }
        for _ in 0..10 {
            scene.app.update();
        }
    }
    let frames = scene.frames.lock().expect("lock");
    let last = frames.last().expect("a frame");
    let at = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    assert!(last[at] > 200, "the section was lost with its camera: {:?}", &last[at..at + 4]);
}

#[test]
#[ignore = "needs a GPU"]
fn a_material_turning_masked_takes_the_masked_pipeline() {
    let mut scene = scene(false);
    let (mesh, chunks) = add_quad(&mut scene, 5.0);
    let clear_red = StandardMaterial { base_color: Color::srgba(1.0, 0.0, 0.0, 0.0), unlit: true, ..StandardMaterial::default() };
    let material = scene.app.world_mut().resource_mut::<Assets<StandardMaterial>>().add(clear_red);
    let section = WorldSection { mesh: mesh.id(), material: material.id().untyped(), world_from_local: Affine3A::IDENTITY, lightmap: None, chunks, zone: None };
    show(&mut scene, vec![section]);
    assert!(run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] > 200).is_some(), "the section never drew");
    let mut materials = scene.app.world_mut().resource_mut::<Assets<StandardMaterial>>();
    materials.get_mut(&material).expect("the material").alpha_mode = bevy::prelude::AlphaMode::Mask(0.5);
    let frame = run_until(&mut scene, |pixel: &dyn Fn(u32, u32) -> [u8; 4]| pixel(SIZE / 2, SIZE / 2)[0] == CLEAR);
    assert!(frame.is_some(), "the old pipeline stayed: nothing was discarded");
}
