use super::*;

fn light(x: f32, color: f32) -> HardwareLightUniform {
    HardwareLightUniform {
        position: Vec4::new(x, 0.0, 0.0, 500.0),
        direction: Vec3::new(0.0, -1.0, 0.0),
        kind: 0,
        color: Vec4::new(color, color, color, 0.0),
        attenuation: Vec4::new(1.0, 0.0, 0.0, 0.0),
    }
}

fn lighting(ambient: f32, lights: &[HardwareLightUniform]) -> SkinnedLighting {
    let mut out = SkinnedLighting { ambient: Vec3::splat(ambient), count: lights.len() as u32, lights: [HardwareLightUniform::default(); MAX_HARDWARE_LIGHTS] };
    for (slot, light) in out.lights.iter_mut().zip(lights) {
        *slot = *light;
    }
    out
}

#[test]
fn an_actor_is_lit_from_the_frame_on_its_entity() {
    use bevy::ecs::system::RunSystemOnce;
    let mut world = bevy::ecs::world::World::new();
    let environment = LightEnvironment { lights: FxHashMap::default(), zone_ambients: vec![[0.5; 3]; 3], level_brightness: 1.0, suns: Vec::new(), zone_suns: Vec::new(), sun: None, terrain: Vec::new() };
    world.insert_resource(ShownScene { lighting: Some(Arc::new(environment)), ..ShownScene::default() });
    let collision = StaticCollision {
        model: crate::view::two_zone_model(),
        hulls: p2_collision::BspHulls::default(),
        terrains: Vec::new(),
        meshes: Vec::new(),
        mesh_set: p2_collision::MeshSet::default(),
        lamp_casters: Vec::new(),
        lamp_masked: Vec::new(),
        albedos: crate::level_collision::LevelAlbedos::default(),
    };
    world.insert_resource(LevelCollision(Some(Arc::new(collision))));
    world.init_resource::<Time>();
    world.init_resource::<Assets<Mesh>>();
    world.init_resource::<Assets<StandardMaterial>>();
    world.init_resource::<Assets<MeshLitMaterial>>();
    world.init_resource::<Assets<ActorLitMaterial>>();
    let place = ActorPlace { location: [0.0, 0.0, 10.0].into(), rotation: [0; 3].into(), glow: 0 };
    let lit = || ActorLit::new(ObjectId::from_index(1), 4, 10.0, Shading::Tint);
    let placed = world.spawn((lit(), ActorFrame { place: Some(place), base: None }, Mesh3d::default())).id();
    let unplaced = world.spawn((lit(), Mesh3d::default())).id();
    world.run_system_once(light_actors).expect("the light system runs");
    assert!(world.get::<MeshMaterial3d<ActorLitMaterial>>(placed).is_some());
    assert!(world.get::<MeshMaterial3d<ActorLitMaterial>>(unplaced).is_none());
    assert_eq!(world.get::<ActorLightCache>(placed).and_then(|cache| cache.lit), Some(place));
    assert_eq!(world.get::<ActorLightCache>(unplaced).and_then(|cache| cache.lit), None);
}

#[test]
fn identical_lighting_unchanged() {
    let shown = lighting(0.2, &[light(0.0, 0.5), light(200.0, 0.3)]);
    assert_eq!(blend_lighting(&shown, &shown, 0.1), shown);
}

#[test]
fn blend_converges_to_target() {
    let (mut shown, target) = (lighting(0.1, &[light(0.0, 0.2)]), lighting(0.6, &[light(3.0, 0.9)]));
    let next = blend_lighting(&shown, &target, 0.5);
    assert!((next.ambient.x - 0.35).abs() < 1e-6);
    assert_eq!(next.count, 1);
    for _ in 0..200 {
        shown = blend_lighting(&shown, &target, 0.1);
    }
    assert_eq!(shown, target);
}

#[test]
fn new_light_fades_in() {
    let (shown, target) = (lighting(0.2, &[]), lighting(0.2, &[light(100.0, 1.0)]));
    let next = blend_lighting(&shown, &target, 0.25);
    assert_eq!(next.count, 1);
    assert!((next.lights[0].color.x - 0.25).abs() < 1e-6);
    assert_eq!(next.lights[0].position, target.lights[0].position);
}

#[test]
fn dropped_light_fades_out() {
    let (shown, target) = (lighting(0.2, &[light(0.0, 1.0)]), lighting(0.2, &[light(100.0, 1.0)]));
    let mut next = blend_lighting(&shown, &target, 0.25);
    assert_eq!(next.count, 2);
    assert!((next.lights[0].color.x - 0.25).abs() < 1e-6);
    assert!((next.lights[1].color.x - 0.75).abs() < 1e-6);
    assert_eq!(next.lights[1].position, shown.lights[0].position);
    for _ in 0..100 {
        next = blend_lighting(&next, &target, 0.25);
    }
    assert_eq!(next, target);
}

#[test]
fn fade_without_light_starts_at_target() {
    let target = lighting(0.4, &[light(0.0, 0.5)]);
    assert_eq!(fade_step(None, None, target, 0.1), LightFade { shown: target, arrived: true, write: false });
}

#[test]
fn fade_at_target_writes_nothing() {
    let target = lighting(0.4, &[light(0.0, 0.5)]);
    assert_eq!(fade_step(Some(target), Some(target), target, 0.1), LightFade { shown: target, arrived: true, write: false });
}

#[test]
fn fade_arrival_writes_an_unseen_difference() {
    let (target, drawn) = (lighting(0.4, &[light(0.0, 0.5)]), lighting(0.4 + 0.5 / 255.0, &[light(0.0, 0.5)]));
    assert_eq!(fade_step(Some(drawn), Some(target), target, 0.1), LightFade { shown: target, arrived: true, write: true });
}

#[test]
fn fade_skips_an_unseen_step() {
    let (start, target) = (lighting(0.2, &[light(0.0, 0.5)]), lighting(0.8, &[light(0.0, 0.5)]));
    let fade = fade_step(Some(start), Some(start), target, 0.001);
    assert!(!fade.arrived && !fade.write);
    assert!((fade.shown.ambient.x - 0.2006).abs() < 1e-6);
}

#[test]
fn fade_writes_a_visible_step() {
    let (start, target) = (lighting(0.2, &[light(0.0, 0.5)]), lighting(0.8, &[light(0.0, 0.5)]));
    let fade = fade_step(Some(start), Some(start), target, 0.5);
    assert!(!fade.arrived && fade.write);
    assert!((fade.shown.ambient.x - 0.5).abs() < 1e-6);
}

#[test]
fn fade_arrives_in_bounded_steps() {
    let target = lighting(0.8, &[light(3.0, 0.9)]);
    let (mut drawn, mut shown) = (Some(lighting(0.2, &[light(0.0, 0.2)])), None);
    let step = settle_step(1.0 / 60.0, LIGHT_SETTLE_SECONDS);
    let arrived_after = (1..=300).find(|_| {
        let fade = fade_step(drawn, shown, target, step);
        shown = Some(fade.shown);
        if fade.write {
            drawn = Some(fade.shown);
        }
        fade.arrived
    });
    assert!(arrived_after.is_some());
    assert_eq!(drawn, Some(target));
    assert_eq!(fade_step(drawn, shown, target, step), LightFade { shown: target, arrived: true, write: false });
}

fn map_light(effect: LightEffect, brightness: f32, x: f32) -> MeshLight {
    MeshLight { light_type: p2_render_cpu::LightType::Steady, effect, brightness, location: Vector::from([x, 0.0, 0.0]), ..MeshLight::default() }
}

#[test]
fn the_brightest_sun_is_picked_and_none_without_sun() {
    let lights = [map_light(LightEffect::None, 255.0, 1.0), map_light(LightEffect::Sunlight, 64.0, 2.0), map_light(LightEffect::Sunlight, 200.0, 3.0), map_light(LightEffect::Sunlight, 100.0, 4.0)];
    let (picked, sun) = brightest_sun(&lights, 1.0).expect("a sun");
    assert_eq!(<[f32; 3]>::from(picked.location), [3.0, 0.0, 0.0]);
    assert_eq!(sun.kind, HardwareKind::Directional);
    let baked = picked.color(1.0);
    assert!((sun.color[0] - 0.875 * baked[0]).abs() < 1e-6);
    assert!(brightest_sun(&lights[..1], 1.0).is_none());
    assert!(brightest_sun(&[] as &[MeshLight], 1.0).is_none());
}

fn open_level(meshes: Vec<p2_collision::MeshCollision>) -> StaticCollision {
    StaticCollision {
        model: Model::default(),
        hulls: p2_collision::BspHulls::default(),
        terrains: Vec::new(),
        mesh_set: p2_collision::MeshSet::new(&meshes),
        meshes,
        lamp_casters: Vec::new(),
        lamp_masked: Vec::new(),
        albedos: crate::level_collision::LevelAlbedos::default(),
    }
}

fn wall_at(x: f32) -> p2_collision::MeshCollision {
    let [a, b, c, d] = [[x, -1000.0, -1000.0], [x, 1000.0, -1000.0], [x, 1000.0, 1000.0], [x, -1000.0, 1000.0]];
    p2_collision::MeshCollision::placed([[a, b, c], [a, c, d], [c, b, a], [d, c, a]], Vector::ZERO, Rotator::ZERO, Vector::new(1.0, 1.0, 1.0), Vector::ZERO)
}

fn floor_at(z: f32) -> p2_collision::MeshCollision {
    let [a, b, c, d] = [[-1000.0, -1000.0, z], [1000.0, -1000.0, z], [1000.0, 1000.0, z], [-1000.0, 1000.0, z]];
    p2_collision::MeshCollision::placed([[a, b, c], [a, c, d], [c, b, a], [d, c, a]], Vector::ZERO, Rotator::ZERO, Vector::new(1.0, 1.0, 1.0), Vector::ZERO)
}

#[test]
fn a_wall_before_the_weapon_leaves_it_less_room() {
    let reach = weapon_reach(&open_level(vec![wall_at(30.0)]), Vector::ZERO, &[Vector::new(40.0, 0.0, 0.0)]);
    assert!((reach.free - 22.0).abs() < 0.5, "{reach:?}");
    assert!((reach.reach - reach.free - 10.0).abs() < 0.5, "{reach:?}");
}

#[test]
fn the_light_at_the_eye_ramps_up_from_nothing_as_the_weapon_touches_a_wall() {
    let level = open_level(vec![wall_at(30.0)]);
    let touching = weapon_reach(&level, Vector::ZERO, &[Vector::new(30.5, 0.0, 0.0)]);
    assert!((touching.reach - touching.free - 0.5).abs() < 0.25, "{touching:?}");
    assert_eq!(weapon_reach(&level, Vector::ZERO, &[Vector::new(29.5, 0.0, 0.0)]), WeaponReach { reach: 0.0, free: f32::MAX });
}

#[test]
fn a_wall_only_one_side_of_the_weapon_passes_counts_by_that_side_alone() {
    let side = Vector::new(10.0, 40.0, 0.0);
    let reach = weapon_reach(&open_level(vec![wall_at(8.0)]), Vector::ZERO, &[Vector::new(0.0, 40.0, 0.0), side]);
    let span = side.dot(side).sqrt();
    assert!((reach.free - (span * 0.8 - 8.0)).abs() < 0.5, "{reach:?}");
    assert!((reach.reach - reach.free - span * 0.2).abs() < 0.5, "{reach:?}");
}

#[test]
fn an_open_level_leaves_the_weapon_all_the_room() {
    let reach = weapon_reach(&open_level(Vec::new()), Vector::ZERO, &[Vector::new(40.0, 0.0, 0.0)]);
    assert_eq!(reach, WeaponReach { reach: 0.0, free: f32::MAX });
}

#[test]
fn the_weapon_traces_to_its_own_box_not_to_its_sphere() {
    let points = weapon_points(Some([Vec3::splat(-5.0), Vec3::splat(5.0)]), Vector::new(40.0, 0.0, 0.0), Rotator::ZERO, 20.0);
    assert_eq!(weapon_reach(&open_level(vec![floor_at(-12.0)]), Vector::ZERO, &points).free, f32::MAX);
}

#[test]
fn the_weapons_box_turns_with_it() {
    let quarter = Rotator { pitch: 0, yaw: 16384, roll: 0 };
    let points = weapon_points(Some([Vec3::new(-30.0, -2.0, -2.0), Vec3::new(30.0, 2.0, 2.0)]), Vector::ZERO, quarter, 0.0);
    assert!(points[1..].iter().all(|p| p.x.abs() < 2.5 && p.y.abs() > 27.5), "{points:?}");
}

#[test]
fn the_room_eases_toward_an_open_level_and_arrives() {
    let far = 51.0;
    let (walled, open) = (WeaponReach { reach: 30.0, free: 22.0 }, WeaponReach { reach: 0.0, free: f32::MAX });
    let (half, arrived) = ease_reach(walled, open, far, 0.5);
    assert!(!arrived);
    assert_eq!((half.free, half.reach - half.free), (22.0 + (far - 22.0) * 0.5, 4.0));
    assert_eq!(ease_reach(half, open, far, 1.0), (WeaponReach { reach: far, free: far }, true));
}

#[test]
fn a_first_room_starts_where_it_is() {
    let walled = WeaponReach { reach: 30.0, free: 22.0 };
    assert_eq!(ease_reach(walled, walled, 51.0, 0.1), (walled, true));
}

#[test]
fn the_weapons_soft_edges_stay_wide_and_inside_the_lamp_atlas_guard_band() {
    let (sun, lamp) = (include_str!("../sun_shadow.wesl"), include_str!("../lamp_shadow.wesl"));
    let shader_const = crate::shaders::shader_const;
    assert!((2.5..=3.0).contains(&shader_const(sun, "HELD_EDGE_LEAST_TEXELS")));
    assert!((16.0..=24.0).contains(&shader_const(sun, "HELD_EDGE_TAPS")));
    assert!(shader_const(lamp, "HELD_LAMP_EDGE_TEXELS") + 1.0 <= shader_const(lamp, "ATLAS_GUARD"));
}

#[test]
fn the_weapon_takes_its_glow_as_the_retail_path_does() {
    let environment = Arc::new(LightEnvironment::new(FxHashMap::default(), vec![[0.1, 0.2, 0.3]], 1.0));
    let level = open_level(Vec::new());
    for glow in [0, 40, 255] {
        let lighting = view_model_lighting(&environment, &MapLights::new(&environment), &level, Vec3::ZERO, Vector::ZERO, glow);
        assert_eq!(lighting.ambient, Vec3::from(environment.ambient(0, glow)), "{glow}");
    }
    assert_eq!(environment.ambient(0, 40), actor_ambient([0.1, 0.2, 0.3], 40));
}

#[test]
fn the_mark_follows_the_canvas_below_its_slot() {
    let slot = RenderLayers::layer(OVERLAY_LAYERS[1]);
    let mut below = [false; OVERLAY_LAYERS.len()];
    assert_eq!(overlay_mark(Some(&slot), below), OverlayMark::WorldLit);
    below[1] = true;
    assert_eq!(overlay_mark(Some(&slot), below), OverlayMark::Flat);
    assert_eq!(overlay_mark(Some(&RenderLayers::layer(OVERLAY_LAYERS[0])), below), OverlayMark::WorldLit);
    assert_eq!(overlay_mark(Some(&RenderLayers::layer(0)), below), OverlayMark::Scene);
    assert_eq!(overlay_mark(None, below), OverlayMark::Scene);
}

#[test]
fn the_shader_reads_the_overlay_mark_and_the_poke_span_by_the_same_numbers() {
    let shader = include_str!("../actor_light.wesl");
    assert_eq!(crate::shaders::shader_const(shader, "OVERLAY_WORLD_LIT"), f64::from(OverlayMark::WorldLit as u32));
    assert_eq!(crate::shaders::shader_const(shader, "POKE_SPAN"), 16.0);
}
