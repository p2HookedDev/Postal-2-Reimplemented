mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::{App, Update};
use bevy::asset::{Assets, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Camera, Camera3d, Exposure, RenderTarget};
use bevy::color::{Alpha, Color, LinearRgba};
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::observer::On;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::{Image, ImageAddressMode};
use bevy::light::{AmbientLight, CascadeShadowConfigBuilder, ClusteredDecal, DirectionalLight};
use bevy::math::{Mat4, Rect, Vec3, Vec4};
use bevy::mesh::{Indices, Mesh, Mesh3d, PrimitiveTopology};
use bevy::pbr::{Lightmap, MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::RenderApp;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_collision::{BspHulls, MeshCollision, MeshSet};
use p2_host::display::{MODERN_GRAPHICS, MODERN_INDIRECT, MODERN_LIQUIDS, Settings};
use p2_math::{Rotator, Vector};
use p2_particles::{SpriteParticle, StripPoint};
use p2_render::actor_light::LightEnvironment;
use p2_render::frame_facts::FrameFacts;
use p2_render::level_collision::{LevelAlbedos, LevelCollision, StaticCollision};
use p2_render::lightmapped::{BakedLight, LightmappedMaterial, LightmappedPlugin};
use p2_render::liquids::LiquidsPlugin;
use p2_render::modern::view_ambient;
use p2_render::particle_frame::{DrawnActor, DrawnEmitter, DrawnParticles, EmitterShape, Liquid, LiquidRole, Pour};
use p2_render::probes::ProbesPlugin;
use p2_render::view::{CASTER_LAYER, SUN_VIEW_LAYER, SceneView, ShownScene};
use p2_render::{MainPassExtraSystems, RenderFollowSystems};
use p2_render_cpu::{LightEffect, MeshLight};
use p2_vm::FluidType;
use rustc_hash::FxHashMap;

const W: u32 = 640;
const H: u32 = 360;
const FLOOR: f32 = -6.0;

fn image(width: u32, height: u32, texels: Vec<u8>, format: TextureFormat) -> Image {
    Image::new(Extent3d { width, height, depth_or_array_layers: 1 }, TextureDimension::D2, texels, format, RenderAssetUsages::default())
}

fn hash(x: u32, y: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65535.0
}

fn concrete() -> Image {
    let side = 64;
    let texels = (0..side * side)
        .flat_map(|i| {
            let grain = 0.8 + 0.4 * hash(i % side, i / side) * hash(i / side / 4, i % side / 4);
            let v = (130.0 * grain) as u8;
            [v, v, (f32::from(v) * 0.96) as u8, 255]
        })
        .collect();
    image(side, side, texels, TextureFormat::Rgba8UnormSrgb)
}

fn tile() -> Image {
    let side = 128;
    let texels = (0..side * side)
        .flat_map(|i| {
            let (x, y) = (i % side, i / side);
            let grout = [x, y].into_iter().any(|c| matches!(c % 64, 0 | 63));
            let colour = if grout {
                [110.0, 105.0, 100.0]
            } else if (x / 64 + y / 64) % 2 == 0 {
                [205.0, 185.0, 150.0]
            } else {
                [160.0, 170.0, 190.0]
            };
            let grain = 0.95 + 0.1 * hash(x, y);
            let [r, g, b] = colour.map(|c: f32| (c * grain) as u8);
            [r, g, b, 255]
        })
        .collect();
    image(side, side, texels, TextureFormat::Rgba8UnormSrgb)
}

#[derive(Clone, Copy)]
enum Floor {
    Concrete,
    Tile,
}

impl Floor {
    fn texture(self) -> Image {
        let mut image = match self {
            Self::Concrete => concrete(),
            Self::Tile => tile(),
        };
        image.sampler = p2_bevy_assets::texture_quality().sampler(0, 1, [ImageAddressMode::Repeat; 2]);
        image
    }

    fn least_spread(self) -> u32 {
        match self {
            Self::Concrete => 6,
            Self::Tile => 25,
        }
    }

    fn repeat(self) -> f32 {
        match self {
            Self::Concrete => 32.0,
            Self::Tile => 64.0,
        }
    }
}

fn splat() -> Image {
    let side = 64u32;
    let blobs = [(32.0, 32.0, 14.0), (20.0, 26.0, 6.0), (44.0, 40.0, 5.0), (12.0, 44.0, 3.0), (52.0, 16.0, 3.5), (40.0, 54.0, 2.5)];
    let texels = (0..side * side)
        .flat_map(|i| {
            let (x, y) = ((i % side) as f32 + 0.5, (i / side) as f32 + 0.5);
            let cover = blobs.iter().map(|&(bx, by, r)| ((r - ((x - bx).powi(2) + (y - by).powi(2)).sqrt()) / 1.5).clamp(0.0, 1.0)).fold(0.0, f32::max);
            [95, 0, 0, (cover * 230.0) as u8]
        })
        .collect();
    image(side, side, texels, TextureFormat::Rgba8UnormSrgb)
}

fn floor_mesh(repeat: f32) -> Mesh {
    let half = 400.0;
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    let corners = [[-half, FLOOR, half], [half, FLOOR, half], [half, FLOOR, -half], [-half, FLOOR, -half]];
    let tiles = half * 2.0 / repeat;
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, corners.to_vec());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, tiles], [tiles, tiles], [tiles, 0.0], [0.0, 0.0]]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32; 4]; 4]);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

fn floor_collision() -> StaticCollision {
    let corner = |x: f32, y: f32| [x, y, FLOOR];
    let floor = [[corner(-400.0, -400.0), corner(400.0, -400.0), corner(400.0, 400.0)], [corner(-400.0, -400.0), corner(400.0, 400.0), corner(-400.0, 400.0)]];
    let meshes = vec![MeshCollision::placed(floor, Vector::ZERO, Rotator::ZERO, Vector::new(1.0, 1.0, 1.0), Vector::ZERO)];
    StaticCollision {
        model: p2_assets::Model::default(),
        hulls: BspHulls::default(),
        terrains: Vec::new(),
        mesh_set: MeshSet::new(&meshes),
        meshes,
        lamp_casters: floor.to_vec(),
        lamp_masked: Vec::new(),
        albedos: LevelAlbedos::default(),
    }
}

fn lying(places: &[(f32, f32)], half: f32) -> DrawnEmitter {
    let sprites = places
        .iter()
        .enumerate()
        .map(|(serial, &(x, y))| SpriteParticle {
            location: Vector::new(x, y, FLOOR),
            previous: Vector::new(x, y, FLOOR),
            size: [half * 2.0; 2],
            up: Vector::new(0.0, half, 0.0),
            right: Vector::new(half, 0.0, 0.0),
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
            serial: serial as u32,
        })
        .collect();
    DrawnEmitter { sprites, shape: EmitterShape { facing: Some(Vec3::Z), ..EmitterShape::default() }, ..DrawnEmitter::default() }
}

fn blood(parent: bevy::ecs::entity::Entity, role: LiquidRole, emitter: DrawnEmitter, pour: Option<Pour>) -> DrawnActor {
    let liquid = Liquid { kind: FluidType::Blood, role, burning: false, pour };
    DrawnActor { parent, drawn: true, seen: true, liquid: Some(liquid), emitters: vec![emitter], ..DrawnActor::default() }
}

fn stream(parent: bevy::ecs::entity::Entity) -> DrawnActor {
    let from = Vector::new(-70.0, -60.0, FLOOR + 40.0);
    let velocity = Vector::new(40.0, -120.0, 60.0);
    let gravity = Vector::new(0.0, 0.0, -950.0);
    let strip = (0..=12).map(|step| {
        let t = step as f32 * 0.03;
        let at = from + velocity * t + gravity * (0.5 * t * t);
        StripPoint { location: at, size: 2.0, color: [255; 4] }
    });
    let pour = Pour { from, velocity: [velocity, velocity], acceleration: gravity, rate: 60.0, flowing: true };
    blood(parent, LiquidRole::Stream, DrawnEmitter { strip: strip.collect(), ..DrawnEmitter::default() }, Some(pour))
}

fn decal_box(x: f32, y: f32, side: f32) -> Transform {
    let trace = 16.0;
    let centre = Vec4::new(x, FLOOR + 4.0 - trace / 2.0, y, 1.0);
    Transform::from_matrix(Mat4::from_cols(Vec4::X * side, Vec4::Z * side, Vec4::Y * trace, centre))
}

fn blood_scene(ground: Floor, sun_way: Vec3) -> Vec<u8> {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((LightmappedPlugin, LiquidsPlugin, ProbesPlugin));
    app.add_systems(Update, view_ambient.after(RenderFollowSystems));
    app.insert_resource(Settings::read(|query| match query {
        MODERN_GRAPHICS => "True".to_owned(),
        MODERN_LIQUIDS => "10".to_owned(),
        MODERN_INDIRECT => "1".to_owned(),
        _ => String::new(),
    }));
    let sun_light = MeshLight { effect: LightEffect::Sunlight, rotation: Vector::new(sun_way.x, sun_way.z, sun_way.y).rotation(), brightness: 128.0, ..MeshLight::default() };
    let lights = FxHashMap::from_iter([(0, sun_light)]);
    app.insert_resource(ShownScene { lighting: Some(Arc::new(LightEnvironment::new(lights, vec![[0.06, 0.06, 0.07]], 1.0))), ..ShownScene::default() });
    app.insert_resource(FrameFacts { in_play: true, game_step: 1.0 / 60.0, ..FrameFacts::default() });
    app.insert_resource(DrawnParticles::default());
    app.insert_resource(LevelCollision(Some(Arc::new(floor_collision()))));
    app.sub_app_mut(RenderApp).configure_sets(
        Core3d,
        MainPassExtraSystems::Liquids.in_set(Core3dSystems::MainPass).after(main_opaque_pass_3d).before(main_transparent_pass_3d),
    );
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(W, H, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let latest = Arc::new(Mutex::new(Vec::<u8>::new()));
    let store = latest.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").clone_from(&event.data);
    });
    let seen = RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]);
    let camera = world.spawn((
        Camera3d::default(),
        Camera::default(),
        RenderTarget::Image(target.into()),
        Tonemapping::None,
        DebandDither::Disabled,
        Msaa::Off,
        DepthPrepass,
        AmbientLight { brightness: 0.0, ..AmbientLight::default() },
        Transform::from_xyz(0.0, FLOOR + 64.0, 0.0).looking_at(Vec3::new(0.0, FLOOR, -90.0), Vec3::Y),
        SceneView::default(),
        seen.clone(),
    ))
    .id();
    let sun = DirectionalLight {
        color: Color::LinearRgba(LinearRgba::rgb(1.0, 0.95, 0.85)),
        illuminance: 1.0,
        shadow_maps_enabled: true,
        affects_lightmapped_mesh_diffuse: false,
        shadow_depth_bias: 0.02,
        ..DirectionalLight::default()
    };
    let cascades = CascadeShadowConfigBuilder { num_cascades: 4, minimum_distance: 0.1, maximum_distance: 400.0, first_cascade_far_bound: 60.0, overlap_proportion: 0.1 };
    let toward = Transform::default().looking_to(sun_way, Vec3::Y);
    world.spawn((sun, cascades.build(), toward, RenderLayers::from_layers(&[SUN_VIEW_LAYER, CASTER_LAYER])));
    let texture = world.resource_mut::<Assets<Image>>().add(ground.texture());
    let light = world.resource_mut::<Assets<Image>>().add(image(2, 2, [170, 165, 155, 255].repeat(4), TextureFormat::Rgba8UnormSrgb));
    let base = StandardMaterial { base_color_texture: Some(texture), lightmap_exposure: 2.0 / Exposure::default().exposure(), ..StandardMaterial::default() };
    let mut extension = BakedLight::default();
    extension.layers.detail.z = 1.0;
    let material = world.resource_mut::<Assets<LightmappedMaterial>>().add(LightmappedMaterial { base, extension });
    let mesh = world.resource_mut::<Assets<Mesh>>().add(floor_mesh(ground.repeat()));
    let lightmap = Lightmap { image: light, uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0), bicubic_sampling: false };
    world.spawn((Mesh3d(mesh), MeshMaterial3d(material), lightmap, Transform::default(), seen));
    let splat = world.resource_mut::<Assets<Image>>().add(splat());
    let marker = world.resource_mut::<Assets<Image>>().add(image(1, 1, vec![128, 128, 255, 255], TextureFormat::Rgba8Unorm));
    let tag = 0x8000_0000 | 0x1_0000 | 2;
    world.spawn((ClusteredDecal { base_color_texture: Some(splat), normal_map_texture: Some(marker), tag, ..Default::default() }, decal_box(45.0, -80.0, 40.0)));
    let parents: Vec<_> = (0..3).map(|_| world.spawn_empty().id()).collect();
    let mut drawn = world.resource_mut::<DrawnParticles>();
    drawn.actors.push(blood(parents[0], LiquidRole::Puddle, lying(&[(-20.0, -95.0)], 30.0), None));
    let trail: Vec<(f32, f32)> = (0..8).map(|i| (5.0 + i as f32 * 6.0, -55.0 - i as f32 * 4.0)).collect();
    drawn.actors.push(blood(parents[1], LiquidRole::Trail, lying(&trail, 6.0), None));
    drawn.actors.push(stream(parents[2]));

    let start = Instant::now();
    let floor_pixel = ((H * 7 / 8 * W + W / 2) * 4) as usize;
    let mut settled = 0;
    loop {
        let mut facts = app.world_mut().resource_mut::<FrameFacts>();
        facts.level_time += 1.0 / 60.0;
        app.update();
        let now = latest.lock().expect("lock").clone();
        let floor_drawn = now.len() == (W * H * 4) as usize && now[floor_pixel..floor_pixel + 3] != [43, 44, 47];
        let probed = app.world().get::<AmbientLight>(camera).is_some_and(|ambient| ambient.color.alpha() > 0.0);
        settled = if floor_drawn && probed && !common::compiling(&app) { settled + 1 } else { 0 };
        if settled >= 180 {
            return now;
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "the scene never settled: floor drawn {floor_drawn}, probes baked {probed}, compiling {}",
            common::compiling(&app)
        );
    }
}

#[test]
#[ignore = "needs a GPU; writes a picture to look at"]
fn blood_on_a_lit_floor_picture() {
    for (ground, prefix, (floor_x, floor_y)) in [(Floor::Concrete, "", (320, 330)), (Floor::Tile, "tile_", (418, 112))] {
        for (name, sun_way) in [("front", Vec3::new(0.4, -0.6, -0.7)), ("back", Vec3::new(-0.15, -0.45, 0.88))] {
            let frame = blood_scene(ground, sun_way);
            let red = frame.as_chunks::<4>().0.iter().filter(|[r, g, ..]| *r >= 8 && u16::from(*r) > 2 * u16::from(*g) + 4).count();
            let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("blood_look_{prefix}{name}.rgba"));
            std::fs::write(&path, &frame).expect("the picture written");
            eprintln!("PICTURE {} {W}x{H}, {red} blood pixels", path.display());
            assert!(red > 200, "{prefix}{name}: no blood drawn: {red} pixels");
            let pixel = |x: u32, y: u32| frame[((y * W + x) * 4) as usize..][..3].to_vec();
            let brightness = |rgb: &[u8]| (u32::from(rgb[0]) * 299 + u32::from(rgb[1]) * 587 + u32::from(rgb[2]) * 114) / 1000;
            let luma = |x: u32| brightness(&pixel(x, H * 7 / 8));
            let lumas: Vec<u32> = (40..600).step_by(2).map(luma).collect();
            let spread = lumas.iter().max().unwrap_or(&0) - lumas.iter().min().unwrap_or(&0);
            assert!(spread > ground.least_spread(), "{prefix}{name}: the floor is one flat colour, its luma spreads {spread}");
            let (pool, floor) = (pixel(240, 180), pixel(floor_x, floor_y));
            assert!(
                brightness(&pool) * 5 < brightness(&floor) * 4 && pool[1] * 2 <= pool[0],
                "{prefix}{name}: the pool {pool:?} against the floor {floor:?}"
            );
        }
    }
}
