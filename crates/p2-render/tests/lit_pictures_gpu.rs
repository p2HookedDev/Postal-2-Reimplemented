mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bevy::app::App;
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::{Color, LinearRgba};
use bevy::ecs::observer::On;
use bevy::ecs::world::World;
use bevy::image::Image;
use bevy::light::{AmbientLight, CascadeShadowConfigBuilder, DirectionalLight, PointLight};
use bevy::material::AlphaMode;
use bevy::math::{Rect, UVec4, Vec3, Vec4};
use bevy::mesh::{Indices, Mesh, Mesh3d, MeshTag, PrimitiveTopology};
use bevy::pbr::{DistanceFog, FogFalloff, Lightmap, MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::storage::ShaderBuffer;
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_host::display::{MODERN_GRAPHICS, Settings};
use p2_render::view::{CASTER_LAYER, SUN_VIEW_LAYER};
use p2_render::actor_light::{ActorLightPlugin, ActorLitMaterial, HardwareLightUniform, HardwareLighting, SkinnedLighting};
use p2_render::blended::{BlendedMaterial, BlendedPlugin, OpaqueDecals, TerrainLayer, TerrainLayerMaterial};
use p2_render::bsp_light::DynamicLight;
use p2_render::lightmapped::{BakedLight, LightmappedMaterial, LightmappedPlugin};
use p2_render::mesh_light::{DynamicLit, MeshLightPlugin, MeshLitMaterial};
use p2_render::particle_material::{ParticleMaterial, ParticlePlugin};
use p2_render::terrain_material::{MAX_LAYERS, TerrainLayers, TerrainMaterial, TerrainParams};

const SIZE: u32 = 64;
const CLEAR: [u8; 3] = [43, 44, 47];
const HALF: f32 = 10.0;
const DEPTH: f32 = -20.0;
const TERRAIN_LIGHT: [[f32; 4]; 4] = [[0.3, 0.4, 0.6, 1.0], [0.9, 0.7, 0.5, 1.0], [0.6, 0.9, 0.4, 1.0], [1.0, 0.8, 0.95, 1.0]];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Look {
    Retail,
    Modern,
}

impl Look {
    fn name(self) -> &'static str {
        match self {
            Self::Retail => "retail",
            Self::Modern => "modern",
        }
    }

    fn flag(self) -> f32 {
        match self {
            Self::Retail => 0.0,
            Self::Modern => 1.0,
        }
    }
}

fn seen(look: Look) -> RenderLayers {
    match look {
        Look::Retail => RenderLayers::layer(0),
        Look::Modern => RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]),
    }
}

fn quad(colors: [[f32; 4]; 4]) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    let corners = [[-HALF, -HALF, DEPTH], [HALF, -HALF, DEPTH], [HALF, HALF, DEPTH], [-HALF, HALF, DEPTH]];
    let uvs = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners.to_vec());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors.to_vec());
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

fn image(side: u32, texels: Vec<u8>, format: TextureFormat) -> Image {
    Image::new(Extent3d { width: side, height: side, depth_or_array_layers: 1 }, TextureDimension::D2, texels, format, RenderAssetUsages::default())
}

fn alpha_map() -> Image {
    image(2, vec![0, 0, 0, 255, 128, 0, 0, 255, 255, 0, 0, 255, 64, 0, 0, 255], TextureFormat::Rgba8Unorm)
}

fn texels(seed: u8) -> Vec<u8> {
    (0..16u8).flat_map(|i| [i.wrapping_mul(53).wrapping_add(seed), 255 - i * 11, (i % 4) * 60 + seed / 2, 255]).collect()
}

fn picture(look: Look, surface: fn(&mut World, Look, Handle<Image>)) -> Vec<u8> {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((LightmappedPlugin, MeshLightPlugin, ActorLightPlugin, BlendedPlugin, ParticlePlugin));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    world.init_resource::<Settings>();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let latest = Arc::new(Mutex::new(Vec::<u8>::new()));
    let store = latest.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").clone_from(&event.data);
    });
    world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        Transform::default(),
        seen(look),
        AmbientLight { brightness: 0.0, ..AmbientLight::default() },
        DistanceFog { color: Color::srgb(0.2, 0.3, 0.6), falloff: FogFalloff::Linear { start: 21.0, end: 26.0 }, ..Default::default() },
    ));
    let light = PointLight {
        color: Color::LinearRgba(LinearRgba::rgb(1.6, 1.4, 1.2)),
        intensity: 4.0 * std::f32::consts::PI,
        range: 14.0,
        shadow_maps_enabled: false,
        ..Default::default()
    };
    world.spawn((DynamicLight, light, Transform::from_xyz(2.0, -1.5, -14.0)));
    if look == Look::Modern {
        let sun = DirectionalLight {
            color: Color::LinearRgba(LinearRgba::rgb(0.9, 0.7, 0.4)),
            illuminance: 1.0,
            shadow_maps_enabled: true,
            affects_lightmapped_mesh_diffuse: false,
            shadow_depth_bias: 0.02,
            ..DirectionalLight::default()
        };
        let cascades = CascadeShadowConfigBuilder { num_cascades: 4, minimum_distance: 0.1, maximum_distance: 60.0, first_cascade_far_bound: 15.0, overlap_proportion: 0.1 };
        let toward = Transform::default().looking_to(Vec3::new(0.3, -0.5, -0.8), Vec3::Y);
        world.spawn((sun, cascades.build(), toward, RenderLayers::from_layers(&[SUN_VIEW_LAYER, CASTER_LAYER])));
    }
    let texture = world.resource_mut::<Assets<Image>>().add(image(4, texels(0), TextureFormat::Rgba8UnormSrgb));
    surface(world, look, texture);
    let centre = ((SIZE / 2 * SIZE + SIZE / 2) * 4) as usize;
    let mut last = Vec::new();
    let mut same = 0;
    for _ in 0..2000 {
        app.update();
        if common::compiling(&app) {
            same = 0;
            continue;
        }
        let now = latest.lock().expect("lock").clone();
        if now.len() != (SIZE * SIZE * 4) as usize || now[centre..centre + 3] == CLEAR {
            continue;
        }
        same = if now == last { same + 1 } else { 0 };
        if same >= 30 {
            return now;
        }
        last = now;
    }
    panic!("{}: the picture never held still", look.name());
}

fn compare(name: &str, frame: &[u8]) -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("pictures").join(format!("{name}.rgba"));
    let Ok(stored) = std::fs::read(&path) else {
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("the pictures folder");
        std::fs::write(&path, frame).expect("the picture written");
        return Some(format!("{name}: no stored picture, written to {} now; run again", path.display()));
    };
    let pixel = |bytes: &[u8], i: usize| bytes.get(i * 4..i * 4 + 4).map(<[u8]>::to_vec);
    let differing: Vec<usize> = (0..frame.len().max(stored.len()) / 4).filter(|&i| pixel(frame, i) != pixel(&stored, i)).collect();
    let &first = differing.first()?;
    let (x, y) = (first % SIZE as usize, first / SIZE as usize);
    Some(format!(
        "{name}: {} pixels differ, the first at ({x}, {y}): stored {:?}, drawn {:?}",
        differing.len(),
        pixel(&stored, first),
        pixel(frame, first)
    ))
}

fn holds(material: &str, looks: &[Look], surface: fn(&mut World, Look, Handle<Image>)) {
    let failures: Vec<String> = looks
        .iter()
        .filter_map(|&look| {
            let first = picture(look, surface);
            let second = picture(look, surface);
            if first != second {
                return Some(format!("{material} {}: two draws of the same scene differ", look.name()));
            }
            compare(&format!("{material}_{}", look.name()), &first)
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[derive(Clone, Copy)]
enum Finish {
    Plain,
    Darken,
    Glass,
}

impl Finish {
    fn apply(self, base: &mut StandardMaterial, blend: &mut Vec4) {
        match self {
            Self::Plain => {}
            Self::Darken => blend.x = 1.0,
            Self::Glass => {
                base.base_color = Color::srgba(1.0, 0.95, 0.9, 0.6);
                base.alpha_mode = AlphaMode::Blend;
            }
        }
    }
}

fn light_map(world: &mut World, look: Look, texture: Handle<Image>, finish: Finish) {
    let light = image(2, vec![255, 200, 60, 255, 90, 160, 255, 255, 220, 220, 220, 255, 30, 60, 120, 255], TextureFormat::Rgba8UnormSrgb);
    let light = world.resource_mut::<Assets<Image>>().add(light);
    let mut base = StandardMaterial { base_color_texture: Some(texture), lightmap_exposure: 2.0 / bevy::camera::Exposure::default().exposure(), ..StandardMaterial::default() };
    let mut extension = BakedLight::default();
    extension.layers.detail.z = look.flag();
    finish.apply(&mut base, &mut extension.layers.blend);
    let material = world.resource_mut::<Assets<LightmappedMaterial>>().add(LightmappedMaterial { base, extension });
    let mesh = world.resource_mut::<Assets<Mesh>>().add(quad([[1.0; 4]; 4]));
    let lightmap = Lightmap { image: light, uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0), bicubic_sampling: false };
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), lightmap, Transform::default(), seen(look)));
}

fn vertex_light(world: &mut World, look: Look, texture: Handle<Image>, finish: Finish) {
    let mut base = StandardMaterial { base_color_texture: Some(texture), ..StandardMaterial::default() };
    let mut extension = DynamicLit::default();
    extension.layers.detail.z = look.flag();
    finish.apply(&mut base, &mut extension.layers.blend);
    let material = world.resource_mut::<Assets<MeshLitMaterial>>().add(MeshLitMaterial { base, extension });
    let colors = [[0.2, 0.5, 0.9, 1.0], [1.6, 0.8, 0.3, 1.0], [0.4, 1.9, 1.2, 1.0], [3.0, 2.2, 0.6, 1.0]];
    let mesh = world.resource_mut::<Assets<Mesh>>().add(quad(colors));
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default(), seen(look)));
}

fn hardware_lights(world: &mut World, look: Look, texture: Handle<Image>) {
    let base = StandardMaterial { base_color_texture: Some(texture), ..StandardMaterial::default() };
    let mut lighting = SkinnedLighting { ambient: Vec3::new(0.15, 0.2, 0.3), count: 2, ..Default::default() };
    lighting.lights[0] = HardwareLightUniform {
        position: Vec4::ZERO,
        direction: Vec3::new(0.3, -0.4, -0.866),
        kind: 2,
        color: Vec4::new(0.7, 0.35, 0.2, 0.0),
        attenuation: Vec4::new(1.0, 0.0, 0.0, 0.0),
    };
    lighting.lights[1] = HardwareLightUniform {
        position: Vec4::new(4.0, 3.0, -16.0, 12.0),
        direction: Vec3::ZERO,
        kind: 0,
        color: Vec4::new(0.9, 0.8, 0.25, 0.0),
        attenuation: Vec4::new(0.4, 0.05, 0.0, 0.0),
    };
    let mut extension = HardwareLighting { lighting, ..Default::default() };
    extension.layers.detail.z = look.flag();
    let material = world.resource_mut::<Assets<ActorLitMaterial>>().add(ActorLitMaterial { base, extension });
    let mesh = world.resource_mut::<Assets<Mesh>>().add(quad([[1.0; 4]; 4]));
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default(), seen(look)));
}

fn terrain(world: &mut World, look: Look, texture: Handle<Image>) {
    let mut images = world.resource_mut::<Assets<Image>>();
    let second = images.add(image(4, texels(70), TextureFormat::Rgba8UnormSrgb));
    let alpha_map = images.add(alpha_map());
    let mut u = [Vec4::ZERO; MAX_LAYERS];
    let mut v = [Vec4::ZERO; MAX_LAYERS];
    [u[0], v[0]] = [Vec4::new(0.05, 0.0, 0.0, 0.5), Vec4::new(0.0, -0.05, 0.0, 0.5)];
    [u[1], v[1]] = [Vec4::new(0.07, 0.03, 0.0, 0.2), Vec4::new(-0.03, 0.08, 0.0, 0.1)];
    let params = TerrainParams { u, v, count: UVec4::new(2, u32::from(look == Look::Modern), 0, 0), ..Default::default() };
    let extension = TerrainLayers { params, texture_0: Some(texture), texture_1: Some(second), alpha_map_1: Some(alpha_map), ..Default::default() };
    let material = world.resource_mut::<Assets<TerrainMaterial>>().add(TerrainMaterial { base: StandardMaterial::default(), extension });
    let mesh = world.resource_mut::<Assets<Mesh>>().add(quad(TERRAIN_LIGHT));
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default(), seen(look)));
}

fn terrain_layer(world: &mut World, look: Look, texture: Handle<Image>) {
    let alpha_map = world.resource_mut::<Assets<Image>>().add(alpha_map());
    let base = StandardMaterial { base_color_texture: Some(texture), alpha_mode: AlphaMode::Blend, ..StandardMaterial::default() };
    let mut extension = TerrainLayer { alpha_map: Some(alpha_map), ..Default::default() };
    extension.layers.detail.z = look.flag();
    let material = world.resource_mut::<Assets<TerrainLayerMaterial>>().add(TerrainLayerMaterial { base, extension });
    let mesh = world.resource_mut::<Assets<Mesh>>().add(quad(TERRAIN_LIGHT));
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default(), seen(look)));
}

fn blended(world: &mut World, look: Look, texture: Handle<Image>) {
    let lamp = PointLight { color: Color::srgb(1.0, 0.8, 0.5), intensity: 2_000_000.0, range: 30.0, shadow_maps_enabled: false, ..Default::default() };
    world.spawn((lamp, Transform::from_xyz(-3.0, 2.0, -13.0)));
    let base = StandardMaterial {
        base_color: Color::srgba(1.0, 0.9, 0.8, 0.75),
        base_color_texture: Some(texture),
        alpha_mode: AlphaMode::Blend,
        ..StandardMaterial::default()
    };
    let material = world.resource_mut::<Assets<BlendedMaterial>>().add(BlendedMaterial { base, extension: OpaqueDecals::default() });
    let mesh = world.resource_mut::<Assets<Mesh>>().add(quad([[1.0; 4]; 4]));
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::default(), seen(look)));
}

fn sprite(world: &mut World, look: Look, texture: Handle<Image>) {
    let floats = |v: [f32; 3]| v.map(f32::to_bits);
    let mut header = [0u32; 20];
    header[0..3].copy_from_slice(&floats([0.6, 0.9, 1.3]));
    header[7] = 1;
    let mut record = [0u32; 20];
    record[0..3].copy_from_slice(&floats([0.0, 0.0, DEPTH]));
    record[3] = u32::from_le_bytes([230, 200, 160, 180]);
    record[4..7].copy_from_slice(&floats([0.0, HALF, 0.0]));
    record[8..11].copy_from_slice(&floats([HALF, 0.0, 0.0]));
    record[12..20].copy_from_slice(&[0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0].map(f32::to_bits));
    let records = world.resource_mut::<Assets<ShaderBuffer>>().add(ShaderBuffer::new(vec![header, record], RenderAssetUsages::default()));
    let props = p2_particles::EmitterProperties { draw_style: p2_particles::DrawStyle::AlphaBlend, ..Default::default() };
    let mut material = ParticleMaterial::new(texture, &props);
    material.sprites = records;
    let material = world.resource_mut::<Assets<ParticleMaterial>>().add(material);
    if look == Look::Modern {
        world.insert_resource(Settings::read(|query| if query == MODERN_GRAPHICS { "True".to_owned() } else { String::new() }));
    }
    let mut grid = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    grid.insert_attribute(Mesh::ATTRIBUTE_POSITION, (0..4).map(|corner| [0.0, corner as f32, 0.0]).collect::<Vec<_>>());
    grid.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    let grid = world.resource_mut::<Assets<Mesh>>().add(grid);
    world.spawn((Mesh3d(grid), MeshMaterial3d(material), MeshTag::new(0), NoFrustumCulling, Transform::default(), seen(look)));
}

#[test]
#[ignore = "needs a GPU"]
fn the_light_map_picture_holds() {
    holds("light_map", &[Look::Retail, Look::Modern], |world, look, texture| light_map(world, look, texture, Finish::Plain));
}

#[test]
#[ignore = "needs a GPU"]
fn the_vertex_light_picture_holds() {
    holds("vertex_light", &[Look::Retail, Look::Modern], |world, look, texture| vertex_light(world, look, texture, Finish::Plain));
}

#[test]
#[ignore = "needs a GPU"]
fn the_light_map_darken_picture_holds() {
    holds("light_map_darken", &[Look::Modern], |world, look, texture| light_map(world, look, texture, Finish::Darken));
}

#[test]
#[ignore = "needs a GPU"]
fn the_light_map_glass_picture_holds() {
    holds("light_map_glass", &[Look::Modern], |world, look, texture| light_map(world, look, texture, Finish::Glass));
}

#[test]
#[ignore = "needs a GPU"]
fn the_vertex_light_darken_picture_holds() {
    holds("vertex_light_darken", &[Look::Modern], |world, look, texture| vertex_light(world, look, texture, Finish::Darken));
}

#[test]
#[ignore = "needs a GPU"]
fn the_vertex_light_glass_picture_holds() {
    holds("vertex_light_glass", &[Look::Modern], |world, look, texture| vertex_light(world, look, texture, Finish::Glass));
}

#[test]
#[ignore = "needs a GPU"]
fn the_sprite_picture_holds() {
    holds("sprite", &[Look::Retail, Look::Modern], sprite);
}

#[test]
#[ignore = "needs a GPU"]
fn the_hardware_lights_picture_holds() {
    holds("hardware_lights", &[Look::Retail, Look::Modern], hardware_lights);
}

#[test]
#[ignore = "needs a GPU"]
fn the_terrain_picture_holds() {
    holds("terrain", &[Look::Retail, Look::Modern], terrain);
}

#[test]
#[ignore = "needs a GPU"]
fn the_terrain_layer_picture_holds() {
    holds("terrain_layer", &[Look::Retail, Look::Modern], terrain_layer);
}

#[test]
#[ignore = "needs a GPU"]
fn the_blended_picture_holds() {
    holds("blended", &[Look::Retail], blended);
}
