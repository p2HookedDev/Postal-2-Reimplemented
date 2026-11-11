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
    let environment = LightEnvironment { lights: FxHashMap::default(), zone_ambients: vec![[0.5; 3]; 3], level_brightness: 1.0, sun: None, terrain: Vec::new() };
    world.insert_resource(ShownScene { lighting: Some(Arc::new(environment)), ..ShownScene::default() });
    let collision = crate::level_collision::StaticCollision {
        model: crate::mesh_view::tests::two_zones(),
        hulls: p2_collision::BspHulls::default(),
        terrains: Vec::new(),
        meshes: Vec::new(),
        mesh_set: p2_collision::MeshSet::default(),
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
