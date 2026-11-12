use bevy::app::{Last, TaskPoolPlugin};
use bevy::asset::{AssetApp, AssetEvent, AssetPlugin, RenderAssetUsages};
use bevy::ecs::entity::EntityHashSet;
use bevy::ecs::prelude::{MessageReader, World};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};

use super::*;
use super::sound::distance_gain;

#[test]
fn gain_is_full_inside_the_radius_then_inverse() {
    assert_eq!(distance_gain(0.0, 200.0), 1.0);
    assert_eq!(distance_gain(200.0, 200.0), 1.0);
    assert!((distance_gain(400.0, 200.0) - 1.0 / 1.9).abs() < 1e-6);
}

#[test]
fn ambient_gain_takes_the_sound_volume_setting_too() {
    assert!((ambient_gain(1.0, 1.0, 0.1) - 0.1).abs() < 1e-6);
    assert_eq!(ambient_gain(1.0, 2.0, 1.0), 1.0);
}

#[test]
fn the_left_channel_ear_sits_on_the_cameras_right() {
    let ears = sound_listener();
    assert_eq!(ears.left_ear_offset, Vec3::X * 2.0);
    assert_eq!(ears.right_ear_offset, Vec3::X * -2.0);
}

#[test]
fn only_the_cameras_draw_never_the_sun_alone() {
    let e = |n| Entity::from_raw_u32(n).unwrap();
    let (mut camera, overlay) = ([e(1), e(3), e(5)], [e(7)]);
    camera.sort_unstable();
    let seen: [&[Entity]; 2] = [&camera, &overlay];
    let shadow_only = EntityHashSet::from_iter([e(20)]);
    assert!(camera_drew(&seen, &shadow_only, e(3), Some(e(10))));
    assert!(camera_drew(&seen, &shadow_only, e(7), None));
    assert!(!camera_drew(&seen, &shadow_only, e(4), Some(e(10))));
    assert!(!camera_drew(&seen, &shadow_only, e(5), Some(e(20))));
    assert!(!camera_drew(&[], &EntityHashSet::default(), e(1), None));
}

#[derive(Resource, Default)]
struct Seen(Vec<usize>);

#[derive(Resource)]
struct Particles(Handle<Mesh>);

fn mesh_changes_per_frame(mut app: App) -> Vec<usize> {
    app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default())).init_asset::<Mesh>().init_resource::<Seen>();
    let mesh = app.world_mut().resource_mut::<Assets<Mesh>>().add(Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default()));
    app.insert_resource(Particles(mesh));
    let rewrite = |world: &mut World| {
        let handle = world.resource::<Particles>().0.clone();
        if let Some(mut mesh) = world.resource_mut::<Assets<Mesh>>().get_mut(&handle) {
            mesh.insert_indices(Indices::U32(vec![0, 1, 2]));
        }
    };
    app.add_systems(PostUpdate, rewrite.in_set(PlaySystems::Present));
    app.add_systems(Last, |mut events: MessageReader<AssetEvent<Mesh>>, mut seen: ResMut<Seen>| {
        seen.0.push(events.read().filter(|event| matches!(event, AssetEvent::Modified { .. })).count());
    });
    app.finish();
    app.cleanup();
    for _ in 0..10 {
        app.world_mut().run_schedule(PostUpdate);
        app.world_mut().run_schedule(Last);
    }
    std::mem::take(&mut app.world_mut().resource_mut::<Seen>().0)
}

#[test]
fn weapon_moved_in_the_pass_is_drawn_there_that_frame() {
    use bevy::transform::components::GlobalTransform;
    let mut app = App::new();
    app.add_plugins((TaskPoolPlugin::default(), bevy::transform::TransformPlugin, PlayPlugin));
    let weapon = app.world_mut().spawn(Transform::default()).id();
    app.add_systems(PostUpdate, (move |mut moved: Query<&mut Transform>| {
        if let Ok(mut transform) = moved.get_mut(weapon) {
            transform.translation.x += 1.0;
        }
    })
    .in_set(PlaySystems::Present));
    app.finish();
    app.cleanup();
    for frame in 1..=5 {
        app.world_mut().run_schedule(PostUpdate);
        let global = app.world().get::<GlobalTransform>(weapon).map(|g| g.translation().x);
        assert_eq!(global, Some(frame as f32));
    }
}

#[test]
fn particle_meshes_reach_the_renderer_in_the_frame_they_change() {
    let mut app = App::new();
    app.add_plugins(PlayPlugin);
    assert_eq!(mesh_changes_per_frame(app), vec![1; 10]);
    let mut app = App::new();
    app.configure_sets(PostUpdate, PlaySystems::Present.after(AssetEventSystems));
    assert_eq!(mesh_changes_per_frame(app)[0], 0);
}

#[test]
fn steps_are_even_over_alternating_frame_times() {
    let mut clock = StepClock::default();
    let (short, long) = (3.0 / 240.0, 4.0 / 240.0);
    let steps: Vec<f32> = (0..60).map(|i| clock.step(if i % 2 == 0 { short } else { long })).collect();
    for &step in &steps[STEP_FRAMES..] {
        assert!((step - (short + long) / 2.0).abs() < 1e-6, "{step}");
    }
    let mut clock = StepClock::default();
    let (mut real, mut game) = (0.0f32, 0.0f32);
    for i in 0..1000 {
        let delta = 0.010 + 0.004 * ((i * 7919) % 13) as f32 / 12.0;
        real += delta;
        game += clock.step(delta);
        assert!((real - game).abs() < STEP_FRAMES as f32 * 0.014, "frame {i}: real {real}, game {game}");
    }
    assert_eq!(StepClock::default().step(1.0), MAX_STEP);
}

#[test]
fn the_look_runs_ahead_only_while_every_tick_turns_alike() {
    let mut look = LookAhead::default();
    let mut rotation = p2_math::Rotator { pitch: 0, yaw: 65000, roll: 0 };
    let view = |rotation, interpolating| p2_vm::PlayerView { actor: None, location: p2_math::Vector::ZERO, rotation, behind: false, interpolating };
    for interpolating in [true, false] {
        look.learn(&view(rotation, interpolating));
        for _ in 0..LOOK_SAMPLES {
            look.in_flight = [10.0, -4.0];
            rotation = p2_math::Rotator { pitch: rotation.pitch - 80, yaw: rotation.yaw + 200, roll: 0 };
            look.learn(&view(rotation, interpolating));
        }
        if interpolating {
            assert_eq!(look.ahead(rotation, rotation, [5.0, 2.0]), rotation, "a cutscene's own turn is not the mouse's");
            rotation = p2_math::Rotator { pitch: 0, yaw: 65000, roll: 0 };
        }
    }
    look.in_flight = [5.0, 0.0];
    let ahead = look.ahead(rotation, rotation, [5.0, 2.0]);
    assert_eq!((shortest_turn(rotation.yaw, ahead.yaw), shortest_turn(rotation.pitch, ahead.pitch)), (200, 40));
    let high = p2_math::Rotator { pitch: 17990, ..rotation };
    assert_eq!(look.ahead(high, high, [0.0, 50.0]).pitch, LOOK_PITCH_LIMITS[1]);

    for _ in 0..2 {
        look.in_flight = [10.0, 0.0];
        rotation.yaw += 900;
        look.learn(&view(rotation, false));
    }
    assert_eq!(look.ahead(rotation, rotation, [5.0, 0.0]).yaw, rotation.yaw);
}

#[test]
fn one_odd_sample_keeps_the_look_ahead_two_stop_it() {
    let samples = |odd: usize| (0..LOOK_SAMPLES).map(|i| if i < odd { 30.0 } else { 10.0 }).collect::<std::collections::VecDeque<f32>>();
    assert_eq!(LookAhead::scale(&samples(1)), 10.0);
    assert_eq!(LookAhead::scale(&samples(2)), 0.0);
}

const FRAME_TIME: f32 = 0.0039;

fn shown_motion(frames: usize, ready: impl Fn(usize) -> f64, step: impl Fn(usize) -> f64) -> Vec<f64> {
    let mix = |a: f64, b: f64, t: f32| a + (b - a) * f64::from(t);
    let (mut clock, mut points, mut tick) = (TickClock::default(), [0.0f64; 3], 0);
    (0..frames)
        .map(|frame| {
            let mut glide = clock.frame(FRAME_TIME);
            if ready(tick) <= frame as f64 * f64::from(FRAME_TIME) {
                points = [glide.pick(points[0], points[1], points[2], mix), points[2], points[2] + step(tick)];
                clock.tick(step(tick) as f32);
                glide = clock.glide();
                tick += 1;
            }
            glide.pick(points[0], points[1], points[2], mix)
        })
        .collect()
}

fn frame_steps(shown: &[f64]) -> Vec<f64> {
    let steps: Vec<f64> = shown.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert!(steps.iter().all(|&step| step >= 0.0), "{steps:?}");
    steps
}

fn mean(steps: &[f64]) -> f64 {
    steps.iter().sum::<f64>() / steps.len() as f64
}

#[test]
fn frame_rounded_ticks_show_as_even_motion() {
    for rate in [120.0, 106.0] {
        let period = 1.0 / rate;
        let steps = frame_steps(&shown_motion(1000, |tick| tick as f64 * period, |_| period));
        let steady = &steps[128..];
        let mean = mean(steady);
        for (frame, step) in steady.iter().enumerate() {
            assert!((step - mean).abs() <= mean * 0.05, "{rate} ticks a second, frame {frame}: {step} against {mean}");
        }
    }
}

#[test]
fn a_late_tick_neither_stops_nor_rushes_the_motion() {
    let period = 1.0 / 120.0;
    let late = |tick: usize| if tick.is_multiple_of(37) { 0.002 } else { 0.0 };
    let steps = frame_steps(&shown_motion(1000, |tick| tick as f64 * period + late(tick), |_| period));
    let steady = &steps[128..];
    let mean = mean(steady);
    for (frame, &step) in steady.iter().enumerate() {
        assert!(step > 0.0 && step <= mean * 1.5, "frame {frame}: {step} against {mean}");
    }
}

#[test]
fn after_a_gap_the_clock_jumps_forward_and_moves_on() {
    let (period, gap, first) = (1.0 / 120.0, 0.3, 200);
    let ready = |tick: usize| tick as f64 * period + if tick >= first { gap } else { 0.0 };
    let steps = frame_steps(&shown_motion(1000, ready, |tick| period + if tick == first { gap } else { 0.0 }));
    let jumps: Vec<usize> = (0..steps.len()).filter(|&frame| steps[frame] > gap / 2.0).collect();
    assert_eq!(jumps.len(), 1, "{steps:?}");
    assert!(steps[jumps[0] + 5..].iter().all(|&step| step > 0.0), "{steps:?}");
}

#[test]
fn with_the_look_ahead_off_a_steady_turn_never_steps_back() {
    let look = LookAhead::default();
    let period = 1.0f32 / 120.0;
    let place = |yaw| (p2_math::Vector { x: 0.0, y: 0.0, z: 0.0 }, p2_math::Rotator { pitch: 0, yaw, roll: 0 });
    let (mut clock, mut points, mut tick) = (TickClock::default(), [place(65000); 3], 0);
    let mut shown = Vec::new();
    for frame in 0..600 {
        let mut glide = clock.frame(FRAME_TIME);
        if tick as f32 * period <= frame as f32 * FRAME_TIME {
            tick += 1;
            points = [glide.pick(points[0], points[1], points[2], glided), points[2], place(65000 + (tick as f32 * 3.0 * 38.4) as i32)];
            clock.tick(period);
            glide = clock.glide();
        }
        let between = glide.pick(points[0], points[1], points[2], glided).1;
        shown.push(look.ahead(points[2].1, between, [0.0; 2]).yaw);
    }
    let turns: Vec<i32> = shown.windows(2).map(|pair| shortest_turn(pair[0], pair[1])).collect();
    assert!(turns.iter().all(|&turn| turn >= 0), "{turns:?}");
    assert!(turns[128..].iter().all(|&turn| turn > 0), "{turns:?}");
}

const LOOP_FRAMES: f32 = 20.0;

fn channel_at(sequence: usize, time: f32) -> ChannelTime {
    ChannelTime { animation: 1, sequence, time, length: LOOP_FRAMES, looping: true }
}

fn shown_channel(frames: usize, at: impl Fn(usize) -> ChannelTime) -> Vec<(f32, usize)> {
    let period = 1.0f32 / 120.0;
    let (mut clock, mut from, mut tick) = (TickClock::default(), None::<ChannelFrom>, 0);
    (0..frames)
        .map(|frame| {
            let mut glide = clock.frame(FRAME_TIME);
            if tick as f32 * period <= frame as f32 * FRAME_TIME {
                if tick > 0 {
                    from = Some(ChannelFrom::new(from.as_ref(), at(tick - 1), glide));
                }
                clock.tick(period);
                glide = clock.glide();
                tick += 1;
            }
            (glided_time(from.as_ref(), at(tick - 1), glide), tick - 1)
        })
        .collect()
}

fn loop_moves(shown: &[(f32, usize)]) -> Vec<f64> {
    shown
        .windows(2)
        .map(|pair| pair[1].0 - pair[0].0)
        .map(|moved| f64::from(if moved < -LOOP_FRAMES * 0.5 { moved + LOOP_FRAMES } else { moved }))
        .collect()
}

fn wraps(shown: &[(f32, usize)]) -> usize {
    shown.windows(2).filter(|pair| pair[1].0 < pair[0].0).count()
}

fn shown_in_tick(shown: &[(f32, usize)], tick: usize) -> Vec<f32> {
    shown.iter().filter(|&&(_, at)| at == tick).map(|&(time, _)| time).collect()
}

#[test]
fn a_loop_moves_evenly_across_its_wrap() {
    let shown = shown_channel(1000, |tick| channel_at(0, (tick as f32 * 0.25) % LOOP_FRAMES));
    assert!(shown.iter().all(|&(time, _)| (0.0..LOOP_FRAMES).contains(&time)), "{shown:?}");
    assert!(wraps(&shown) >= 4, "{shown:?}");
    let moves = loop_moves(&shown);
    let steady = &moves[128..];
    let mean = mean(steady);
    for (frame, &moved) in steady.iter().enumerate() {
        assert!((moved - mean).abs() <= mean * 0.05, "frame {frame}: {moved} against {mean}");
    }
}

#[test]
fn a_wrap_moves_no_more_than_one_tick_in_a_frame() {
    let step = |tick: usize| if tick.is_multiple_of(2) { 0.2 } else { 0.3 };
    let shown = shown_channel(1000, |tick| channel_at(0, (0..tick).map(step).sum::<f32>() % LOOP_FRAMES));
    assert!(wraps(&shown) >= 4, "{shown:?}");
    let moves = loop_moves(&shown);
    assert!(moves.iter().all(|&moved| (-1e-4..=0.3 + 1e-4).contains(&moved)), "{moves:?}");
}

#[test]
fn a_channel_that_drops_back_without_a_wrap_shows_the_tick_at_once() {
    let time = |tick: usize| match tick {
        0..90 => (tick as f32 * 0.25).min(19.0),
        90..160 => (tick - 90) as f32 * 0.25,
        _ => (tick - 160) as f32 * 0.25,
    };
    let shown = shown_channel(600, |tick| channel_at(0, time(tick)));
    for restart in [90, 160] {
        let times = shown_in_tick(&shown, restart);
        assert!(!times.is_empty() && times.iter().all(|&time| time == 0.0), "tick {restart}: {times:?}");
    }
}

#[test]
fn an_animation_played_once_and_started_again_shows_its_start_at_once() {
    let shown = shown_channel(600, |tick| ChannelTime { looping: false, ..channel_at(0, (tick as f32 * 0.25) % LOOP_FRAMES) });
    let times = shown_in_tick(&shown, 80);
    assert!(!times.is_empty() && times.iter().all(|&time| time == 0.0), "{times:?}");
}

#[test]
fn a_new_sequence_shows_at_once() {
    let at = |tick: usize| if tick < 80 { channel_at(0, (tick as f32 * 0.25) % LOOP_FRAMES) } else { channel_at(1, (tick - 80) as f32 * 0.25) };
    let times = shown_in_tick(&shown_channel(400, at), 80);
    assert!(!times.is_empty() && times.iter().all(|&time| time == 0.0), "{times:?}");
}

#[test]
fn a_backward_play_shows_each_tick_at_once() {
    let time = |tick: usize| 19.0 - tick as f32 * 0.1;
    let shown = shown_channel(400, |tick| channel_at(0, time(tick)));
    assert!(shown.iter().all(|&(at, tick)| at == time(tick)), "{shown:?}");
}

#[test]
fn a_glide_turns_the_short_way_round() {
    let rotator = |yaw| p2_math::Rotator { pitch: 0, yaw, roll: 0 };
    let (from, to) = (p2_math::Vector { x: 0.0, y: 0.0, z: 0.0 }, p2_math::Vector { x: 8.0, y: 0.0, z: -4.0 });
    let (location, rotation) = glided((from, rotator(65000)), (to, rotator(65536 + 464)), 0.25);
    assert_eq!((location, shortest_turn(65000, rotation.yaw)), (p2_math::Vector { x: 2.0, y: 0.0, z: -1.0 }, 250));
    assert_eq!(glided((from, rotator(65000)), (to, rotator(464)), 1.0).0, to);
}

#[test]
fn glided_bones_keep_their_length_while_they_turn() {
    use p2_assets::pose::Frame;
    let quarter = Frame { axes: [[0.0, 2.0, 0.0], [-2.0, 0.0, 0.0], [0.0, 0.0, 2.0]], origin: [4.0, 0.0, 0.0] };
    let from = vec![Frame { axes: [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]], origin: [0.0; 3] }];
    let half = glided_bones(&from, &[quarter], 0.5);
    let axis = half[0].axes[0];
    assert!((axis[0] - 2f32.sqrt()).abs() < 1e-5 && (axis[1] - 2f32.sqrt()).abs() < 1e-5, "{axis:?}");
    assert_eq!(half[0].origin, [2.0, 0.0, 0.0]);
    assert_eq!(glided_bones(&[], &[quarter], 0.5), vec![quarter]);
}

#[test]
fn the_vm_can_move_to_its_thread() {
    fn send<T: Send>() {}
    send::<Simulation>();
    send::<Job>();
    send::<VmFrame>();
}

#[test]
fn input_stays_where_it_was_read_while_no_map_is_played() {
    use bevy::ecs::system::RunSystemOnce;
    let mut world = World::new();
    world.init_resource::<Time>();
    world.init_resource::<Play>();
    world.insert_resource(PlayerInput { typed: "a".into(), mouse: [3.0, 4.0], ..Default::default() });
    world.run_system_once(gather_input).expect("the input system runs");
    assert_eq!(world.resource::<PlayerInput>().typed, "a");
    assert_eq!(world.resource::<Play>().backlog.mouse, [0.0; 2]);
    assert!(world.resource::<Play>().backlog.typed.is_empty());
}
