mod common;

use bevy::app::App;
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::camera::{Camera3d, RenderTarget};
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::{MeshInputUniform, MeshMaterial3d, MeshUniform, RenderMeshInstances, StandardMaterial};
use bevy::prelude::{AlphaMode, Transform};
use bevy::render::RenderApp;
use bevy::render::batching::gpu_preprocessing::BatchedInstanceBuffers;
use bevy::render::mesh::allocator::MeshAllocator;
use bevy::render::render_resource::TextureFormat;
use bevy::render::sync_world::MainEntity;

fn quads(mesh: &mut Mesh, count: usize, shift: f32) {
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    for i in 0..count {
        let c = Vec3::new(i as f32 + shift, 0.0, -5.0);
        let base = positions.len() as u32;
        positions.extend([c, c + Vec3::X, c + Vec3::X + Vec3::Y, c + Vec3::Y].map(|v| v.to_array()));
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let n = positions.len();
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0; 4]; n]);
    mesh.insert_indices(Indices::U32(indices));
}

#[test]
#[ignore = "needs a GPU"]
fn rewritten_meshes_are_drawn_from_the_current_frame() {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let target = world.resource_mut::<Assets<Image>>().add(Image::new_target_texture(64, 64, TextureFormat::Rgba8UnormSrgb, None));
    world.spawn((Camera3d::default(), RenderTarget::Image(target.into()), Transform::default()));
    let material = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { unlit: true, alpha_mode: AlphaMode::Add, ..Default::default() });
    let mut meshes = Vec::new();
    for _ in 0..2 {
        let mesh: Handle<Mesh> = world.resource_mut::<Assets<Mesh>>().add(Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default()));
        let entity = world.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone()), Transform::default(), NoFrustumCulling)).id();
        meshes.push((entity, mesh));
    }
    for frame in 0..12 {
        for (i, (_, mesh)) in meshes.iter().enumerate() {
            let count = if i == 0 { 1 + frame } else { 1 + (frame * 7) % 5 };
            if let Some(mut m) = app.world_mut().resource_mut::<Assets<Mesh>>().get_mut(mesh) {
                quads(&mut m, count, frame as f32 * 0.1);
            }
        }
        app.update();
        let render = app.sub_app_mut(RenderApp).world_mut();
        let allocator = render.resource::<MeshAllocator>();
        let instances = render.resource::<RenderMeshInstances>();
        let RenderMeshInstances::GpuBuilding(_) = instances else {
            eprintln!("no GPU preprocessing on this device: draw ranges come from the mesh allocator at draw time");
            return;
        };
        let inputs = render.resource::<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>();
        for (entity, mesh) in &meshes {
            let vertices = allocator.mesh_vertex_slice(&mesh.id()).expect("vertices").range;
            let indices = allocator.mesh_index_slice(&mesh.id()).expect("indices").range;
            let index = instances.render_mesh_queue_data(MainEntity::from(*entity)).expect("instance").current_uniform_index;
            let input = inputs.current_input_buffer.get(index.0).expect("input");
            assert_eq!(
                (input.first_vertex_index, input.first_index_index, input.index_count),
                (vertices.start, indices.start, indices.end - indices.start),
                "frame {frame}"
            );
        }
    }
}

fn drawn_over(style: p2_particles::DrawStyle, texel: [u8; 4], background: f32) -> [u8; 3] {
    use std::sync::{Arc, Mutex};
    use bevy::camera::Camera;
    use bevy::color::Color;
    use bevy::ecs::observer::On;
    use bevy::render::gpu_readback::{Readback, ReadbackComplete};
    use bevy::render::render_resource::{Extent3d, TextureDimension};
    use bevy::render::view::{DebandDither, Msaa, Tonemapping};
    use p2_engine::render::particle_material::{ParticleMaterial, ParticlePlugin};

    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins(ParticlePlugin);
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(64, 64, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= bevy::render::render_resource::TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.clone().into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
    ));
    let quad = |z: f32| {
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
        let corners = [[-10.0, -10.0, z], [10.0, -10.0, z], [10.0, 10.0, z], [-10.0, 10.0, z]];
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners.to_vec());
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.5, 0.5]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.5, 0.5]; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0; 4]; 4]);
        mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
        mesh
    };
    let back_mesh = world.resource_mut::<Assets<Mesh>>().add(quad(-6.0));
    let front_mesh = world.resource_mut::<Assets<Mesh>>().add(quad(-5.0));
    let back = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial {
        base_color: Color::srgb(background, background, background),
        unlit: true,
        ..Default::default()
    });
    world.spawn((Mesh3d(back_mesh), MeshMaterial3d(back), Transform::default()));
    let mut image = Image::new_fill(
        Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &texel,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = bevy::image::ImageSampler::nearest();
    let texture = world.resource_mut::<Assets<Image>>().add(image);
    let props = p2_particles::EmitterProperties { draw_style: style, ..Default::default() };
    let material = world.resource_mut::<Assets<ParticleMaterial>>().add(ParticleMaterial::new(texture, &props));
    let pixels = Arc::new(Mutex::new(Vec::new()));
    let store = pixels.clone();
    world.spawn(Readback::texture(target)).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").clone_from(&event.data);
    });
    let at = (32 * 64 + 32) * 4;
    let bare = [(background * 255.0).round() as u8; 3];
    let pixel = || pixels.lock().expect("lock").get(at..at + 3).map(|p| [p[0], p[1], p[2]]);
    for _ in 0..2000 {
        app.update();
        if pixel().is_some_and(|p| p.iter().zip(bare).all(|(&a, b)| a.abs_diff(b) <= 1)) {
            break;
        }
    }
    app.world_mut().spawn((Mesh3d(front_mesh), MeshMaterial3d(material), Transform::default()));
    let mut last = None;
    let mut same = 0;
    for _ in 0..2000 {
        app.update();
        let Some(now) = pixel() else { continue };
        same = if last == Some(now) { same + 1 } else { 0 };
        last = Some(now);
        if same >= 10 && now.iter().zip(bare).any(|(&a, b)| a.abs_diff(b) > 1) {
            break;
        }
    }
    last.expect("no readback")
}

#[test]
#[ignore = "needs a GPU"]
fn fire_sprites_on_the_gpu_match_the_engine() {
    use p2_particles::DrawStyle;
    let texel = [255, 153, 51, 102];
    for style in [DrawStyle::Translucent, DrawStyle::Brighten] {
        for background in [64u8, 90, 115, 150] {
            let d = f32::from(background) / 255.0;
            let engine = [255u8, 153, 51].map(|s| {
                let s = f32::from(s) / 255.0;
                let out = if style == DrawStyle::Translucent { s + d } else { s + d * (1.0 - s) };
                (out.min(1.0) * 255.0).round() as u8
            });
            let gpu = drawn_over(style, texel, d);
            let tolerance = if background == 64 { 3 } else { 40 };
            for c in 0..3 {
                assert!(gpu[c].abs_diff(engine[c]) <= tolerance, "{style:?} over {background}: GPU {gpu:?} vs engine {engine:?}");
            }
        }
    }
}
