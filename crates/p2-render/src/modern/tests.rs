use bevy::ecs::message::Messages;
use bevy::ecs::system::RunSystemOnce;
use bevy::ecs::world::World;
use bevy::pbr::StandardMaterial;

use super::*;
use crate::world::view::SKY_LAYER;
use p2_render_cpu::LightEffect;
use p2_host::settings::{MODERN_GRAPHICS, MODERN_REFLECTION_STRENGTH, MODERN_REFLECTIONS};
use crate::lighting::actor_light::LightEnvironment;
use crate::world::blended::TerrainLayer;
use crate::world::terrain_material::TerrainLayers;

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
    world.init_resource::<crate::post::debug_view::DebugView>();
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
            world.resource::<Assets<TerrainMaterial>>().get(&ground).unwrap().extension.params.count.y & u32::from(MODERN_ON) != 0,
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
    world.init_resource::<crate::post::debug_view::DebugView>();
    world.run_system_once(follow_flag::<LightmappedMaterial>).unwrap();
    let bits = modern_flag(world.resource::<Settings>());
    let level = world.resource::<Settings>().material_level();
    assert!(bits & MODERN_ON != 0);
    let materials = world.resource::<Assets<LightmappedMaterial>>();
    assert!(ids.iter().all(|id| materials.get(id).is_some_and(|material| material.flag() & 7 == u16::from(bits))));
    assert!(ids.iter().all(|id| materials.get(id).is_some_and(|material| crate::world::material_class::levels_of(material.flag()).0 == level)));
}

#[test]
fn the_pool_mirror_bit_follows_the_reflection_pass_into_every_key() {
    use crate::gpu::shaders::SkyKey;
    assert_eq!(level_bits(15, 15) & (POOL_MIRROR | 7), 0);
    let settings = |level: &str, strength: &str| {
        Settings::read(|query| match query {
            MODERN_GRAPHICS => "True".to_owned(),
            MODERN_REFLECTIONS => level.to_owned(),
            MODERN_REFLECTION_STRENGTH => strength.to_owned(),
            _ => String::new(),
        })
    };
    assert_eq!([("2", "10"), ("0", "10"), ("2", "0")].map(|(level, strength)| mirror_bit(&settings(level, strength))), [POOL_MIRROR, 0, 0]);
    let flag = f32::from(POOL_MIRROR | u16::from(MODERN_ON));
    let mut layers = BakedLight::default();
    layers.layers.detail.z = flag;
    let mut actor = HardwareLighting::default();
    actor.layers.detail.z = flag;
    let mut ground = TerrainLayers::default();
    ground.params.count.y = u32::from(POOL_MIRROR);
    let mut layer = TerrainLayer::default();
    layer.layers.detail.z = flag;
    assert!([SkyKey::from(&layers), SkyKey::from(&actor), SkyKey::from(&ground), SkyKey::from(&layer)].iter().all(|key| key.mirror));
    assert!(!SkyKey::from(&BakedLight::default()).mirror);
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
    assert_eq!([1, 2, 3, 4].map(sun_quality), [(1024, 3), (2048, 3), (2048, 4), (4096, 4)]);
    assert_eq!([1, 2, 3, 4, 5].map(shadow_reach), [2048.0, 4096.0, 8192.0, 16384.0, 32768.0]);
    let farthest = sun_cascades(5);
    assert_eq!(farthest.bounds.len(), 3);
    assert!((farthest.bounds[0] - FIRST_CASCADE_FAR).abs() < 1.0 && (farthest.bounds[2] - 4096.0).abs() < 1.0);
    assert!((sun_cascades(1).bounds[2] - 2048.0).abs() < 1.0);
    assert!((sun_cascades(1).overlap_proportion - SUN_OVERLAP).abs() < 1e-6);
}

#[test]
fn ambient_fades_then_lands_on_the_zone() {
    let (dark, bright) = (LinearRgba::rgb(0.0, 0.0, 0.0), LinearRgba::rgb(1.0, 0.5, 0.25));
    assert_eq!(blend_light(dark, bright, 0.5), LinearRgba::rgb(0.5, 0.25, 0.125));
    assert_eq!(blend_light(dark, bright, 0.0), dark);
    assert_eq!(blend_light(dark, bright, 1.0), bright);
    let mut shown = dark;
    for _ in 0..32 {
        shown = blend_light(shown, bright, 0.25);
    }
    assert_eq!(shown, bright);
}

fn light(effect: LightEffect, brightness: f32, x: f32) -> MeshLight {
    MeshLight { light_type: p2_render_cpu::LightType::Steady, effect, brightness, location: Vector::from([x, 0.0, 0.0]), ..MeshLight::default() }
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
fn no_caster_lands_on_the_sun_view_layer() {
    let sun_view = RenderLayers::layer(SUN_VIEW_LAYER);
    for layers in [RenderLayers::layer(0), RenderLayers::layer(OVERLAY_LAYERS[0]), RenderLayers::from_layers(&[0, CASTER_LAYER, MOVER_LAYER])] {
        for (casts, mover) in [(false, false), (true, false), (true, true)] {
            assert!(!caster_layers(&layers, casts, mover).intersects(&sun_view));
        }
    }
}

#[test]
fn overlays_see_the_sun_only_under_modern_with_at_most_one_casting_sun() {
    let settings = |on: &'static str| Settings::read(move |query| if query == MODERN_GRAPHICS { on.to_owned() } else { String::new() });
    let mut world = World::new();
    let ambient = AmbientLight { color: Color::LinearRgba(LinearRgba::new(0.1, 0.2, 0.3, 4.0)), brightness: 1.0, affects_lightmapped_meshes: false };
    world.spawn((SceneView::default(), ambient.clone(), ShadowFilteringMethod::Hardware2x2));
    let own = RenderLayers::layer(OVERLAY_LAYERS[0]);
    let overlay = world.spawn((OverlayCamera(0), own.clone(), AmbientLight::default(), ShadowFilteringMethod::Gaussian)).id();
    world.spawn(DirectionalLight { shadow_maps_enabled: true, ..DirectionalLight::default() });
    let layers = |world: &World| world.get::<RenderLayers>(overlay).cloned();
    world.insert_resource(settings("True"));
    world.run_system_once(overlay_light).expect("runs");
    assert_eq!(layers(&world), Some(own.clone().with(SUN_VIEW_LAYER)));
    assert_eq!(world.get::<ShadowFilteringMethod>(overlay).copied(), Some(ShadowFilteringMethod::Hardware2x2));
    assert!(world.get::<AmbientLight>(overlay).is_some_and(|light| light.color == ambient.color && light.brightness == ambient.brightness));
    let second = world.spawn(DirectionalLight { shadow_maps_enabled: true, ..DirectionalLight::default() }).id();
    world.run_system_once(overlay_light).expect("runs");
    assert_eq!(layers(&world), Some(own.clone()));
    if let Some(mut light) = world.get_mut::<DirectionalLight>(second) {
        light.shadow_maps_enabled = false;
    }
    world.run_system_once(overlay_light).expect("runs");
    assert_eq!(layers(&world), Some(own.clone().with(SUN_VIEW_LAYER)));
    for mut light in world.query::<&mut DirectionalLight>().iter_mut(&mut world) {
        light.shadow_maps_enabled = false;
    }
    world.run_system_once(overlay_light).expect("runs");
    assert_eq!(layers(&world), Some(own.clone().with(SUN_VIEW_LAYER)), "an unshadowed sun lights the weapon as it lights the world");
    world.insert_resource(settings("False"));
    world.run_system_once(overlay_light).expect("runs");
    assert_eq!(layers(&world), Some(own));
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
fn a_mover_keeps_its_layer_for_the_lamps_with_the_suns_shadows_off() {
    let settings = |sun: &'static str, lamps: &'static str| {
        Settings::read(move |query| match query {
            MODERN_GRAPHICS => "True".to_owned(),
            MODERN_SHADOWS => sun.to_owned(),
            MODERN_LAMP_SHADOWS => lamps.to_owned(),
            _ => String::new(),
        })
    };
    let mut world = World::new();
    world.insert_resource(settings("0", "3"));
    world.init_resource::<SunCacheStale>();
    let follow = FollowActor { actor: p2_vm::ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
    let prop = world.spawn((Mesh3d::default(), follow)).id();
    let system = world.register_system(sun_casters);
    world.run_system(system).unwrap();
    let moves = |world: &World| world.get::<RenderLayers>(prop).is_some_and(|layers| layers.intersects(&RenderLayers::layer(MOVER_LAYER)));
    assert!(moves(&world));
    world.insert_resource(settings("0", "0"));
    world.run_system(system).unwrap();
    assert!(!moves(&world));
}

#[test]
fn suns_on_one_direction_add_up_and_others_are_fills() {
    let down = p2_math::Rotator { pitch: -16384, yaw: 0, roll: 0 };
    let slanted = p2_math::Rotator { pitch: -8192, yaw: 0, roll: 0 };
    let sun = |brightness: f32, rotation, x: f32| MeshLight { rotation, ..light(LightEffect::Sunlight, brightness, x) };
    let lights = [sun(128.0, down, 1.0), sun(64.0, down, 2.0), sun(32.0, down, 3.0), sun(40.0, slanted, 4.0)];
    let environment = LightEnvironment::new(lights.into_iter().enumerate().map(|(key, light)| (key as i32, light)).collect(), Vec::new(), 1.0);
    let alone = linear_light(sun(128.0, down, 1.0).color(1.0));
    let together = linear_light(sun(224.0, down, 1.0).color(1.0));
    let shown = sun_color(environment.sun_set());
    assert!(shown[0] > alone[0] * 2.0, "the stacked suns are not in the sun: {shown:?} against one sun {alone:?}");
    assert!((shown[0] - together[0]).abs() < together[0] * 0.02, "{shown:?} against {together:?}");
    assert_eq!(fill_suns(environment.sun_set()).len(), 1);
}

#[test]
fn a_copy_of_the_suns_in_another_zone_does_not_add_up() {
    let down = p2_math::Rotator { pitch: -16384, yaw: 0, roll: 0 };
    let sun = |brightness: f32, x: f32| MeshLight { rotation: down, ..light(LightEffect::Sunlight, brightness, x) };
    let one_zone = [sun(128.0, 1.0), sun(64.0, 2.0)];
    let copies = [sun(128.0, 1.0), sun(64.0, 2.0), sun(128.0, 101.0), sun(64.0, 102.0), sun(128.0, 201.0), sun(64.0, 202.0)];
    let environment = |lights: &[MeshLight]| LightEnvironment::new(lights.iter().enumerate().map(|(key, light)| (key as i32, *light)).collect(), Vec::new(), 1.0);
    let zone_of = |at: Vector| (at.x / 100.0) as u8;
    let wanted = sun_color(environment(&one_zone).sun_set());
    assert!(sun_color(environment(&copies).sun_set())[0] > wanted[0] * 4.0);
    assert_eq!(sun_color(environment(&copies).suns_of_one_zone(zone_of).sun_set()), wanted);
}

#[test]
fn a_zone_has_the_set_of_its_own_suns_without_the_special_ones() {
    let down = p2_math::Rotator { pitch: -16384, yaw: 0, roll: 0 };
    let sun = |brightness: f32, x: f32| MeshLight { rotation: down, ..light(LightEffect::Sunlight, brightness, x) };
    let lights = [sun(128.0, 1.0), sun(64.0, 101.0), MeshLight { special_lit: true, ..sun(200.0, 102.0) }, MeshLight { special_lit: true, ..sun(90.0, 201.0) }];
    let environment = LightEnvironment::new(lights.into_iter().enumerate().map(|(key, light)| (key as i32, light)).collect(), Vec::new(), 1.0)
        .suns_of_one_zone(|at| (at.x / 100.0) as u8);
    let color = |zone: u8| environment.zone_sun_set(zone).map(sun_color);
    assert_eq!(color(0), Some(linear_light(sun(128.0, 1.0).color(1.0))));
    assert_eq!(color(1), Some(linear_light(sun(64.0, 101.0).color(1.0))));
    assert_eq!(color(2), None);
    assert_eq!(color(3), None);
}
