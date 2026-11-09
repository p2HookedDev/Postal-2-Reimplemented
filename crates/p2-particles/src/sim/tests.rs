use super::*;
use super::draw::{particle_color, size_scale, subdivision};
use crate::props::{CoordinateSystem, DirectionUsage, DrawStyle, EmitterKind};
use crate::props::{RangeVector, RotationSource};
use crate::props::{ColorKey, Range, SizeKey};

fn drawn() -> EmitterProperties {
    EmitterProperties { seconds_before_inactive: 0.0, ..EmitterProperties::default() }
}

#[test]
fn spawn_ends_on_values_that_never_settle() {
    let props = EmitterProperties { initial_time_range: Range::new(5.5, 5.5), lifetime_range: Range::new(1.0, 1.0), max_particles: 1, ..drawn() };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
    emitter.tick(1.0, Vector::ZERO, Rotator::ZERO);
    assert_eq!(emitter.emitters()[0].live_particles(), 1);
}

fn props() -> EmitterProperties {
    EmitterProperties { lifetime_range: Range::new(2.0, 2.0), ..drawn() }
}

fn particle(time: f32, max_lifetime: f32) -> Particle {
    Particle { time, max_lifetime, color_multiplier: [1.0; 3].into(), active: true, subdivision: -1, ..Particle::default() }
}

#[test]
fn script_commands_spawn_cap_and_reinitialize() {
    let quiet = EmitterProperties { max_particles: 4, respawn_dead_particles: false, ..props() };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![quiet.clone()], 1);
    emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
    let before = emitter.emitters()[0].live_particles();
    emitter.spawn_one(0, [5.0, 0.0, 0.0].into());
    assert_eq!(emitter.emitters()[0].live_particles(), before + 1);
    emitter.tick(0.5, [0.0; 3].into(), [0; 3].into());
    emitter.cap_time(0, 0.1);
    assert!(emitter.emitters()[0].particles.iter().filter(|p| p.active).all(|p| p.time <= 0.1));
    emitter.reinitialize(0, EmitterProperties { max_particles: 8, ..quiet });
    assert_eq!(emitter.emitters()[0].particles.len(), 8);
    assert_eq!(emitter.emitters()[0].live_particles(), 0);
}

#[test]
fn size_scale_interpolates_keys() {
    let mut p = props();
    p.use_size_scale = true;
    assert_eq!(size_scale(&p, 1.0, 2.0), 0.5);
    p.use_regular_size_scale = false;
    p.size_scale = vec![SizeKey { time: 0.5, size: 3.0 }, SizeKey { time: 1.0, size: 5.0 }];
    assert!((size_scale(&p, 0.5, 2.0) - 2.0).abs() < 1e-6);
    assert!((size_scale(&p, 1.5, 2.0) - 4.0).abs() < 1e-6);
    p.size_scale_repeats = 1.0;
    assert!((size_scale(&p, 1.25, 2.0) - 2.0).abs() < 1e-6);
}

#[test]
fn color_scale_and_fades() {
    let mut p = props();
    p.use_color_scale = true;
    p.color_scale = vec![ColorKey { time: 0.0, color: [255, 0, 0, 255] }, ColorKey { time: 1.0, color: [0, 0, 255, 0] }];
    assert_eq!(particle_color(&p, &particle(1.0, 2.0)), [127, 0, 127, 255]);
    p.draw_style = DrawStyle::AlphaBlend;
    assert_eq!(particle_color(&p, &particle(1.0, 2.0)), [127, 0, 127, 127]);
    let mut p = props();
    p.fade_out = true;
    p.fade_out_start_time = 1.0;
    assert_eq!(particle_color(&p, &particle(1.5, 2.0)), [127, 127, 127, 127]);
    p.draw_style = DrawStyle::AlphaBlend;
    assert_eq!(particle_color(&p, &particle(1.5, 2.0)), [255, 255, 255, 127]);
    let mut p = props();
    p.fade_in = true;
    p.fade_in_end_time = 1.0;
    assert_eq!(particle_color(&p, &particle(0.25, 2.0)), [63, 63, 63, 63]);
}

#[test]
fn initial_spawning_fills_over_one_lifetime() {
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![props()], 1);
    let mut counts = Vec::new();
    for _ in 0..30 {
        emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        counts.push(emitter.emitters()[0].live_particles());
    }
    assert_eq!(counts[0], 0);
    assert_eq!(counts[1], 1);
    assert!((4..=5).contains(&counts[9]), "{counts:?}");
    assert!((9..=10).contains(&counts[19]), "{counts:?}");
    assert_eq!(counts[29], 10);
}

#[test]
fn rate_spawning_and_dying() {
    let mut p = props();
    p.automatic_initial_spawning = false;
    p.respawn_dead_particles = false;
    p.initial_particles_per_second = 4.0;
    p.particles_per_second = 4.0;
    p.max_particles = 100;
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
    for _ in 0..50 {
        emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
    }
    let live = emitter.emitters()[0].live_particles();
    assert!((8..=9).contains(&live), "{live}");
}

#[test]
fn steady_stream_at_60_hz_is_evenly_spaced_every_frame() {
    let mut p = props();
    p.automatic_initial_spawning = false;
    p.respawn_dead_particles = false;
    p.initial_particles_per_second = 30.0;
    p.particles_per_second = 30.0;
    p.max_particles = 100;
    p.start_velocity_range.x = Range::new(100.0, 100.0);
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
    let spacing = 100.0 / 30.0;
    for frame in 0..180 {
        emitter.tick(1.0 / 60.0, [0.0; 3].into(), [0; 3].into());
        let mut xs: Vec<f32> = emitter.sprites(0, View::default()).map(|s| s.location[0]).collect();
        xs.sort_by(f32::total_cmp);
        if frame > 2 {
            assert!(xs[0] < spacing + 1e-3, "frame {frame}: youngest at {}", xs[0]);
        }
        for pair in xs.windows(2) {
            assert!((pair[1] - pair[0] - spacing).abs() < 1e-2, "frame {frame}: {xs:?}");
        }
    }
}

#[test]
fn particles_move_by_each_frames_own_step() {
    let mut p = props();
    p.automatic_initial_spawning = false;
    p.respawn_dead_particles = false;
    p.initial_particles_per_second = 30.0;
    p.particles_per_second = 30.0;
    p.max_particles = 100;
    p.start_velocity_range.x = Range::new(100.0, 100.0);
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
    let steps = [1.0 / 60.0, 1.0 / 30.0, 0.011, 0.05, 1.0 / 144.0, 0.1, 0.004, 1.0 / 60.0];
    let spacing = 100.0 / 30.0;
    let mut before: Vec<(bool, f32, f32)> = Vec::new();
    for frame in 0..160 {
        let dt = steps[frame % steps.len()];
        emitter.tick(dt, [0.0; 3].into(), [0; 3].into());
        let now: Vec<(bool, f32, f32)> = emitter.emitters()[0].particles.iter().map(|p| (p.active, p.time, p.location[0])).collect();
        for (slot, (&(was, time, x), &(is, new_time, new_x))) in before.iter().zip(&now).enumerate() {
            if was && is && (new_time - time - dt).abs() < 1e-5 {
                assert!((new_x - x - 100.0 * dt).abs() < 1e-3, "frame {frame} slot {slot}: moved {} for step {dt}", new_x - x);
            }
        }
        let mut xs: Vec<f32> = emitter.sprites(0, View::default()).map(|s| s.location[0]).collect();
        xs.sort_by(f32::total_cmp);
        for pair in xs.windows(2) {
            assert!((pair[1] - pair[0] - spacing).abs() < 1e-2, "frame {frame}: {xs:?}");
        }
        before = now;
    }
}

#[test]
fn auto_destroy_after_burst() {
    let mut p = props();
    p.respawn_dead_particles = false;
    p.max_particles = 3;
    let settings = EmitterSettings { auto_destroy: true, ..EmitterSettings::default() };
    let mut emitter = Emitter::new(settings, vec![p], 1);
    let mut ticks = 0;
    while !emitter.destroyed() && ticks < 100 {
        emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        ticks += 1;
    }
    assert!(emitter.destroyed());
    assert!((25..=45).contains(&ticks), "{ticks}");
}

#[test]
fn actor_rotation_applies_before_rotation_offset() {
    let p = EmitterProperties {
        start_location_offset: [10.0, 0.0, 0.0].into(),
        use_rotation_from: RotationSource::Actor,
        rotation_offset: [0x4000, 0, 0].into(),
        ..props()
    };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
    emitter.tick(0.5, [0.0; 3].into(), [0, 0x4000, 0].into());
    let e = &emitter.emitters()[0];
    assert!(e.active_particles > 0);
    for p in e.particles.iter().filter(|p| p.active) {
        let expected = Vector::from([0.0, 10.0, 0.0]);
        assert!((p.location - expected).dot(p.location - expected) < 1e-3, "{:?}", p.location);
    }
}

#[test]
fn independent_particles_spawn_at_owner_and_move() {
    let mut p = props();
    p.start_velocity_range = RangeVector::splat(10.0, 10.0);
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
    emitter.tick(0.5, [100.0, 0.0, 0.0].into(), [0; 3].into());
    emitter.tick(0.5, [100.0, 0.0, 0.0].into(), [0; 3].into());
    for sprite in emitter.sprites(0, View::default()) {
        assert!(sprite.location[0] >= 100.0 && sprite.location[0] <= 110.0, "{sprite:?}");
        assert_eq!(sprite.size, [100.0, 100.0]);
    }
}

fn floor(start: Vector, end: Vector) -> Option<CollisionHit> {
    (start[2] >= 0.0 && end[2] < 0.0).then(|| {
        let t = start[2] / (start[2] - end[2]);
        CollisionHit { location: (start + ((end - start) * t)), normal: [0.0, 0.0, 1.0].into() }
    })
}

#[test]
fn collisions_bounce_damp_and_spawn_in_the_other_emitter() {
    let falling = EmitterProperties {
        acceleration: [0.0, 0.0, -1000.0].into(),
        use_collision: true,
        use_max_collisions: true,
        max_collisions: Range::new(3.0, 3.0),
        spawn_from_other_emitter: Some(1),
        spawn_amount: 1,
        damping_factor_range: RangeVector::splat(0.5, 0.5),
        max_particles: 1,
        respawn_dead_particles: false,
        automatic_initial_spawning: false,
        initial_particles_per_second: 1000.0,
        lifetime_range: Range::new(10.0, 10.0),
        ..drawn()
    };
    let lying = EmitterProperties {
        max_particles: 5,
        respawn_dead_particles: false,
        automatic_initial_spawning: false,
        lifetime_range: Range::new(30.0, 30.0),
        ..drawn()
    };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![falling, lying], 1);
    let owner = Owner { location: [0.0, 0.0, 100.0].into(), ..Owner::default() };
    let mut lowest = f32::MAX;
    let mut bounces = Vec::new();
    for frame in 0..300 {
        emitter.tick_colliding(1.0 / 60.0, owner, &mut floor);
        for p in emitter.emitters()[0].particles.iter().filter(|p| p.active) {
            lowest = lowest.min(p.location[2]);
            if bounces.last().is_none_or(|&(_, hits)| hits != p.hit_count) {
                bounces.push((frame, p.hit_count));
            }
        }
    }
    assert!(lowest >= 0.0, "fell through the floor: {lowest}");
    assert_eq!(bounces.iter().map(|&(_, hits)| hits).collect::<Vec<_>>(), [0, 1, 2], "{bounces:?}");
    assert_eq!(emitter.emitters()[0].live_particles(), 0);
    let lying: Vec<Vector> = emitter.emitters()[1].particles.iter().filter(|p| p.active).map(|p| p.location).collect();
    assert_eq!(lying.len(), 3, "{lying:?}");
    assert!(lying.iter().all(|l| (l[2] - SURFACE_OFFSET).abs() < 1e-3), "{lying:?}");
}

#[test]
fn shatter_glass_is_one_burst() {
    let range = |x: (f32, f32), y: (f32, f32), z: (f32, f32)| RangeVector {
        x: Range::new(x.0, x.1),
        y: Range::new(y.0, y.1),
        z: Range::new(z.0, z.1),
    };
    let falling = EmitterProperties {
        acceleration: [0.0, 0.0, -3000.0].into(),
        use_collision: true,
        use_max_collisions: true,
        max_collisions: Range::new(3.0, 3.0),
        spawn_from_other_emitter: Some(1),
        spawn_amount: 1,
        damping_factor_range: range((0.5, 0.9), (0.5, 0.9), (0.4, 0.7)),
        max_particles: 15,
        respawn_dead_particles: false,
        start_location_range: range((0.0, 0.0), (-100.0, 100.0), (-100.0, 100.0)),
        initial_particles_per_second: 5000.0,
        automatic_initial_spawning: false,
        lifetime_range: Range::new(6.0, 6.0),
        start_velocity_range: range((25.0, 250.0), (-30.0, 30.0), (-100.0, 50.0)),
        ..drawn()
    };
    let lying = EmitterProperties { max_particles: 15, respawn_dead_particles: false, lifetime_range: Range::new(30.0, 30.0), ..drawn() };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![falling, lying], 7);
    let owner = Owner { location: [0.0, 0.0, 100.0].into(), ..Owner::default() };
    let mut falling_spawned = 0;
    let mut last_falling = 0;
    for frame in 0..3600 {
        emitter.tick_colliding(1.0 / 60.0, owner, &mut floor);
        let [falling, lying] = [0, 1].map(|i| emitter.emitters()[i].live_particles());
        assert!(falling <= 15 && lying <= 15, "frame {frame}: {falling} falling, {lying} lying");
        if frame == 0 {
            falling_spawned = falling;
        }
        if falling > 0 {
            last_falling = frame;
        }
    }
    assert_eq!(falling_spawned, 15, "one burst");
    assert!(last_falling < 3 * 60, "falling shards still alive at frame {last_falling}");
    assert_eq!(emitter.emitters()[0].live_particles() + emitter.emitters()[1].live_particles(), 0);
}

#[test]
fn particles_slower_than_min_squared_velocity_come_to_rest() {
    let p = EmitterProperties {
        acceleration: [0.0, 0.0, -1000.0].into(),
        use_collision: true,
        damping_factor_range: RangeVector::splat(0.2, 0.2),
        min_squared_velocity: 2500.0,
        max_particles: 1,
        respawn_dead_particles: false,
        automatic_initial_spawning: false,
        initial_particles_per_second: 1000.0,
        lifetime_range: Range::new(10.0, 10.0),
        ..drawn()
    };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
    let owner = Owner { location: [0.0, 0.0, 50.0].into(), ..Owner::default() };
    for _ in 0..240 {
        emitter.tick_colliding(1.0 / 60.0, owner, &mut floor);
    }
    let particle = emitter.emitters()[0].particles[0];
    assert!(particle.stopped && particle.active, "{particle:?}");
    assert!(particle.location[2] >= 0.0 && particle.location[2] < 0.1, "{particle:?}");
}

#[test]
fn sprite_axes_follow_direction_usage() {
    let view = View { location: [-500.0, 0.0, 0.0].into(), right: [0.0, 1.0, 0.0].into(), up: [0.0, 0.0, 1.0].into() };
    let sized = |usage: DirectionUsage| EmitterProperties {
        use_direction_as: usage,
        start_size_range: RangeVector { x: Range::new(10.0, 10.0), y: Range::new(20.0, 20.0), z: Range::new(1.0, 1.0) },
        start_velocity_range: RangeVector { x: Range::new(0.0, 0.0), y: Range::new(0.0, 0.0), z: Range::new(100.0, 100.0) },
        ..props()
    };
    let axes = |usage: DirectionUsage| {
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![sized(usage)], 1);
        for _ in 0..10 {
            emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
        }
        let sprite = emitter.sprites(0, view).next().expect("a sprite");
        (sprite.up, sprite.right)
    };
    let close = |a: Vector, b: Vector| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3);
    let (up, right) = axes(DirectionUsage::None);
    assert!(close(up, [0.0, 0.0, -10.0].into()) && close(right, [0.0, 10.0, 0.0].into()), "{up:?} {right:?}");
    let (up, right) = axes(DirectionUsage::Up);
    assert!(close(up, [0.0, 0.0, 20.0].into()) && right[2].abs() < 1e-3 && (right.dot(right).sqrt() - 10.0).abs() < 1e-3, "{up:?} {right:?}");
    let (up, right) = axes(DirectionUsage::Normal);
    assert!(up[2].abs() < 1e-6 && right[2].abs() < 1e-6, "{up:?} {right:?}");
    assert!((up.dot(up).sqrt() - 20.0).abs() < 1e-3 && (right.dot(right).sqrt() - 10.0).abs() < 1e-3);
}

#[test]
fn mesh_particles_spin_and_scale_per_axis() {
    let fixed = |v: Vector| RangeVector { x: Range::new(v[0], v[0]), y: Range::new(v[1], v[1]), z: Range::new(v[2], v[2]) };
    let mesh = EmitterProperties {
        kind: EmitterKind::Mesh,
        spin_particles: true,
        start_spin_range: fixed([0.25, 0.0, 0.5].into()),
        start_size_range: fixed([2.0, 3.0, 4.0].into()),
        ..props()
    };
    let first = |props: EmitterProperties, rotation: Rotator| {
        let mut emitter = Emitter::new(EmitterSettings::default(), vec![props], 1);
        for _ in 0..10 {
            emitter.tick(0.1, [0.0; 3].into(), rotation);
        }
        emitter.meshes(0).next().expect("a mesh particle")
    };
    let m = first(mesh.clone(), [0; 3].into());
    let near = |a: i32, b: i32| (a - b).abs() <= 1;
    assert!(near(m.spin[0], 0) && near(m.spin[1], 16384) && near(m.spin[2], 32768), "{:?}", m.spin);
    assert_eq!(m.size, [2.0, 3.0, 4.0].into());
    assert_eq!(m.frame, [0; 3].into());
    let m = first(EmitterProperties { spin_particles: false, uniform_size: true, ..mesh.clone() }, [0; 3].into());
    assert_eq!((m.spin, m.size), (Rotator::ZERO, Vector::new(2.0, 2.0, 2.0)));
    let m = first(EmitterProperties { coordinate_system: CoordinateSystem::Relative, ..mesh }, [0, 16384, 0].into());
    assert_eq!(m.frame, [0, 16384, 0].into());
}

#[test]
fn subdivisions_follow_age() {
    let mut p = props();
    p.texture_u_subdivisions = 2;
    p.texture_v_subdivisions = 2;
    let (uv, blend) = subdivision(&p, &particle(1.0, 2.0));
    assert_eq!(uv, [0.5, 0.0, 1.0, 0.5]);
    assert_eq!(blend, None);
    p.blend_between_subdivisions = true;
    let (_, blend) = subdivision(&p, &particle(1.25, 2.0));
    assert_eq!(blend, Some(([0.5, 0.5, 1.0, 1.0], 127)));
}

#[test]
fn undrawn_emitters_pause_after_seconds_before_inactive() {
    let p = EmitterProperties { seconds_before_inactive: 0.5, max_particles: 100, ..props() };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![EmitterProperties { initial_particles_per_second: 10.0, particles_per_second: 10.0, ..p }], 1);
    for _ in 0..10 {
        emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
    }
    let paused = emitter.emitters()[0].live_particles();
    for _ in 0..10 {
        emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
    }
    assert_eq!(emitter.emitters()[0].live_particles(), paused, "no updates while undrawn");
    emitter.emitters_mut()[0].mark_rendered();
    emitter.tick(0.1, [0.0; 3].into(), [0; 3].into());
    assert!(emitter.emitters()[0].live_particles() > paused, "drawn again: updating");
}

#[test]
fn drag_along_actors_push_particles() {
    let p = EmitterProperties {
        use_actor_forces: true,
        max_particles: 1,
        respawn_dead_particles: false,
        lifetime_range: Range::new(10.0, 10.0),
        initial_particles_per_second: 1000.0,
        automatic_initial_spawning: false,
        ..props()
    };
    let mut emitter = Emitter::new(EmitterSettings::default(), vec![p], 1);
    emitter.tick(0.01, [0.0; 3].into(), [0; 3].into());
    let force = |x: f32| ActorForce { location: [x, 0.0, 0.0].into(), velocity: [100.0, 0.0, 0.0].into(), radius: 100.0, scale: 1.0 };
    let e = &mut emitter.emitters_mut()[0];
    let before = e.particles[0].velocity;
    e.particles[0].mass = 1.0;
    e.handle_actor_force(&force(-500.0), 0.1);
    assert_eq!(e.particles[0].velocity, before, "out of range");
    e.handle_actor_force(&force(-50.0), 0.1);
    assert!((e.particles[0].velocity[0] - before[0] - 2.5).abs() < 1e-3, "{:?}", e.particles[0].velocity);
}
