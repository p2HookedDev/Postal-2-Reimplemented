use super::*;
use super::restyle::{surface_alpha_mode, surface_two_sided};
use super::fog::blend_fog;
use super::zone_culling::{CastsUnseen, attachments_with_their_base, forced_shown, move_draws, sun_reaches, walk_view};
use bevy::math::EulerRot;
use p2_assets::{Blending, RenderState};
use bevy::mesh::Mesh;
use crate::scene::MaterialImages;
use crate::scene::ModelView;
use crate::level_collision::{LevelAlbedos, StaticCollision};

fn drawn_as(state: RenderState) -> SectionView {
    SectionView::textured(Handle::default(), Some(MaterialImages { image: Handle::default(), glow: None, detail: None, specular: None, state }))
}

#[test]
fn switched_material_takes_scrolling_blending_and_sides() {
    let mut world = bevy::ecs::world::World::new();
    let image: Handle<Image> = Handle::default();
    let mut store = Assets::<StandardMaterial>::default();
    let material = store.add(StandardMaterial { base_color_texture: Some(image.clone()), ..Default::default() });
    world.insert_resource(store);
    world.init_resource::<AnimatedMaterials>();
    let still = RenderState::default();
    let panned = RenderState {
        blending: Some(Blending::Additive),
        two_sided: true,
        uv: vec![UvModifier::Panner { direction: [1.0, 0.0], rate: 1.0 }],
        ..Default::default()
    };
    let switch = |world: &mut bevy::ecs::world::World, was: &RenderState, now: &RenderState| {
        let look = SwitchedLook { was: drawn_as(was.clone()), now: drawn_as(now.clone()), size: [1.0; 2], specular_size: [1.0; 2] };
        restyle_switched(world, &HashMap::from([(image.id(), look)]));
    };

    switch(&mut world, &still, &panned);
    let drawn = world.resource::<Assets<StandardMaterial>>().get(&material).cloned().unwrap();
    assert_eq!(drawn.alpha_mode, AlphaMode::Add);
    assert!(drawn.double_sided);
    assert_eq!(drawn.cull_mode, None);
    assert_eq!(world.resource::<AnimatedMaterials>().textures.len(), 1);

    switch(&mut world, &panned, &panned);
    assert_eq!(world.resource::<AnimatedMaterials>().textures.len(), 1);
    switch(&mut world, &panned, &still);
    let drawn = world.resource::<Assets<StandardMaterial>>().get(&material).cloned().unwrap();
    assert_eq!(drawn.alpha_mode, AlphaMode::Opaque);
    assert_eq!(drawn.cull_mode, Some(Face::Back));
    assert_eq!(drawn.uv_transform, Affine2::IDENTITY);
    assert!(world.resource::<AnimatedMaterials>().textures.is_empty());
}

#[test]
fn a_surfaces_own_flags_outlast_a_switch() {
    assert_eq!(surface_alpha_mode(AlphaMode::Add, AlphaMode::Opaque, AlphaMode::Blend), AlphaMode::Add);
    assert!(surface_two_sided(true, false, false));
    assert_eq!(surface_alpha_mode(AlphaMode::Opaque, AlphaMode::Opaque, AlphaMode::Add), AlphaMode::Add);
    assert_eq!(surface_alpha_mode(AlphaMode::Premultiplied, AlphaMode::Blend, AlphaMode::Opaque), AlphaMode::Opaque);
    assert!(!surface_two_sided(true, true, false));
    assert!(surface_two_sided(false, false, true));
}

#[test]
fn sun_reaches_by_the_box_nearest_point() {
    let zone = ZoneCulled { zones: vec![1], min: [100.0, -10.0, -10.0], max: [120.0, 10.0, 10.0], only_from: None };
    assert!(sun_reaches([0.0; 3], 100.0, &zone));
    assert!(!sun_reaches([0.0; 3], 99.0, &zone));
    assert!(sun_reaches([110.0, 0.0, 0.0], 0.0, &zone));
}

#[test]
fn fog_snaps_at_full_step_and_blends_in_steps() {
    let red = (LinearRgba::new(1.0, 0.0, 0.0, 1.0), 100.0, 900.0);
    let blue = (LinearRgba::new(0.0, 0.0, 1.0, 1.0), 300.0, 2000.0);
    let none = (LinearRgba::NONE, 0.0, 100.0);
    assert_eq!(blend_fog(red, Some(blue), 1.0), blue);
    assert_eq!(blend_fog(red, None, 1.0), (LinearRgba::NONE, 100.0, 900.0));
    assert_eq!(blend_fog(none, Some(red), 1.0), red);
    let (color, start, end) = blend_fog(red, Some(blue), 0.5);
    assert_eq!((color.red, color.blue, start, end), (0.5, 0.5, 200.0, 1450.0));
    let (color, start, end) = blend_fog(none, Some(red), 0.25);
    assert_eq!((color.red, color.alpha, start, end), (1.0, 0.25, 100.0, 900.0));
    let (color, start, _) = blend_fog(red, None, 0.25);
    assert_eq!((color.red, color.alpha, start), (1.0, 0.75, 100.0));
    let mut shown = red;
    for _ in 0..100 {
        shown = blend_fog(shown, None, 0.2);
    }
    assert_eq!(shown.0, LinearRgba::NONE);
}

#[test]
fn view_rotation_matches_yaw_pitch_and_rolls_about_the_view() {
    let unit = std::f32::consts::TAU / 65536.0;
    for [pitch, yaw] in [[0, 0], [0, 16384], [3000, -12000], [-8000, 40000], [62000, 5000]] {
        let rig = Quat::from_euler(EulerRot::YXZ, -(yaw as f32) * unit - std::f32::consts::FRAC_PI_2, (pitch as i16) as f32 * unit, 0.0);
        let view = view_rotation([pitch, yaw, 0].into());
        assert!(rig.dot(view).abs() > 0.9999, "pitch {pitch} yaw {yaw}: {rig} vs {view}");
    }
    let (level, rolled) = (view_rotation([2000, 9000, 0].into()), view_rotation([2000, 9000, 8192].into()));
    assert!((level * -Vec3::Z).distance(rolled * -Vec3::Z) < 1e-4, "roll keeps the view direction");
    assert!((level * Vec3::Y).dot(rolled * Vec3::Y) < 0.75, "roll tilts up");
}

#[test]
fn the_walk_keeps_a_portal_nearer_than_the_near_plane() {
    let origin = Vec3::new(100.0, 200.0, 300.0);
    let transform = Transform::from_translation(origin);
    let projection = Projection::Perspective(PerspectiveProjection { near: NEAR_CLIPPING_PLANE, far: 500_000.0, ..Default::default() });
    let view = walk_view(&transform, &projection, origin);
    assert_eq!(view.len(), 5);
    let ahead = to_bevy(Vec3::new(0.0, 0.0, -1.0));
    assert!(view.iter().all(|plane| plane.normal.dot(ahead.into()) >= plane.d));
}

#[test]
fn a_forced_zone_tag_shows_only_from_its_zones() {
    assert!(forced_shown(None, Some(3)));
    assert!(forced_shown(Some(&[1, 3]), Some(3)));
    assert!(!forced_shown(Some(&[1, 3]), Some(2)));
    assert!(!forced_shown(Some(&[]), Some(0)));
    assert!(forced_shown(Some(&[1]), None));
}

#[test]
fn a_moving_actor_is_in_the_zones_where_it_is() {
    let mut app = App::new();
    let model = two_zone_model();
    let collision = StaticCollision {
        hulls: p2_collision::BspHulls::new(&model),
        model,
        terrains: Vec::new(),
        meshes: Vec::new(),
        mesh_set: p2_collision::MeshSet::default(),
        lamp_casters: Vec::new(),
        lamp_masked: Vec::new(),
        albedos: LevelAlbedos::default(),
    };
    app.insert_resource(LevelCollision(Some(Arc::new(collision))));
    app.insert_resource(ShownScene { sky_zone: Some(2), ..Default::default() });
    app.add_systems(Update, (follower_zones, layers_by_zone).chain());
    let follow = FollowActor { actor: ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
    let root = app.world_mut().spawn(ActorModel(follow.actor)).id();
    let aabb = Aabb::from_min_max(Vec3::splat(-10.0), Vec3::splat(10.0));
    let section = app.world_mut().spawn((follow, Transform::from_xyz(0.0, 50.0, 0.0), aabb, Mesh3d::default(), ChildOf(root))).id();
    let zones = |app: &App| app.world().get::<ZoneCulled>(root).map(|zone| zone.zones.clone());
    let layers = |app: &App| app.world().get::<RenderLayers>(section).cloned().unwrap_or_default();
    app.update();
    assert_eq!(zones(&app), Some(vec![2]));
    assert_eq!(layers(&app), RenderLayers::layer(SKY_LAYER), "only in the sky zone: the sky camera's");
    for (y, expected, drawn) in [(5.0, vec![1, 2], RenderLayers::layer(0).with(SKY_LAYER)), (-50.0, vec![1], RenderLayers::layer(0))] {
        if let Some(mut transform) = app.world_mut().get_mut::<Transform>(section) {
            transform.translation.y = y;
        }
        app.update();
        assert_eq!(zones(&app), Some(expected));
        assert_eq!(layers(&app), drawn);
    }
}

#[test]
fn an_unseen_caster_leaves_the_view_with_its_children_and_comes_back() {
    let mut app = App::new();
    app.insert_resource(ShownScene::default());
    app.add_systems(Update, layers_by_zone);
    let zone = ZoneCulled { zones: vec![1], min: [0.0; 3], max: [0.0; 3], only_from: None };
    let root = app.world_mut().spawn((zone, LayeredByZone)).id();
    let caster = app.world_mut().spawn((ChildOf(root), RenderLayers::layer(0).with(CASTER_LAYER))).id();
    let plain = app.world_mut().spawn((ChildOf(root), Mesh3d::default())).id();
    let overlay = app.world_mut().spawn((ChildOf(root), RenderLayers::layer(4))).id();
    let layers = |app: &App, entity| app.world().get::<RenderLayers>(entity).cloned().unwrap_or_default();
    move_draws(app.world_mut(), root, 0, UNSEEN_LAYER);
    assert_eq!(layers(&app, caster), RenderLayers::layer(UNSEEN_LAYER).with(CASTER_LAYER));
    assert_eq!(layers(&app, plain), RenderLayers::layer(UNSEEN_LAYER));
    assert_eq!(layers(&app, overlay), RenderLayers::layer(4));
    app.world_mut().entity_mut(root).insert(CastsUnseen);
    let later = app.world_mut().spawn((ChildOf(root), Mesh3d::default())).id();
    app.update();
    assert_eq!(layers(&app, caster), RenderLayers::layer(UNSEEN_LAYER).with(CASTER_LAYER), "its zones changing leaves it unseen");
    assert_eq!(layers(&app, later), RenderLayers::layer(UNSEEN_LAYER), "a draw that joins an unseen actor is unseen");
    move_draws(app.world_mut(), root, UNSEEN_LAYER, 0);
    assert_eq!(layers(&app, caster), RenderLayers::layer(0).with(CASTER_LAYER));
    assert_eq!(layers(&app, plain), RenderLayers::layer(0));
    assert_eq!(layers(&app, later), RenderLayers::layer(0));
}

#[test]
fn an_attachment_leaves_the_view_with_the_model_it_is_on() {
    let mut app = App::new();
    app.add_systems(Update, attachments_with_their_base);
    let id = ObjectId::from_index;
    let one = Vector::new(1.0, 1.0, 1.0);
    let none = Vector::ZERO;
    let turn = Rotator::ZERO;
    let follow = FollowActor { actor: id(1), scale: one, pre_pivot: none, skeletal: None };
    let on = |actor, base| FollowBone {
        actor,
        base,
        bone: 0,
        body: bevy::math::Mat4::IDENTITY,
        base_follow: follow,
        relative_location: none,
        relative_rotation: turn,
        scale: one,
        pre_pivot: none,
        mesh_scale: one,
        origin: none,
        rot_origin: turn,
    };
    let body = app.world_mut().spawn((ActorModel(id(1)), Visibility::Hidden)).id();
    let head = app.world_mut().spawn(on(id(2), id(1))).id();
    let hat = app.world_mut().spawn((on(id(3), id(2)), RenderLayers::layer(0).with(CASTER_LAYER))).id();
    let loose = app.world_mut().spawn(on(id(5), id(4))).id();
    let layers = |app: &App, entity| app.world().get::<RenderLayers>(entity).cloned().unwrap_or_default();
    app.update();
    assert_eq!(layers(&app, head), RenderLayers::layer(UNSEEN_LAYER));
    assert_eq!(layers(&app, hat), RenderLayers::layer(UNSEEN_LAYER).with(CASTER_LAYER), "a hat is on the head, which is on the body");
    assert_eq!(layers(&app, loose), RenderLayers::layer(0), "on an actor with no model: left in view");
    app.world_mut().entity_mut(body).insert(Visibility::Inherited);
    app.update();
    assert_eq!(layers(&app, head), RenderLayers::layer(0));
    assert_eq!(layers(&app, hat), RenderLayers::layer(0).with(CASTER_LAYER));
    app.world_mut().entity_mut(body).insert(CastsUnseen);
    app.update();
    assert_eq!(layers(&app, head), RenderLayers::layer(UNSEEN_LAYER), "the body only casts: so does the head");
}

#[test]
fn draws_under_a_zone_layered_root_follow_its_zones_also_when_they_join_later() {
    let mut app = App::new();
    app.insert_resource(ShownScene { sky_zone: Some(2), ..Default::default() });
    app.add_systems(Update, layers_by_zone);
    let zone = ZoneCulled { zones: vec![2], min: [0.0; 3], max: [0.0; 3], only_from: None };
    let root = app.world_mut().spawn((zone, LayeredByZone)).id();
    let middle = app.world_mut().spawn(ChildOf(root)).id();
    let first = app.world_mut().spawn((ChildOf(middle), Mesh3d::default())).id();
    app.update();
    let later = app.world_mut().spawn((ChildOf(middle), Mesh3d::default())).id();
    let overlay = app.world_mut().spawn((ChildOf(middle), RenderLayers::layer(7))).id();
    app.update();
    let layers = |app: &App, entity| app.world().get::<RenderLayers>(entity).cloned().unwrap_or_default();
    assert_eq!(layers(&app, first), RenderLayers::layer(SKY_LAYER));
    assert_eq!(layers(&app, later), RenderLayers::layer(SKY_LAYER));
    assert_eq!(layers(&app, overlay), RenderLayers::layer(7), "an overlay draw is left alone");
    assert!(app.world().get::<RenderLayers>(middle).is_none(), "what is no mesh, such as a joint, is no draw");
}

#[test]
fn a_shown_scene_is_spawned_and_a_clear_removes_it() {
    let mut app = view_app();
    let model = ModelView {
        sections: vec![SectionView::textured(Handle::default(), None)],
        instances: vec![Transform::default(), Transform::from_xyz(1.0, 0.0, 0.0)],
        zone: None,
        casts: false,
    };
    let scene = Scene {
        models: vec![model],
        center: Vec3::ZERO,
        radius: 1.0,
        sky: None,
        start: None,
        collision: None,
        fogs: Vec::new(),
        decals: Vec::new(),
        actors: Vec::new(),
        attachments: Vec::new(),
        lighting: None,
        light_bakes: None,
        antiportals: Vec::new(),
        light_maps: Vec::new(),
    };
    let counts = |app: &mut App| {
        let world = app.world_mut();
        (world.query::<&MapModel>().iter(world).count(), world.query::<&Mesh3d>().iter(world).count())
    };
    app.update();
    assert_eq!(counts(&mut app), (0, 0));
    app.world_mut().write_message(SceneChange::Clear);
    app.world_mut().write_message(SceneChange::Show(Box::new(scene)));
    app.update();
    assert_eq!(counts(&mut app), (2, 2), "the static root and the attachments root, one draw an instance");
    assert_eq!(app.world().resource::<ShownScene>().radius, 1.0);
    app.world_mut().write_message(SceneChange::Clear);
    app.update();
    assert_eq!(counts(&mut app), (0, 0));
    assert_eq!(app.world().resource::<ShownScene>().radius, 0.0);
}

fn view_app() -> App {
    use bevy::asset::AssetApp;
    let mut app = App::new();
    app.add_plugins((bevy::MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::transform::TransformPlugin, crate::P2RenderPlugins))
        .init_asset::<Mesh>()
        .init_asset::<Image>()
        .init_asset::<StandardMaterial>();
    app
}

#[test]
fn the_main_view_and_an_overlay_camera_take_their_fov_in_the_frame_it_is_written() {
    let mut app = view_app();
    app.add_systems(Update, (|mut facts: ResMut<FrameFacts>| facts.fov = 45.0).before(MeshViewSystems::Camera));
    app.add_systems(PostUpdate, (|mut hud: ResMut<HudView>| hud.overlays = vec![30.0]).before(SceneViewSystems::Overlays));
    app.world_mut().resource_mut::<MeshView>().viewport = Some((UVec2::ZERO, UVec2::new(800, 600)));
    let fovs = |app: &mut App| {
        let world = app.world_mut();
        let fov = |projection: &Projection| match projection {
            Projection::Perspective(perspective) => perspective.fov,
            _ => 0.0,
        };
        let main = world.query_filtered::<&Projection, With<crate::world_pass::WorldCamera>>().iter(world).next().map(fov);
        let overlay = world.query::<(&Projection, &OverlayCamera)>().iter(world).find(|(_, camera)| camera.0 == 0).map(|(projection, _)| fov(projection));
        (main, overlay)
    };
    let vertical = |degrees: f32| Some(crate::modern_view::vertical_fov(degrees, 800.0 / 600.0, false));
    app.update();
    assert_eq!(fovs(&mut app), (vertical(45.0), vertical(30.0)));
}

#[test]
fn fog_written_before_the_camera_is_placed_starts_to_show_in_that_frame() {
    use crate::scene::{ZoneFog, ZoneFogs};
    let zones = vec![None, Some(ZoneFog { color: [1, 2, 3], start: 10.0, end: 20.0 })];
    let volume = ZoneFog { color: [200, 0, 0], start: 30.0, end: 40.0 };
    let mut app = view_app();
    let written = zones.clone();
    app.add_systems(Update, (move |mut facts: ResMut<FrameFacts>, mut fogs: bevy::ecs::message::MessageWriter<ZoneFogs>| {
        facts.volume_fog = Some(volume);
        fogs.write(ZoneFogs(written.clone()));
    })
    .before(MeshViewSystems::Camera));
    app.update();
    assert_eq!(app.world().resource::<ShownScene>().fogs, zones);
    let world = app.world_mut();
    let shown = world.query::<&DistanceFog>().iter(world).next().map(|fog| match fog.falloff {
        FogFalloff::Linear { start, end } => (fog.color.to_linear().with_alpha(1.0), start, end),
        _ => (LinearRgba::NONE, 0.0, 0.0),
    });
    assert_eq!(shown, Some((Color::srgb_u8(200, 0, 0).to_linear(), 30.0, 40.0)), "fog from none starts at its colour and distances");
}

#[test]
fn spawned_sections_of_one_look_share_a_material_until_the_map_is_left() {
    let mut world = bevy::ecs::world::World::new();
    world.init_resource::<Assets<StandardMaterial>>();
    world.init_resource::<Assets<MeshLitMaterial>>();
    world.init_resource::<ActorMaterialCache>();
    let mut state = bevy::ecs::system::SystemState::<ActorMaterials>::new(&mut world);
    let mut materials = state.get_mut(&mut world).expect("the material stores");
    let section = SectionView::textured(Handle::default(), None);
    let first = materials.material(ActorLook::Spawned, &section);
    assert!(first == materials.material(ActorLook::Spawned, &section));
    assert!(first != materials.material(ActorLook::Follower, &section));
    assert!(first != materials.material(ActorLook::Canvas, &section));
    materials.forget_spawned();
    assert!(first != materials.material(ActorLook::Spawned, &section));
}

#[test]
fn a_draws_layers_follow_its_overlay_slot() {
    let mut world = bevy::ecs::world::World::new();
    let draw = world.spawn(OverlaySlot(Some(2))).id();
    assert_eq!(world.get::<RenderLayers>(draw), Some(&RenderLayers::layer(OVERLAY_LAYERS[2])));
    world.entity_mut(draw).insert(OverlaySlot(None));
    assert_eq!(world.get::<RenderLayers>(draw), Some(&RenderLayers::default()));
    let attached = world.spawn(RenderLayers::layer(CASTER_LAYER)).id();
    world.entity_mut(attached).insert(OverlaySlot(Some(0)));
    assert_eq!(world.get::<RenderLayers>(attached), Some(&RenderLayers::layer(OVERLAY_LAYERS[0])));
}
