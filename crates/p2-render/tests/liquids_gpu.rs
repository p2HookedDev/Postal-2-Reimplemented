mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::App;
use bevy::asset::Assets;
use bevy::camera::{Camera, Camera3d, RenderTarget};
use bevy::color::Color;
use bevy::core_pipeline::core_3d::{main_opaque_pass_3d, main_transparent_pass_3d};
use bevy::core_pipeline::prepass::DepthPrepass;
use bevy::core_pipeline::schedule::{Core3d, Core3dSystems};
use bevy::ecs::entity::Entity;
use bevy::ecs::observer::On;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::AmbientLight;
use bevy::mesh::{Mesh, Mesh3d};
use bevy::pbr::{MeshMaterial3d, StandardMaterial};
use bevy::prelude::Transform;
use bevy::render::RenderApp;
use bevy::render::extract_resource::ExtractResourcePlugin;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::{DebandDither, Msaa, Tonemapping};
use p2_host::settings::{MODERN_GRAPHICS, MODERN_LIQUIDS, Settings};
use p2_math::Vector;
use p2_render::MainPassExtraSystems;
use p2_render::world::frame_facts::FrameFacts;
use p2_render::effects::liquids::LiquidsPlugin;
use p2_render::effects::liquids::layer::{APRON, FILM, FILM_LIFE, LayerFrame, LayerSlot, LayerSource, SIDE, SLOT_SIDE, SLOTS_SIDE, TEXELS, new_liquid_atlas};
use p2_render::effects::liquids::layer_gpu::LayerGpuPlugin;
use p2_render::effects::particle_frame::{DrawnActor, DrawnEmitter, DrawnParticles, Liquid, LiquidRole, Pour};
use p2_particles::StripPoint;
use p2_render::world::view::SceneView;
use p2_vm::FluidType;

const SIZE: u32 = 128;

fn is_red([r, _, b, _]: [u8; 4]) -> bool {
    u16::from(r) > u16::from(b) + 30
}

fn red_pixels(frame: &Mutex<Vec<u8>>) -> usize {
    frame.lock().expect("lock").as_chunks::<4>().0.iter().filter(|&&pixel| is_red(pixel)).count()
}

fn rendering(eye: Transform, game_step: f32) -> (App, Arc<Mutex<Vec<u8>>>, Entity) {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins(LiquidsPlugin);
    app.insert_resource(Settings::read(|query| match query {
        MODERN_GRAPHICS => "True".to_owned(),
        MODERN_LIQUIDS => "10".to_owned(),
        _ => String::new(),
    }));
    app.insert_resource(FrameFacts { in_play: true, game_step, ..FrameFacts::default() });
    app.insert_resource(DrawnParticles::default());
    app.sub_app_mut(RenderApp).configure_sets(
        Core3d,
        MainPassExtraSystems::Liquids.in_set(Core3dSystems::MainPass).after(main_opaque_pass_3d).before(main_transparent_pass_3d),
    );
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let mut target = Image::new_target_texture(SIZE, SIZE, TextureFormat::Rgba8UnormSrgb, None);
    target.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = world.resource_mut::<Assets<Image>>().add(target);
    let frames = Arc::new(Mutex::new(Vec::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(target.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").clone_from(&event.data);
    });
    let camera = world
        .spawn((
            Camera3d::default(),
            Camera::default(),
            RenderTarget::Image(target.into()),
            Tonemapping::None,
            DebandDither::Disabled,
            Msaa::Off,
            DepthPrepass,
            AmbientLight::default(),
            eye,
            SceneView::default(),
        ))
        .id();
    (app, frames, camera)
}

fn until_red(app: &mut App, frames: &Mutex<Vec<u8>>) {
    let start = Instant::now();
    let mut settled = 0;
    while settled < 4 {
        app.update();
        settled = if red_pixels(frames) >= 20 && !common::compiling(app) { settled + 1 } else { 0 };
        assert!(start.elapsed() < Duration::from_secs(60), "no liquid was drawn: compiling {}", common::compiling(app));
    }
}

fn showing(actor: impl FnOnce(Entity) -> DrawnActor) -> (App, Arc<Mutex<Vec<u8>>>) {
    let (mut app, frames, _) = rendering(Transform::default(), 0.02);
    let world = app.world_mut();
    let wall = world.resource_mut::<Assets<Mesh>>().add(common::quad(100.0, -60.0, [1.0; 4]));
    let grey = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { base_color: Color::srgb(0.2, 0.2, 0.2), unlit: true, ..Default::default() });
    world.spawn((Mesh3d(wall), MeshMaterial3d(grey), Transform::default()));
    let parent = world.spawn_empty().id();
    world.resource_mut::<DrawnParticles>().actors.push(actor(parent));
    until_red(&mut app, &frames);
    (app, frames)
}

fn pouring(parent: Entity) -> DrawnActor {
    let pour = Pour {
        from: Vector::new(0.0, -40.0, NOZZLE),
        velocity: [Vector::new(0.0, 0.0, -40.0), Vector::new(0.0, 0.0, -40.0)],
        acceleration: Vector::new(0.0, 0.0, -950.0),
        rate: 120.0,
        flowing: true,
    };
    let strip = (0..=8).map(|step| StripPoint { location: Vector::new(0.0, -40.0, NOZZLE - step as f32 * 3.0 * NOZZLE / 12.0), size: 4.0, color: [255; 4] });
    let emitter = DrawnEmitter { strip: strip.collect(), ..DrawnEmitter::default() };
    let liquid = Liquid { kind: FluidType::Puke, role: LiquidRole::Stream, burning: false, pour: Some(pour) };
    DrawnActor { parent, drawn: true, seen: true, place: Some(pour.from), liquid: Some(liquid), emitters: vec![emitter], ..DrawnActor::default() }
}

const NOZZLE: f32 = 12.0;

fn red_rows(frame: &Mutex<Vec<u8>>) -> Vec<usize> {
    let frame = frame.lock().expect("lock");
    let pixels = frame.as_chunks::<4>().0;
    (0..SIZE as usize).filter(|row| pixels.get(row * SIZE as usize + SIZE as usize / 2).is_some_and(|&pixel| is_red(pixel))).collect()
}

#[test]
#[ignore = "needs a GPU"]
fn a_pour_draws_one_unbroken_ribbon_from_its_nozzle_to_its_hit_and_nothing_once_it_stops() {
    let (mut app, frame) = showing(pouring);
    let span = (SIZE as f32 * 0.36) as usize;
    let rows = red_rows(&frame);
    let first = rows.first().copied().unwrap_or_default();
    let last = rows.windows(2).take_while(|pair| pair[1] - pair[0] <= 3).last().map_or(first, |pair| pair[1]);
    assert!(first <= SIZE as usize / 2 - span + 3 && last + 3 >= SIZE as usize / 2 + span, "the ribbon runs unbroken over rows {first} to {last}: {rows:?}");
    let liquid = app.world_mut().resource_mut::<DrawnParticles>().actors[0].liquid.as_mut().and_then(|liquid| liquid.pour.as_mut()).map(|pour| pour.flowing = false);
    assert!(liquid.is_some());
    let stopped = Instant::now();
    while red_pixels(&frame) > 0 {
        app.update();
        assert!(stopped.elapsed() < Duration::from_secs(30), "{} pixels of liquid are still drawn", red_pixels(&frame));
    }
}

fn atlas_texel(atlas: &[u8], x: u32, y: u32) -> [f32; 4] {
    let row = (SLOTS_SIDE * SLOT_SIDE) as usize;
    let at = (y as usize * row + x as usize) * 8;
    std::array::from_fn(|k| half(u16::from_le_bytes([atlas[at + 2 * k], atlas[at + 2 * k + 1]])))
}

fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let fraction = f32::from(bits & 0x3ff);
    sign * match exponent {
        0 => fraction * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
    }
}

fn slot_texel(atlas: &[u8], slot: u32, x: i32, y: i32) -> [f32; 4] {
    let corner = [slot % SLOTS_SIDE * SLOT_SIDE + APRON, slot / SLOTS_SIDE * SLOT_SIDE + APRON];
    atlas_texel(atlas, (corner[0] as i32 + x) as u32, (corner[1] as i32 + y) as u32)
}

fn layer(slope: f32) -> (App, Arc<Mutex<Vec<u8>>>) {
    let mut app = App::new();
    common::headless_renderer(&mut app);
    app.add_plugins((LayerGpuPlugin, ExtractResourcePlugin::<LayerFrame>::default()));
    app.finish();
    app.cleanup();
    let world = app.world_mut();
    let atlas = new_liquid_atlas(&mut world.resource_mut::<Assets<Image>>());
    let frames = Arc::new(Mutex::new(Vec::new()));
    let store = frames.clone();
    world.spawn(Readback::texture(atlas.clone())).observe(move |event: On<ReadbackComplete>| {
        store.lock().expect("lock").clone_from(&event.data);
    });
    let floor: Vec<f32> = (0..TEXELS).map(|i| -slope * (i / SIDE as usize) as f32).collect();
    let floors = vec![(0, Arc::new(floor.clone())), (1, Arc::new(floor))];
    let mut slots = vec![LayerSlot::default(); 4];
    (slots[0].around[4], slots[0].around[5], slots[1].around[3], slots[1].around[4]) = (0, 1, 0, 1);
    (slots[1].atlas_x, slots[1].atlas_y) = (SLOT_SIDE, 0);
    world.insert_resource(LayerFrame { capacity: 4, level: 1, steps: 1, slots, active: vec![0, 1], sources: Vec::new(), cleared: vec![0, 1], floors, atlas: Some(atlas.id()), film_life: FILM_LIFE });
    (app, frames)
}

fn hold(app: &mut App, [x, y]: [f32; 2], radius: f32) {
    let mut frame = app.world_mut().resource_mut::<LayerFrame>();
    let blood = |x: f32| LayerSource { x, y, radius, amount: FILM, kind: f32::from(FluidType::Blood as u8), ..LayerSource::default() };
    frame.sources = vec![blood(x), blood(x - SIDE as f32)];
    (frame.slots[0].first_source, frame.slots[0].sources, frame.slots[1].first_source, frame.slots[1].sources) = (0, 1, 1, 1);
}

fn handed_over(app: &mut App) {
    let mut frame = app.world_mut().resource_mut::<LayerFrame>();
    frame.cleared.clear();
    frame.floors.clear();
}

fn until(app: &mut App, frames: &Mutex<Vec<u8>>, what: &str, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let start = Instant::now();
    loop {
        app.update();
        handed_over(app);
        let atlas = frames.lock().expect("lock").clone();
        if !atlas.is_empty() && done(&atlas) {
            return atlas;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "{what}: compiling {}", common::compiling(app));
    }
}

fn depth(atlas: &[u8], slot: u32, x: i32, y: i32) -> f32 {
    slot_texel(atlas, slot, x, y)[0]
}

#[test]
#[ignore = "needs a GPU"]
fn a_held_pool_is_smooth_across_its_tiles_and_dries_once_let_go() {
    let (mut app, frames) = layer(0.0);
    hold(&mut app, [120.0, 64.0], 20.0);
    let rim = |atlas: &[u8]| depth(atlas, 1, 4, 64);
    let held = |atlas: &[u8]| (depth(atlas, 0, 120, 64) - FILM).abs() < 0.02 && rim(atlas) > 0.02 && rim(atlas) < FILM - 0.02;
    let atlas = until(&mut app, &frames, "the disc never held its film on both tiles", held);
    assert!(depth(&atlas, 0, 20, 20) < 1e-3, "far away is dry: {}", depth(&atlas, 0, 20, 20));
    for y in [50, 64, 78] {
        assert_eq!(slot_texel(&atlas, 0, SIDE as i32, y), slot_texel(&atlas, 1, 0, y), "slot 0's apron is slot 1's first texel at row {y}");
        assert_eq!(slot_texel(&atlas, 1, -1, y), slot_texel(&atlas, 0, SIDE as i32 - 1, y), "slot 1's apron is slot 0's last texel at row {y}");
    }
    assert!(slot_texel(&atlas, 0, 20, 20)[3] < 0.0, "a texel far from liquid is marked for the surfaces to skip");
    assert!(slot_texel(&atlas, 0, 120, 64)[3] >= 0.0, "one in the pool is not");
    let kind = slot_texel(&atlas, 0, 120, 64)[1];
    assert!(kind.floor() == f32::from(FluidType::Blood as u8) && kind.fract() > 0.9, "blood, all of it: {kind}");
    app.world_mut().resource_mut::<LayerFrame>().sources.clear();
    until(&mut app, &frames, "the film never dried once nothing held it", |atlas| depth(atlas, 0, 120, 64) < 1e-3);
}

#[test]
#[ignore = "needs a GPU"]
fn poured_liquid_runs_downhill_and_keeps_its_film_behind() {
    let (mut app, frames) = layer(0.2);
    until(&mut app, &frames, "the layer never published its floor", |atlas| slot_texel(atlas, 0, 64, 30)[2] == -6.0);
    {
        let mut frame = app.world_mut().resource_mut::<LayerFrame>();
        frame.sources = vec![LayerSource { x: 64.0, y: 30.0, radius: 6.0, amount: 600.0, kind: f32::from(FluidType::Urine as u8), adds: 1, seed: 0 }];
        (frame.slots[0].first_source, frame.slots[0].sources) = (0, 1);
    }
    app.update();
    app.world_mut().resource_mut::<LayerFrame>().sources.clear();
    let middle = |atlas: &[u8]| {
        let (mut volume, mut moment) = (0.0, 0.0);
        for (x, y) in (0..SIDE as i32).flat_map(|y| (0..SIDE as i32).map(move |x| (x, y))) {
            volume += depth(atlas, 0, x, y);
            moment += depth(atlas, 0, x, y) * y as f32;
        }
        moment / volume.max(1e-6)
    };
    let atlas = until(&mut app, &frames, "the pour never ran downhill from row 30", |atlas| middle(atlas) > 34.0);
    assert!(depth(&atlas, 0, 64, 30) > 0.1, "where it landed keeps a film: {}", depth(&atlas, 0, 64, 30));
}
