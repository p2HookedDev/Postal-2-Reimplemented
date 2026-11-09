use super::*;

fn square(size: i32) -> FluidSurface {
    let settings = FluidSettings { hexagonal: false, x_size: size, y_size: size, damping: 0.0, warm_up_time: 0.0, ..FluidSettings::default() };
    FluidSurface::new(settings, Vector::ZERO, Vec::new(), 1)
}

fn pond(settings: FluidSettings) -> FluidSurface {
    FluidSurface::new(FluidSettings { warm_up_time: 0.0, x_size: 24, y_size: 24, ..settings }, Vector::ZERO, Vec::new(), 7)
}

fn peak(surface: &FluidSurface) -> f32 {
    surface.verts[surface.latest].iter().fold(0.0, |peak: f32, h| peak.max(h.abs()))
}

#[test]
fn a_bad_update_rate_ends_the_tick() {
    for update_rate in [-30.0, 0.0] {
        let settings = FluidSettings { update_rate, x_size: 5, y_size: 5, ..FluidSettings::default() };
        let mut surface = FluidSurface::new(settings, Vector::ZERO, Vec::new(), 1);
        surface.pling(Vector::ZERO, 30.0, 0.0);
        surface.tick(1.0, true, &[]);
        assert_eq!(surface.height(2, 2), 0.0);
    }
}

#[test]
fn origin_is_half_the_grid_below_the_location() {
    let surface = FluidSurface::new(FluidSettings::default(), Vector::new(100.0, 200.0, 50.0), Vec::new(), 1);
    assert_eq!(surface.origin, Vector::new(100.0 - 31.0 * 16.0, 200.0 - 31.0 * 16.0 * ROW, 50.0));
}

#[test]
fn a_narrow_pling_spreads_the_same_kick_around_the_nearest_vertex() {
    let mut surface = square(9);
    surface.pling(Vector::new(3.0, -4.0, 99.0), 30.0, 0.0);
    let heights = &surface.verts[surface.latest];
    assert!((heights.iter().sum::<f32>() - 1.0).abs() < 1e-5);
    assert!(heights.iter().all(|&h| h <= surface.height(4, 4)));
    assert!(heights.iter().filter(|&&h| h != 0.0).count() > 1);
}

#[test]
fn a_wide_pling_falls_off_with_distance() {
    let mut surface = square(5);
    surface.pling(Vector::ZERO, 30.0, 96.0);
    assert_eq!(surface.height(2, 2), 1.0);
    assert!((surface.height(3, 2) - 0.942_809).abs() < 1e-5);
    assert_eq!(surface.height(4, 2), 0.0);
}

#[test]
fn a_pling_lands_where_the_vertex_is_drawn_on_odd_rows() {
    let mut surface = FluidSurface::new(FluidSettings { x_size: 9, y_size: 9, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 1);
    for (x, y) in [(4, 3), (3, 3), (4, 4), (3, 4)] {
        let drawn = surface.vertices()[surface.index(x, y)].position;
        assert_eq!(surface.vertex_xy(x, y), (drawn.x, drawn.y));
        surface.verts[surface.latest].fill(0.0);
        surface.pling(drawn, 30.0, 0.0);
        assert!(surface.verts[surface.latest].iter().all(|&h| h <= surface.height(x, y)));
        assert!(surface.height(x, y) > 0.0);
    }
}

#[test]
fn an_edge_takes_no_kick() {
    let mut surface = square(5);
    surface.pling_vertex(0, 2, 30.0);
    surface.pling_vertex(4, 2, 30.0);
    surface.pling_vertex(2, 0, 30.0);
    assert!(surface.verts.iter().flatten().all(|&h| h == 0.0));
}

#[test]
fn one_wave_step_spreads_a_raised_vertex() {
    let mut surface = square(5);
    surface.pling_vertex(2, 2, 30.0);
    let step = 1.0 / 30.0;
    surface.wave_step(step);
    let c2 = 150.0 * step * 150.0 * step;
    let lap = -4.0 / (32.0 * 32.0);
    assert!((surface.height(2, 2) - (2.0 + lap * c2)).abs() < 1e-6);
    assert!((surface.height(2, 1) - c2 / (32.0 * 32.0)).abs() < 1e-6);
    assert_eq!(surface.height(2, 0), 0.0);
    assert_eq!(surface.verts[1 - surface.latest][surface.index(2, 2)], 1.0);
}

#[test]
fn clamped_vertices_hold_still() {
    let mut surface = square(5);
    surface.clamped = vec![1 << 12];
    surface.pling_vertex(2, 2, 30.0);
    surface.pling_vertex(2, 1, 30.0);
    surface.update(1.0 / 30.0, &[]);
    assert_eq!(surface.height(2, 2), 0.0);
}

#[test]
fn simulation_waits_until_drawn_after_warm_up() {
    let mut surface = square(5);
    surface.pling_vertex(2, 2, 30.0);
    surface.tick(1.0, false, &[]);
    assert_eq!(surface.height(2, 2), 1.0);
    surface.tick(0.05, true, &[]);
    assert!(surface.height(2, 2) > 1.5);
}

#[test]
fn buffers_hold_one_vertex_per_grid_point_and_two_triangles_per_cell() {
    for hexagonal in [false, true] {
        let mut surface = FluidSurface::new(FluidSettings { hexagonal, x_size: 4, y_size: 5, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 1);
        assert_eq!(surface.vertices().len(), 20);
        assert_eq!(surface.indices().len(), 3 * (2 * 4 - 2) * 4);
    }
}

#[test]
fn hexagonal_odd_rows_shift_half_a_spacing() {
    let mut surface = FluidSurface::new(FluidSettings { x_size: 3, y_size: 3, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 1);
    let vertices = surface.vertices();
    assert_eq!(vertices[3].position.x - vertices[0].position.x, 16.0);
    assert_eq!(vertices[5].uv[0], 1.0);
}

#[test]
fn the_same_time_gives_the_same_water_at_any_frame_rate() {
    let run = |frames: u32, delta: f32| {
        let mut surface = pond(FluidSettings::default());
        surface.pling(Vector::new(10.0, -20.0, 0.0), -300.0, 0.0);
        for _ in 0..frames {
            surface.tick(delta, true, &[]);
        }
        surface.verts[surface.latest].clone()
    };
    let slow = run(61, 1.0 / 60.0);
    assert_eq!(slow, run(122, 1.0 / 120.0));
    assert_eq!(slow, run(244, 1.0 / 240.0));
}

#[test]
fn a_long_frame_runs_only_the_capped_steps() {
    let mut surface = pond(FluidSettings::default());
    surface.tick(0.0, true, &[]);
    surface.tick(100.0, true, &[]);
    assert!(surface.rollover <= 1.0 / 30.0);
}

#[test]
fn drawn_heights_move_between_the_last_two_steps() {
    let mut surface = pond(FluidSettings::default());
    surface.pling_vertex(12, 12, 300.0);
    surface.tick(1.5 / 30.0, true, &[]);
    let (old, new) = (surface.verts[1 - surface.latest][surface.index(12, 12)], surface.height(12, 12));
    surface.lerp_drawn();
    let drawn = surface.drawn[surface.index(12, 12)];
    assert!(old != new && (drawn - (old + new) / 2.0).abs() < 1e-3, "{old} {new} {drawn}");
}

#[test]
fn a_ripple_spreads_then_decays() {
    let mut surface = pond(FluidSettings::default());
    surface.pling(Vector::ZERO, -300.0, 0.0);
    let kicked = peak(&surface);
    assert!(kicked > 0.0);
    for _ in 0..15 {
        surface.tick(1.0 / 30.0, true, &[]);
    }
    let (near, far) = (surface.height(12, 12), surface.height(18, 12));
    assert!(near != 0.0 && far != 0.0);
    for _ in 0..1800 {
        surface.tick(1.0 / 30.0, true, &[]);
    }
    assert!(peak(&surface) < 0.05, "{}", peak(&surface));
}

#[test]
fn heights_stay_in_bounds_at_any_frame_time() {
    let toucher = FluidToucher { location: Vector::new(-40.0, 30.0, 0.0), velocity: Vector::new(600.0, 0.0, 0.0), radius: 28.0 };
    for delta in [1.0 / 500.0, 1.0 / 60.0, 1.0 / 15.0, 0.5] {
        let settings = FluidSettings { noise_frequency: 200.0, noise_strength: (-128.0, 128.0), damping: 0.1, ..FluidSettings::default() };
        let mut surface = pond(settings);
        surface.oscillators.push(FluidOscillator { location: Vector::ZERO, frequency: 1.0, phase: 0, strength: 32.0, radius: 32.0, time: 0.0 });
        let limit = surface.height_limit;
        for _ in 0..(20.0 / delta) as u32 {
            surface.pling(Vector::new(30.0, 30.0, 0.0), -300.0, 0.0);
            surface.tick(delta, true, &[toucher]);
            assert!(surface.verts.iter().flatten().all(|h| h.is_finite() && h.abs() <= limit * 1.001));
        }
    }
}

#[test]
fn a_wave_speed_too_fast_for_the_grid_stays_stable() {
    for hexagonal in [false, true] {
        let mut surface = pond(FluidSettings { hexagonal, speed: 5000.0, update_rate: 10.0, ..FluidSettings::default() });
        surface.pling(Vector::ZERO, -300.0, 0.0);
        for _ in 0..600 {
            surface.tick(0.1, true, &[]);
        }
        let unbounded = surface.height_limit * 0.5;
        assert!(peak(&surface) < unbounded, "{}", peak(&surface));
    }
}

#[test]
fn one_shot_moves_the_water_a_visible_share_of_a_cell() {
    let mut surface = FluidSurface::new(FluidSettings { warm_up_time: 0.0, ..FluidSettings::default() }, Vector::ZERO, Vec::new(), 7);
    let hit = surface.vertices()[surface.index(16, 16)].position;
    surface.pling(hit, -300.0, 0.0);
    let mut deepest: f32 = 0.0;
    for _ in 0..60 {
        surface.update(1.0 / 30.0, &[]);
        deepest = deepest.max(peak(&surface));
    }
    assert!(deepest > 0.15 * 32.0, "{deepest}");
    assert!(deepest <= surface.height_limit, "{deepest}");
}

#[test]
fn a_noise_rate_under_the_update_rate_still_fires() {
    let mut surface = pond(FluidSettings { noise_frequency: 5.0, ..FluidSettings::default() });
    for _ in 0..30 {
        surface.update(1.0 / 30.0, &[]);
    }
    assert!(peak(&surface) > 0.0);
}
