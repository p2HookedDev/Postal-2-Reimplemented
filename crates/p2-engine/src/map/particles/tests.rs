use super::*;
use super::sprites::drawn_location;

#[test]
fn scene_targets_queries_do_not_conflict() {
    fn uses_scene_targets(_: SceneTargets) {}
    let mut system = bevy::ecs::system::IntoSystem::into_system(uses_scene_targets);
    bevy::ecs::system::System::initialize(&mut system, &mut bevy::ecs::world::World::new());
}

#[test]
fn sprites_move_evenly_every_frame_between_ticks() {
    let frames = 3;
    let place = |ticks: usize| -> Vector { [10.0 * ticks as f32, 0.0, 0.0].into() };
    let mut drawn = Vec::new();
    for newest in 1..5 {
        for frame in 0..frames {
            let shown = frame as f32 / frames as f32 - 0.25;
            drawn.push(drawn_location(place(newest - 1), place(newest), shown).x);
        }
    }
    assert_eq!(drawn[0], -2.5);
    for pair in drawn.windows(2) {
        assert!((pair[1] - pair[0] - 10.0 / frames as f32).abs() < 1e-3, "{drawn:?}");
    }
}

#[test]
fn newborn_sprites_do_not_move_before_their_birth_place() {
    use p2_particles::{EmitterProperties, EmitterSettings};
    let props = EmitterProperties { max_particles: 1, initial_particles_per_second: 0.0, particles_per_second: 0.0, automatic_initial_spawning: false, ..EmitterProperties::default() };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
    emitter.tick(1.0 / 120.0, [30.0, -5.0, 2.0].into(), [0; 3].into());
    emitter.spawn_one(0, [0.0; 3].into());
    let sprite = emitter.sprites(0, p2_particles::View::default()).next().expect("a sprite");
    assert_eq!(sprite.previous, sprite.location);
    let (birth, moved): (Vector, Vector) = ([30.0, -5.0, 2.0].into(), [34.0, -5.0, 2.0].into());
    for shown in [0.0, 0.25, 0.5, 1.0] {
        assert_eq!(drawn_location(sprite.previous, sprite.location, shown), sprite.location);
        let x = drawn_location(birth, moved, shown).x;
        assert!((30.0..=34.0).contains(&x), "{shown}: {x}");
    }
    assert_eq!(drawn_location(birth, moved, 0.0), birth);
}

#[test]
fn sprites_stand_at_the_time_the_world_is_shown_at() {
    let (previous, newest): (Vector, Vector) = ([0.0; 3].into(), [8.0, 0.0, -4.0].into());
    assert_eq!(drawn_location(previous, newest, 1.0), newest);
    assert_eq!(drawn_location(previous, newest, 0.0), previous);
    assert_eq!(drawn_location(previous, newest, -0.25), Vector::from([-2.0, 0.0, 1.0]));
}

#[test]
fn an_emitter_on_a_bone_is_never_drawn_or_stepped_where_the_game_keeps_its_actor() {
    let kept_at = [5.0, 0.0, 0.0];
    let flash = crate::play::ActorFrame::kept_at(kept_at, Some(ObjectId::from_index(2)));
    let tick = 1.0 / 120.0;
    let unplaced = emitter_turn(None, &flash, tick, 1.0);
    assert!(unplaced.hidden);
    assert_eq!(unplaced.step, None);
    let muzzle = Vector::from([5.0, 0.0, 98.0]);
    let placed = emitter_turn(Some((muzzle, [0; 3].into())), &flash, tick, 1.0);
    assert!(!placed.hidden);
    assert_eq!(placed.step, Some(tick));
    assert_eq!(placed.owner.location, muzzle);
    assert_ne!(placed.owner.location, Vector::from(kept_at));
    assert_eq!(placed.here, Vec3::from(to_bevy(muzzle)));
}

#[test]
fn the_particle_step_never_draws_or_steps_a_flash_on_a_weapon_where_the_game_keeps_it() {
    use bevy::app::{App, TaskPoolPlugin, Update};
    use p2_particles::{EmitterProperties, EmitterSettings};
    let (flash, weapon) = (ObjectId::from_index(1), ObjectId::from_index(2));
    let (kept_at, tick) = ([5.0, 0.0, 0.0], 1.0 / 120.0);
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default());
    app.init_resource::<Hud>().init_resource::<LevelCollision>().init_resource::<FrameTimes>().init_resource::<Assets<Mesh>>();
    app.init_resource::<DrawnParticles>();
    let mut play = Play::default();
    play.set(Some(PlayWorld::with_actors([(flash, crate::play::ActorFrame::kept_at(kept_at, Some(weapon)))])));
    app.insert_resource(play);
    let parent = app.world_mut().spawn(Transform::default()).id();
    app.world_mut().spawn((Transform::default(), SceneView::default()));
    let props = EmitterProperties { max_particles: 1, ..EmitterProperties::default() };
    let simulation = Emitter::new(EmitterSettings::default(), vec![props], 1);
    app.insert_resource(Particles { actors: vec![particle_actor(flash, parent, simulation, Vec::new())], ..Particles::default() });
    app.add_systems(Update, present_particles);
    let frame = |app: &mut App| {
        app.world_mut().resource_mut::<Play>().world_mut().expect("a world").present_step(tick);
        app.update();
        let turn = &app.world().resource::<Particles>().actors[0].turn;
        (turn.hidden, turn.step, turn.owner.location)
    };
    let (hidden, stepped, _) = frame(&mut app);
    assert!(hidden);
    assert_eq!(stepped, None);
    let muzzle = Vector::from([5.0, 0.0, 98.0]);
    let on_bone = crate::play::BoneFrame { location: muzzle, axes: [[1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into(), [0.0, 0.0, 1.0].into()] };
    app.world_mut().resource_mut::<Play>().world_mut().expect("a world").shown.places.put_on_bone(flash, weapon, on_bone, on_bone);
    for _ in 0..3 {
        let (hidden, stepped, at) = frame(&mut app);
        assert!(!hidden);
        assert_eq!(stepped, Some(tick));
        assert_eq!(at, muzzle);
        assert_ne!(at, Vector::from(kept_at));
    }
}

fn particle_actor(actor: ObjectId, parent: Entity, simulation: Emitter, draws: Vec<EmitterDraw>) -> ParticleActor {
    ParticleActor {
        actor,
        simulation,
        parent,
        draws,
        objects: Vec::new(),
        bounds: None,
        buffers: DrawBuffers::default(),
        zone_box: None,
        overlay: None,
        name: String::new(),
        zone_seen: true,
        drawn: true,
        born: std::time::Instant::now(),
        peak: 0,
        frames: ParticleFrames::default(),
        turn: Turn::default(),
        puddle_seed: None,
    }
}

fn sprite_emitter_app() -> bevy::app::App {
    use bevy::app::{App, TaskPoolPlugin, Update};
    use bevy::ecs::schedule::IntoScheduleConfigs;
    use p2_particles::{EmitterProperties, EmitterSettings};
    let actor = ObjectId::from_index(1);
    let mut app = App::new();
    app.add_plugins(TaskPoolPlugin::default());
    app.init_resource::<Hud>().init_resource::<LevelCollision>().init_resource::<FrameTimes>().init_resource::<Assets<Mesh>>();
    app.init_resource::<DrawnParticles>();
    let mut play = Play::default();
    play.set(Some(PlayWorld::with_actors([(actor, crate::play::ActorFrame::kept_at([0.0; 3], None))])));
    app.insert_resource(play);
    let parent = app.world_mut().spawn(Transform::default()).id();
    app.world_mut().spawn((Transform::default(), SceneView::default()));
    let draw = EmitterDraw { disabled: false, mesh_reach: None, pending_model: None, name: String::new() };
    let props = EmitterProperties {
        max_particles: 4,
        automatic_initial_spawning: false,
        initial_particles_per_second: 1000.0,
        particles_per_second: 1000.0,
        acceleration: [0.0, 0.0, -300.0].into(),
        ..EmitterProperties::default()
    };
    let look = EmitterLook { parent, order: 0, image: Handle::default(), props: props.clone(), name: String::new(), fogless: false };
    let simulation = Emitter::new(EmitterSettings::default(), vec![props], 1);
    app.insert_resource(Particles { actors: vec![particle_actor(actor, parent, simulation, vec![draw])], born: vec![look], ..Particles::default() });
    p2_render::particle_draw::init_drawing_resources(&mut app);
    app.insert_resource(p2_render::frame_facts::FrameFacts { in_play: true, ..Default::default() });
    app.add_systems(Update, (present_particles, p2_render::particle_draw::draw_particles).chain());
    app
}

fn step_frame(app: &mut bevy::app::App) {
    app.world_mut().resource_mut::<Play>().world_mut().expect("a world").present_step(1.0 / 120.0);
    app.update();
}

#[test]
fn a_fixed_emitters_sprite_records_keep_their_words() {
    use std::hash::{Hash, Hasher};
    let mut app = sprite_emitter_app();
    let mut key = rustc_hash::FxHasher::default();
    for _ in 0..5 {
        step_frame(&mut app);
        let records = &app.world().resource::<p2_render::particle_draw::ParticleDraws>().records;
        assert_eq!(records.len(), 5, "a header and four sprites");
        records.hash(&mut key);
    }
    assert_eq!(key.finish(), 9_078_378_109_377_746_994);
}

fn lit_sprite_emitter_app() -> bevy::app::App {
    use p2_render::actor_light::LightEnvironment;
    use p2_render_cpu::MeshLight;
    let mut app = sprite_emitter_app();
    app.insert_resource(p2_host::display::Settings::read(|query| if query == p2_host::display::MODERN_GRAPHICS { "True".to_owned() } else { String::new() }));
    let lighting = LightEnvironment {
        lights: FxHashMap::default(),
        zone_ambients: vec![[0.1, 0.2, 0.3]],
        level_brightness: 1.0,
        sun: None,
        suns: Vec::new(),
        zone_suns: Vec::new(),
        terrain: Vec::new(),
    };
    app.insert_resource(p2_render::view::ShownScene { lighting: Some(Arc::new(lighting)), ..Default::default() });
    let glow = MeshLight { location: [0.0, 0.0, 40.0].into(), radius: 3.0, brightness: 255.0, saturation: 255, dynamic: true, ..MeshLight::default() };
    let publish = move |frame: ResMut<p2_render::bsp_light::FrameLights>| p2_render::bsp_light::FrameLights::publish(frame, &[glow]);
    bevy::ecs::system::RunSystemOnce::run_system_once(app.world_mut(), publish).expect("lights published");
    app
}

#[test]
fn a_fixed_emitters_lit_sprite_records_keep_their_words() {
    use std::hash::{Hash, Hasher};
    let mut app = lit_sprite_emitter_app();
    let mut key = rustc_hash::FxHasher::default();
    for _ in 0..5 {
        step_frame(&mut app);
        let records = &app.world().resource::<p2_render::particle_draw::ParticleDraws>().records;
        assert_eq!(records.len(), 5, "a header and four sprites");
        assert_ne!(records[0][0..3], [0; 3], "lit");
        assert_ne!(records[0][4..7], [0; 3], "a script light's share");
        records.hash(&mut key);
    }
    assert_eq!(key.finish(), 9_119_684_805_991_967_389);
}

#[test]
fn the_hand_over_is_refilled_in_place() {
    let mut app = sprite_emitter_app();
    let places = |app: &bevy::app::App| {
        let drawn = app.world().resource::<DrawnParticles>();
        let mut places = vec![(drawn.actors.as_ptr() as usize, drawn.actors.capacity())];
        for slot in &drawn.actors {
            places.push((slot.emitters.as_ptr() as usize, slot.emitters.capacity()));
            for emitter in &slot.emitters {
                assert_eq!(emitter.sprites.len(), 4);
                places.push((emitter.sprites.as_ptr() as usize, emitter.sprites.capacity()));
                places.push((emitter.strip.as_ptr() as usize, emitter.strip.capacity()));
                places.push((emitter.beams.as_ptr() as usize, emitter.beams.capacity()));
            }
        }
        places
    };
    step_frame(&mut app);
    let first = places(&app);
    assert_eq!(first.len(), 5, "one actor with one emitter");
    step_frame(&mut app);
    assert_eq!(places(&app), first);
}
