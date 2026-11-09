use super::*;
use super::restyle::{surface_alpha_mode, surface_two_sided};
use super::fog::blend_fog;
use super::zone_culling::{forced_shown, sun_reaches, walk_view};
use bevy::math::EulerRot;
use p2_assets::Model;

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

fn two_zones() -> Model {
    let node = p2_assets::BspNode {
        plane: [0.0, 0.0, 1.0, 0.0],
        node_flags: 0,
        vert_pool: 0,
        surface: 0,
        back: None,
        front: None,
        coplanar: None,
        collision_bound: None,
        render_bound: None,
        zones: [1, 2],
        num_vertices: 0,
        leaves: [0, 1],
        section: None,
        first_vertex: 0,
        light_map: None,
    };
    Model {
        vectors: Vec::new(),
        points: Vec::new(),
        nodes: vec![node],
        surfaces: Vec::new(),
        verts: Vec::new(),
        num_shared_sides: 0,
        zones: Vec::new(),
        polys: 0,
        bounds: Vec::new(),
        leaf_hulls: Vec::new(),
        leaves: [1, 2].map(|zone| p2_assets::Leaf { zone, permeating: 0, volumetric: 0, visible_zones: 0 }).to_vec(),
        lights: Vec::new(),
        root_outside: true,
        linked: false,
        sections: Vec::new(),
        light_maps: Vec::new(),
        light_map_textures: Vec::new(),
    }
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
    let model = two_zones();
    let collision = StaticCollision { hulls: p2_collision::BspHulls::new(&model), model, terrains: Vec::new(), meshes: Vec::new(), mesh_set: p2_collision::MeshSet::default() };
    app.insert_resource(LevelCollision(Some(Arc::new(collision))));
    app.add_systems(Update, follower_zones);
    let follow = FollowActor { actor: ObjectId::from_index(1), scale: [1.0; 3].into(), pre_pivot: [0.0; 3].into(), skeletal: None };
    let root = app.world_mut().spawn(ActorModel(follow.actor)).id();
    let aabb = Aabb::from_min_max(Vec3::splat(-10.0), Vec3::splat(10.0));
    let section = app.world_mut().spawn((follow, Transform::from_xyz(0.0, 50.0, 0.0), aabb, ChildOf(root))).id();
    let zones = |app: &App| app.world().get::<ZoneCulled>(root).map(|zone| zone.zones.clone());
    app.update();
    assert_eq!(zones(&app), Some(vec![2]));
    for (y, expected) in [(5.0, vec![1, 2]), (-50.0, vec![1])] {
        if let Some(mut transform) = app.world_mut().get_mut::<Transform>(section) {
            transform.translation.y = y;
        }
        app.update();
        assert_eq!(zones(&app), Some(expected));
    }
}
