use bevy::ecs::message::Messages;
use bevy::ecs::system::RunSystemOnce;
use bevy::ecs::world::World;
use bevy::pbr::StandardMaterial;

use super::*;
use crate::display::MODERN_GRAPHICS;
use crate::render::blended::TerrainLayer;
use crate::render::terrain_material::TerrainLayers;

#[test]
fn every_material_kind_follows_the_setting() {
    let mut world = World::new();
    let base = StandardMaterial::default;
    let mut lightmapped = Assets::<LightmappedMaterial>::default();
    let bsp = lightmapped.add(LightmappedMaterial { base: base(), extension: BakedLight::default() });
    let mut meshes = Assets::<MeshLitMaterial>::default();
    let mesh = meshes.add(MeshLitMaterial { base: base(), extension: DynamicLit::default() });
    let mut actors = Assets::<ActorLitMaterial>::default();
    let actor = actors.add(ActorLitMaterial { base: base(), extension: HardwareLighting::default() });
    let mut terrain = Assets::<TerrainMaterial>::default();
    let ground = terrain.add(TerrainMaterial { base: base(), extension: TerrainLayers::default() });
    let mut terrain_layers = Assets::<TerrainLayerMaterial>::default();
    let layer = terrain_layers.add(TerrainLayerMaterial { base: base(), extension: TerrainLayer::default() });
    world.insert_resource(lightmapped);
    world.insert_resource(meshes);
    world.insert_resource(actors);
    world.insert_resource(terrain);
    world.insert_resource(terrain_layers);
    world.init_resource::<Messages<AssetEvent<LightmappedMaterial>>>();
    world.init_resource::<Messages<AssetEvent<MeshLitMaterial>>>();
    world.init_resource::<Messages<AssetEvent<ActorLitMaterial>>>();
    world.init_resource::<Messages<AssetEvent<TerrainMaterial>>>();
    world.init_resource::<Messages<AssetEvent<TerrainLayerMaterial>>>();
    let mut flags = |on: bool| {
        let value = if on { "True" } else { "False" };
        world.insert_resource(Settings::read(|query| if query == MODERN_GRAPHICS { value.to_owned() } else { String::new() }));
        world.run_system_once(follow_flag::<LightmappedMaterial>).unwrap();
        world.run_system_once(follow_flag::<MeshLitMaterial>).unwrap();
        world.run_system_once(follow_flag::<ActorLitMaterial>).unwrap();
        world.run_system_once(follow_flag::<TerrainMaterial>).unwrap();
        world.run_system_once(follow_flag::<TerrainLayerMaterial>).unwrap();
        [
            world.resource::<Assets<LightmappedMaterial>>().get(&bsp).unwrap().extension.layers.detail.z > 0.5,
            world.resource::<Assets<MeshLitMaterial>>().get(&mesh).unwrap().extension.layers.detail.z > 0.5,
            world.resource::<Assets<ActorLitMaterial>>().get(&actor).unwrap().extension.layers.detail.z > 0.5,
            world.resource::<Assets<TerrainMaterial>>().get(&ground).unwrap().extension.params.count.y == 1,
            world.resource::<Assets<TerrainLayerMaterial>>().get(&layer).unwrap().extension.layers.detail.z > 0.5,
        ]
    };
    assert_eq!(flags(true), [true; 5]);
    assert_eq!(flags(false), [false; 5]);
    assert_eq!(flags(true), [true; 5]);
}

#[test]
fn every_material_gets_the_flag_once() {
    let mut world = World::new();
    let mut materials = Assets::<LightmappedMaterial>::default();
    let ids: Vec<_> = (0..3).map(|_| materials.add(LightmappedMaterial { base: StandardMaterial::default(), extension: BakedLight::default() })).collect();
    world.insert_resource(materials);
    world.init_resource::<Messages<AssetEvent<LightmappedMaterial>>>();
    world.insert_resource(Settings::read(|query| if query == MODERN_GRAPHICS { "True".to_owned() } else { String::new() }));
    world.run_system_once(follow_flag::<LightmappedMaterial>).unwrap();
    let bits = modern_flag(world.resource::<Settings>());
    assert!(bits & MODERN_ON != 0);
    let materials = world.resource::<Assets<LightmappedMaterial>>();
    assert!(ids.iter().all(|id| materials.get(id).is_some_and(|material| material.flag() == bits)));
}

#[test]
fn the_modern_flag_holds_each_effect_and_round_trips() {
    let settings = |master: &str, relief: &str, specular: &str| {
        Settings::read(|query| match query {
            MODERN_GRAPHICS => master.to_owned(),
            MODERN_RELIEF => relief.to_owned(),
            MODERN_SPECULAR => specular.to_owned(),
            _ => String::new(),
        })
    };
    assert_eq!(modern_flag(&settings("False", "False", "False")), 0);
    assert_eq!(modern_flag(&settings("True", "", "")), MODERN_ON);
    assert_eq!(modern_flag(&settings("True", "False", "True")), MODERN_ON | NO_RELIEF);
    assert_eq!(modern_flag(&settings("True", "True", "False")), MODERN_ON | NO_GLOSS);
    assert_eq!(modern_flag(&settings("True", "False", "False")), MODERN_ON | NO_RELIEF | NO_GLOSS);
    for bits in 0..8u8 {
        let flag = f32::from(bits);
        assert_eq!(u32::from(bits), (flag.max(0.0) + 0.5) as u32);
    }
}

#[test]
fn view_effects_scale_with_their_levels() {
    assert!((bloom(5).intensity - 0.1).abs() < 1e-6 && (bloom(10).intensity - 0.2).abs() < 1e-6);
    assert_eq!([1, 2].map(|level| sun_contact_shadows(level).linear_steps), [8, 16]);
    assert!(matches!(ssao(1).quality_level, ScreenSpaceAmbientOcclusionQualityLevel::Low));
    assert!(matches!(ssao(2).quality_level, ScreenSpaceAmbientOcclusionQualityLevel::Medium));
    assert!(matches!(ssao(3).quality_level, ScreenSpaceAmbientOcclusionQualityLevel::High));
}

#[test]
fn sun_shadow_quality_follows_its_levels() {
    assert_eq!([1, 2, 3, 4].map(sun_quality), [(1024, 2, 3), (2048, 2, 3), (2048, 3, 4), (4096, 3, 4)]);
    assert_eq!([1, 2, 3, 4, 5].map(shadow_reach), [2048.0, 4096.0, 8192.0, 16384.0, 32768.0]);
    let ultra = sun_cascades(4, 5);
    assert_eq!(ultra.bounds.len(), 3);
    assert!((ultra.bounds[0] - FIRST_CASCADE_FAR).abs() < 1.0 && (ultra.bounds[2] - 32768.0).abs() < 1.0);
    assert_eq!(sun_cascades(1, 1).bounds.len(), 2);
    assert!((sun_cascades(0, 1).overlap_proportion - SUN_OVERLAP).abs() < 1e-6);
}

#[test]
fn ambient_fades_then_lands_on_the_zone() {
    let (dark, bright) = (LinearRgba::rgb(0.0, 0.0, 0.0), LinearRgba::rgb(1.0, 0.5, 0.25));
    assert_eq!(blend_ambient(dark, bright, 0.5), LinearRgba::rgb(0.5, 0.25, 0.125));
    assert_eq!(blend_ambient(dark, bright, 0.0), dark);
    assert_eq!(blend_ambient(dark, bright, 1.0), bright);
    let mut shown = dark;
    for _ in 0..32 {
        shown = blend_ambient(shown, bright, 0.25);
    }
    assert_eq!(shown, bright);
}

fn light(effect: p2_render_cpu::LightEffect, brightness: f32, x: f32) -> MeshLight {
    MeshLight { light_type: p2_render_cpu::LightType(1), effect, brightness, location: Vector::from([x, 0.0, 0.0]), ..MeshLight::default() }
}

#[test]
fn the_brightest_sun_is_picked_and_none_without_sun() {
    let lights = [light(p2_render_cpu::LightEffect(0), 255.0, 1.0), light(LE_SUNLIGHT, 64.0, 2.0), light(LE_SUNLIGHT, 200.0, 3.0), light(LE_SUNLIGHT, 100.0, 4.0)];
    let (picked, sun) = brightest_sun(&lights, 1.0).expect("a sun");
    assert_eq!(<[f32; 3]>::from(picked.location), [3.0, 0.0, 0.0]);
    assert_eq!(sun.kind, p2_render_cpu::HardwareKind::Directional);
    let baked = picked.color(1.0);
    assert!((sun.color[0] - 0.875 * baked[0]).abs() < 1e-6);
    assert!(brightest_sun(&lights[..1], 1.0).is_none());
    assert!(brightest_sun(&[] as &[MeshLight], 1.0).is_none());
}

#[test]
fn casters_carry_the_caster_layers_only_in_the_world() {
    let world = RenderLayers::layer(0);
    let still = RenderLayers::from_layers(&[0, CASTER_LAYER, STATIC_CASTER_LAYER]);
    let moving = RenderLayers::from_layers(&[0, CASTER_LAYER, MOVER_LAYER]);
    assert!(caster_layers(&world, true, false) == still);
    assert!(caster_layers(&world, true, true) == moving);
    assert!(caster_layers(&moving, true, false) == still);
    assert!(caster_layers(&still, false, false) == world);
    let overlay = RenderLayers::layer(OVERLAY_LAYERS[0]);
    assert!(caster_layers(&overlay, true, true) == overlay);
    let sky = RenderLayers::layer(SKY_LAYER);
    assert!(caster_layers(&sky, true, false) == sky);
    let view = RenderLayers::from_layers(&[0, SUN_VIEW_LAYER]);
    assert!(!view.intersects(&RenderLayers::from_layers(&[CASTER_LAYER, MOVER_LAYER, STATIC_CASTER_LAYER])));
    assert!(!RenderLayers::from_layers(&OVERLAY_LAYERS).intersects(&RenderLayers::from_layers(&[SUN_VIEW_LAYER, CASTER_LAYER, MOVER_LAYER, STATIC_CASTER_LAYER])));
}

#[test]
fn see_through_surfaces_do_not_cast() {
    for mode in [AlphaMode::Blend, AlphaMode::Premultiplied, AlphaMode::Add, AlphaMode::Multiply, AlphaMode::Mask(2.0)] {
        assert!(see_through_mode(mode));
    }
    for mode in [AlphaMode::Opaque, AlphaMode::Mask(0.5)] {
        assert!(!see_through_mode(mode));
    }
}

#[test]
fn every_solid_world_mesh_casts_and_movers_stand_apart() {
    let settings = |on: &'static str| Settings::read(move |query| if query == MODERN_GRAPHICS { on.to_owned() } else { String::new() });
    let mut world = World::new();
    world.insert_resource(settings("True"));
    world.init_resource::<SunCacheStale>();
    let follow = FollowActor { actor: p2_vm::ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
    let floor = world.spawn(Mesh3d::default()).id();
    let prop = world.spawn((Mesh3d::default(), follow)).id();
    let settled = world.spawn((Mesh3d::default(), follow, StillProp(true))).id();
    let backdrop = world.spawn((Mesh3d::default(), NoSunCast)).id();
    let weapon = world.spawn((Mesh3d::default(), follow, RenderLayers::layer(OVERLAY_LAYERS[0]))).id();
    let hud = world.spawn((Mesh3d::default(), follow, CanvasDrawn(0))).id();
    let system = world.register_system(sun_casters);
    world.run_system(system).unwrap();
    let on = |world: &World, entity: Entity, layer: usize| world.get::<RenderLayers>(entity).is_some_and(|layers| layers.intersects(&RenderLayers::layer(layer)));
    assert!(on(&world, floor, CASTER_LAYER) && on(&world, floor, STATIC_CASTER_LAYER) && !on(&world, floor, MOVER_LAYER));
    assert!(on(&world, prop, CASTER_LAYER) && on(&world, prop, MOVER_LAYER) && !on(&world, prop, STATIC_CASTER_LAYER));
    assert!(on(&world, settled, STATIC_CASTER_LAYER) && !on(&world, settled, MOVER_LAYER));
    assert!(!on(&world, backdrop, CASTER_LAYER) && !on(&world, weapon, CASTER_LAYER) && !on(&world, hud, CASTER_LAYER));
    world.insert_resource(settings("False"));
    world.run_system(system).unwrap();
    assert!(!on(&world, prop, CASTER_LAYER) && !on(&world, floor, CASTER_LAYER));
}

#[test]
fn suns_on_one_direction_add_up_and_others_are_fills() {
    let down = p2_math::Rotator { pitch: -16384, yaw: 0, roll: 0 };
    let slanted = p2_math::Rotator { pitch: -8192, yaw: 0, roll: 0 };
    let sun = |brightness: f32, rotation, x: f32| MeshLight { rotation, ..light(LE_SUNLIGHT, brightness, x) };
    let lights = [sun(128.0, down, 1.0), sun(64.0, down, 2.0), sun(32.0, down, 3.0), sun(40.0, slanted, 4.0)];
    let environment = LightEnvironment::new(lights.into_iter().enumerate().map(|(key, light)| (key as i32, light)).collect(), Vec::new(), 1.0);
    let alone = linear_light(sun(128.0, down, 1.0).color(1.0));
    let together = linear_light(sun(224.0, down, 1.0).color(1.0));
    let shown = sun_color(&environment);
    assert!(shown[0] > alone[0] * 2.0, "the stacked suns are not in the sun: {shown:?} against one sun {alone:?}");
    assert!((shown[0] - together[0]).abs() < together[0] * 0.02, "{shown:?} against {together:?}");
    assert_eq!(fill_suns(&environment, environment.sun.as_ref()).len(), 1);
}
