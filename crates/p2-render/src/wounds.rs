use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::prelude::{Commands, Component, Entity, Has, Local, Query, Res, ResMut, Resource, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::{ClusteredDecal, SimulationLightSystems};
use bevy::math::{Affine3A, Mat4, Vec3};
use bevy::mesh::skinning::SkinnedMesh;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::transform::TransformSystems;
use bevy::transform::components::{GlobalTransform, Transform};
use p2_bevy_assets::to_bevy;
use p2_vm::ObjectId;
use rustc_hash::FxHashMap;

use crate::components::{FollowActor, MapScoped};
use p2_host::display::{MODERN_DECAL_AGING, MODERN_LIQUIDS, MODERN_WOUNDS, Settings, modern_on, modern_turned_off};
use crate::frame_facts::FrameFacts;
use crate::screen_pass::modern_level;

/// Blood drying in projector tag bits 3..7 (the frame buffer blend keeps bits 0..3); `projector.wesl` reads the
/// same.
const DRY_SHIFT: u32 = 3;
const DRY_STEPS: f32 = 15.0;
const DRYING_SECONDS: f32 = 4.0;
/// A projector (bit 31) aimed at actors (bit 19), with the wound bit 15: it also takes the bodies that refuse the
/// game's projectors, and only the faces turned to the shot.
const WOUND_TAG: u32 = 1 << 31 | 1 << 19 | 1 << 15;
const SPLAT_BLEND: u32 = 0x7 | 0x300 | 0x3fe0_0000;
const WOUNDS_PER_BODY: usize = 8;
const WOUND_SIZE: f32 = 12.0;
const WOUND_GROWTH: f32 = 8.0;
const WOUND_SIZES: u32 = 5;
const BONE_REACH: f32 = 48.0;
const BACK_REACH: f32 = 2.0;

fn dryness(age: f32) -> f32 {
    1.0 - (-age.max(0.0) / DRYING_SECONDS).exp()
}

// Fading in projector tag bits 10..15 (material blend keeps 8..10, bit 15 marks a wound); the shader reads the
// same.
const FADE_SHIFT: u32 = 10;
const FADE_STEPS: f32 = 31.0;

fn fade(life: f32, age: f32) -> f32 {
    if life <= 0.0 {
        return 0.0;
    }
    (1.0 - (life - age)).clamp(0.0, 1.0)
}

fn aged_tag(tag: u32, life: f32, age: f32, blood_age: Option<f32>) -> u32 {
    let fade = (fade(life, age) * FADE_STEPS).round() as u32;
    let dry = blood_age.map_or(0, |age| (dryness(age) * DRY_STEPS).round() as u32);
    tag | fade << FADE_SHIFT | dry << DRY_SHIFT
}

pub struct WoundsPlugin;

#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct WoundsSystems;

impl Plugin for WoundsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wounds>().init_resource::<WoundSplats>().add_systems(
            PostUpdate,
            (
                present_wounds.run_if(modern_on(MODERN_WOUNDS)).in_set(WoundsSystems),
                drop_wounds.run_if(modern_turned_off(MODERN_WOUNDS)).in_set(WoundsSystems),
                age_decals.run_if(modern_on(MODERN_DECAL_AGING)).in_set(WoundsSystems),
                restore_decal_tags.run_if(modern_turned_off(MODERN_DECAL_AGING)).in_set(WoundsSystems),
                mark_blood_decals.in_set(WoundsSystems),
                place_wounds.after(TransformSystems::Propagate).before(SimulationLightSystems::AssignLightsToClusters)),
        );
    }
}

#[derive(Resource, Default)]
pub struct WoundSplats(pub Vec<(Handle<Image>, u32)>);

struct Wound {
    decal: Entity,
    joint: Entity,
    local: Mat4,
    tag: u32,
    age: f32,
}

#[derive(Resource, Default)]
pub struct Wounds {
    bodies: FxHashMap<ObjectId, Vec<Wound>>,
    made: u32,
}

fn add_oldest_out<T>(list: &mut Vec<T>, item: T, age: impl Fn(&T) -> f32) -> Option<T> {
    if list.len() < WOUNDS_PER_BODY {
        list.push(item);
        return None;
    }
    let oldest = list.iter_mut().max_by(|a, b| age(a).total_cmp(&age(b)))?;
    Some(std::mem::replace(oldest, item))
}

fn wound_frame(joints: &[Mat4], hit: Vec3, toward: Vec3, size: f32, roll: f32) -> Option<(usize, Mat4)> {
    let distance = |joint: &Mat4| joint.w_axis.truncate().distance(hit);
    let (index, joint) = joints.iter().enumerate().min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))?;
    if distance(joint) > BONE_REACH {
        return None;
    }
    let depth = (joint.w_axis.truncate() - hit).dot(toward).max(size / 2.0) + BACK_REACH;
    let (side, lift) = toward.any_orthonormal_pair();
    let (sin, cos) = roll.sin_cos();
    let (across, up) = (side * cos + lift * sin, lift * cos - side * sin);
    let centre = hit + toward * (depth / 2.0 - BACK_REACH);
    let projector = Mat4::from_cols((across * size).extend(0.0), (up * size).extend(0.0), (-toward * depth).extend(0.0), centre.extend(1.0));
    Some((index, joint.inverse() * projector))
}

fn wound_tag(splat: u32, age: f32) -> u32 {
    WOUND_TAG | splat & SPLAT_BLEND | ((dryness(age) * DRY_STEPS).round() as u32) << DRY_SHIFT
}

fn place_wounds(wounds: Res<Wounds>, joints: Query<&GlobalTransform, Without<ClusteredDecal>>, mut decals: Query<(&mut Transform, &mut GlobalTransform), With<ClusteredDecal>>) {
    for wound in wounds.bodies.values().flatten() {
        let (Ok(joint), Ok((mut transform, mut global))) = (joints.get(wound.joint), decals.get_mut(wound.decal)) else { continue };
        let placed = joint.affine() * Affine3A::from_mat4(wound.local);
        transform.set_if_neq(Transform::from_matrix(Mat4::from(placed)));
        global.set_if_neq(GlobalTransform::from(placed));
    }
}

/// Each hit on a person that pushes it (a shot or a blow, not fire or a splash) leaves one of the game's wall
/// blood decals on the nearest bone, each wound its own size and turn.
pub fn present_wounds(
    (facts, splats): (Res<FrameFacts>, Res<WoundSplats>),
    mut wounds: ResMut<Wounds>,
    mut commands: Commands,
    skins: Query<(&FollowActor, &SkinnedMesh)>,
    joints: Query<&GlobalTransform>,
    mut decals: Query<&mut ClusteredDecal>,
) {
    let wounds = &mut *wounds;
    if !facts.in_play {
        return;
    }
    let step = facts.step();
    wounds.bodies.retain(|_, body| {
        body.retain_mut(|wound| {
            let Ok(mut decal) = decals.get_mut(wound.decal) else { return false };
            let Ok(_) = joints.get(wound.joint) else {
                commands.entity(wound.decal).despawn();
                return false;
            };
            wound.age += step;
            let tag = wound_tag(wound.tag, wound.age);
            if decal.tag != tag {
                decal.tag = tag;
            }
            true
        });
        !body.is_empty()
    });
    if splats.0.is_empty() {
        return;
    }
    let origin = facts.origin;
    for hit in facts.person_hits.iter().filter(|hit| hit.damage > 0.0) {
        let Some(toward) = Vec3::from(to_bevy(hit.momentum)).try_normalize() else { continue };
        let Some((_, skin)) = skins.iter().find(|(follow, _)| follow.actor == hit.person) else { continue };
        let placed: Vec<(Entity, Mat4)> = skin.joints.iter().filter_map(|&joint| Some((joint, Mat4::from(joints.get(joint).ok()?.affine())))).collect();
        let frames: Vec<Mat4> = placed.iter().map(|&(_, frame)| frame).collect();
        wounds.made = wounds.made.wrapping_add(1);
        let seed = wounds.made;
        let (image, splat) = &splats.0[seed as usize % splats.0.len()];
        let size = WOUND_SIZE + WOUND_GROWTH * (seed.wrapping_mul(7) % WOUND_SIZES) as f32 / (WOUND_SIZES - 1) as f32;
        let roll = seed.wrapping_mul(2_654_435_761) as f32 / u32::MAX as f32 * std::f32::consts::TAU;
        let Some((index, local)) = wound_frame(&frames, origin + Vec3::from(to_bevy(hit.at)), toward, size, roll) else { continue };
        let decal = ClusteredDecal { base_color_texture: Some(image.clone()), tag: wound_tag(*splat, 0.0), ..Default::default() };
        let decal = commands.spawn((MapScoped, decal, Transform::from_matrix(frames[index] * local))).id();
        let wound = Wound { decal, joint: placed[index].0, local, tag: *splat, age: 0.0 };
        if let Some(old) = add_oldest_out(wounds.bodies.entry(hit.person).or_default(), wound, |wound| wound.age)
            && let Ok(mut old) = commands.get_entity(old.decal)
        {
            old.despawn();
        }
    }
}

fn drop_wounds(mut wounds: ResMut<Wounds>, mut commands: Commands) {
    for wound in wounds.bodies.drain().flat_map(|(_, body)| body) {
        if let Ok(mut decal) = commands.get_entity(wound.decal) {
            decal.despawn();
        }
    }
}

#[derive(Component)]
pub struct DecalAge {
    tag: u32,
    life: f32,
    age: f32,
    blood: bool,
}

impl DecalAge {
    pub fn new(tag: u32, life: f32, texture: &str) -> Self {
        Self { tag, life, age: 0.0, blood: is_blood(texture) }
    }
}

#[derive(Component)]
pub struct BloodDecal;

pub fn is_blood(texture: &str) -> bool {
    texture.to_ascii_lowercase().contains("blood")
}

fn age_decals(facts: Res<FrameFacts>, mut decals: Query<(&mut ClusteredDecal, &mut DecalAge)>) {
    let step = facts.step();
    for (mut decal, mut age) in &mut decals {
        age.age += step;
        let tag = aged_tag(age.tag, age.life, age.age, age.blood.then_some(age.age));
        if decal.tag != tag {
            decal.tag = tag;
        }
    }
}

fn restore_decal_tags(mut decals: Query<(&mut ClusteredDecal, &DecalAge)>) {
    for (mut decal, age) in &mut decals {
        decal.tag = age.tag;
    }
}

/// With our liquids on, the game's blood decals and wounds carry a flat normal map, which marks them for
/// `projector.wesl` to draw as our blood; with them off, none does.
fn mark_blood_decals(
    settings: Option<Res<Settings>>,
    mut images: Option<ResMut<Assets<Image>>>,
    mut marker: Local<Option<Handle<Image>>>,
    mut decals: Query<(&mut ClusteredDecal, Option<&DecalAge>, Has<BloodDecal>)>,
) {
    let on = modern_level(settings.as_deref(), MODERN_LIQUIDS) > 0;
    if on && marker.is_none() {
        let Some(images) = images.as_deref_mut() else { return };
        let flat = Image::new_fill(Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, TextureDimension::D2, &[128, 128, 255, 255], TextureFormat::Rgba8Unorm, RenderAssetUsages::RENDER_WORLD);
        *marker = Some(images.add(flat));
    }
    for (mut decal, age, placed) in &mut decals {
        let wound = decal.tag & WOUND_TAG == WOUND_TAG && decal.metallic_roughness_texture.is_none();
        let blood = age.is_some_and(|age| age.blood) || placed || wound;
        let wanted = marker.as_ref().filter(|_| on && blood);
        if decal.normal_map_texture.as_ref() != wanted {
            decal.normal_map_texture = wanted.cloned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaders::shader_const;

    #[test]
    fn the_shader_reads_a_wounds_tag_as_it_is_written() {
        let shader = include_str!("projector.wesl");
        let bits = |name| shader_const(shader, name) as u32;
        assert_eq!(bits("DRY_SHIFT"), DRY_SHIFT);
        assert_eq!(bits("FADE_SHIFT"), FADE_SHIFT);
        assert_eq!(bits("PROJECTOR") | bits("TARGET_ACTOR") | bits("WOUND"), WOUND_TAG);
    }

    #[test]
    fn a_body_stands_its_wounds_as_blood_and_skips_the_paste() {
        let shader = include_str!("actor_light.wesl");
        assert!(shader.contains("splat_pool(pbr_input, decals, normal, actor_target(in.instance_index))"));
        assert!(shader.contains("actor_target(in.instance_index), pooled)"));
    }

    #[test]
    fn blood_dries_from_wet_and_levels_off() {
        assert_eq!(dryness(0.0), 0.0);
        assert!(dryness(1.0) < dryness(2.0) && dryness(2.0) < dryness(8.0));
        assert!(dryness(20.0) > 0.99 && dryness(20.0) <= 1.0);
    }

    #[test]
    fn a_wound_projects_along_the_shot_to_the_joint_and_moves_with_it() {
        let joints = [Mat4::IDENTITY, Mat4::from_translation(Vec3::new(0.0, 50.0, 0.0)), Mat4::from_translation(Vec3::new(30.0, 40.0, 0.0))];
        let hit = Vec3::new(28.0, 42.0, 5.0);
        let (index, local) = wound_frame(&joints, hit, Vec3::NEG_Z, 9.0, 0.0).unwrap();
        assert_eq!(index, 2);
        let placed = joints[2] * local;
        assert!(placed.transform_point3(Vec3::new(0.0, 0.0, 0.5)).distance(hit + Vec3::Z * BACK_REACH) < 1e-4);
        assert!(placed.transform_point3(Vec3::new(0.0, 0.0, -0.5)).distance(Vec3::new(28.0, 42.0, 0.0)) < 1e-4);
        assert!((placed.x_axis.length() - 9.0).abs() < 1e-4 && (placed.y_axis.length() - 9.0).abs() < 1e-4);
        let turned = Mat4::from_rotation_translation(bevy::math::Quat::from_rotation_y(1.0), Vec3::new(30.0, 40.0, 10.0));
        let moved = (turned * local).transform_point3(Vec3::new(0.0, 0.0, 0.5));
        assert!(moved.distance(turned.transform_point3(Vec3::new(-2.0, 2.0, 7.0))) < 1e-4);
        assert!(wound_frame(&joints, Vec3::new(500.0, 0.0, 0.0), Vec3::NEG_Z, 9.0, 0.0).is_none());
        assert!(wound_frame(&[], hit, Vec3::NEG_Z, 9.0, 0.0).is_none());
    }

    #[test]
    fn a_roll_turns_the_splash_and_keeps_its_size() {
        let (_, flat) = wound_frame(&[Mat4::IDENTITY], Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z, 9.0, 0.0).unwrap();
        let (_, turned) = wound_frame(&[Mat4::IDENTITY], Vec3::new(0.0, 0.0, 5.0), Vec3::NEG_Z, 9.0, std::f32::consts::FRAC_PI_2).unwrap();
        assert!((turned.x_axis.length() - 9.0).abs() < 1e-4 && (turned.y_axis.length() - 9.0).abs() < 1e-4);
        assert!(turned.x_axis.truncate().normalize().abs_diff_eq(flat.y_axis.truncate().normalize(), 1e-4));
        assert!(turned.w_axis.abs_diff_eq(flat.w_axis, 1e-4));
    }

    #[test]
    fn the_oldest_wound_gives_way() {
        let mut ages: Vec<f32> = Vec::new();
        for age in [3.0, 9.0, 1.0, 4.0, 2.0, 8.0, 5.0, 6.0] {
            assert_eq!(add_oldest_out(&mut ages, age, |&age| age), None);
        }
        assert_eq!(add_oldest_out(&mut ages, 0.0, |&age| age), Some(9.0));
        assert_eq!(ages.len(), WOUNDS_PER_BODY);
        assert_eq!(ages[1], 0.0);
    }

    #[test]
    fn a_wound_takes_the_splats_blend_and_dries() {
        let blend = 1 << 21 | 10 << 22 | 1 << 8 | 2;
        let splat = 1 << 31 | 1 << 30 | 1 << 16 | 0x80 | blend;
        assert_eq!(wound_tag(splat, 0.0), WOUND_TAG | blend);
        assert_eq!(wound_tag(splat, 60.0), WOUND_TAG | blend | 15 << DRY_SHIFT);
        assert_eq!((WOUND_TAG | SPLAT_BLEND) & (0xf << DRY_SHIFT | 0x80 | 0x1f << 10), 0);
    }

    #[test]
    fn blood_is_named_in_the_texture_path() {
        assert!(is_blood("P2Blood.Splats.BloodSplat1"));
        assert!(is_blood("nathans.Inventory.bloodsplat"));
        assert!(!is_blood("Effects.Scorch.BulletHole"));
        assert!(!is_blood("Fluids.PeeSplat"));
    }

    #[test]
    fn with_liquids_on_blood_decals_and_wounds_are_marked_and_nothing_else() {
        fn liquids(level: &'static str) -> Settings {
            Settings::read(move |query: &str| match query {
                p2_host::display::MODERN_GRAPHICS => "1".to_owned(),
                MODERN_LIQUIDS => level.to_owned(),
                _ => String::new(),
            })
        }
        let mut app = App::new();
        app.insert_resource(liquids("10")).init_resource::<Assets<Image>>().add_systems(PostUpdate, mark_blood_decals);
        let blood = app.world_mut().spawn((ClusteredDecal::default(), DecalAge::new(3, 0.0, "nathans.Skins.bloodsplatblend1"))).id();
        let hole = app.world_mut().spawn((ClusteredDecal::default(), DecalAge::new(3, 0.0, "Effects.Scorch.BulletHole"))).id();
        let wound = app.world_mut().spawn(ClusteredDecal { tag: WOUND_TAG, ..ClusteredDecal::default() }).id();
        let placed = app.world_mut().spawn((ClusteredDecal::default(), BloodDecal)).id();
        let pool = app.world_mut().spawn(ClusteredDecal { tag: WOUND_TAG, metallic_roughness_texture: Some(Handle::default()), ..ClusteredDecal::default() }).id();
        let marked = |app: &App, entity: Entity| app.world().get::<ClusteredDecal>(entity).is_some_and(|decal| decal.normal_map_texture.is_some());
        app.update();
        assert!(marked(&app, blood) && marked(&app, wound) && marked(&app, placed) && !marked(&app, hole) && !marked(&app, pool));
        app.insert_resource(liquids("0"));
        app.update();
        assert!(!marked(&app, blood) && !marked(&app, wound));
    }

    #[test]
    fn a_frames_game_step_ages_a_decals_tag() {
        let mut app = App::new();
        let on = |query: &str| if [p2_host::display::MODERN_GRAPHICS, MODERN_DECAL_AGING].contains(&query) { "1".to_owned() } else { String::new() };
        app.insert_resource(Settings::read(on)).insert_resource(FrameFacts { game_step: 4.5, ..FrameFacts::default() }).add_systems(PostUpdate, age_decals.run_if(modern_on(MODERN_DECAL_AGING)));
        let decal = app.world_mut().spawn((ClusteredDecal::default(), DecalAge::new(3, 5.0, "Effects.Scorch.BulletHole"))).id();
        app.update();
        assert_ne!(aged_tag(3, 5.0, 4.5, None), 3);
        assert_eq!(app.world().get::<ClusteredDecal>(decal).map(|decal| decal.tag), Some(aged_tag(3, 5.0, 4.5, None)));
    }

    #[test]
    fn fades_over_the_last_second_only() {
        assert_eq!(fade(10.0, 0.0), 0.0);
        assert_eq!(fade(10.0, 9.0), 0.0);
        assert!((fade(10.0, 9.5) - 0.5).abs() < 1e-5);
        assert_eq!(fade(10.0, 10.0), 1.0);
        assert_eq!(fade(10.0, 12.0), 1.0);
        assert_eq!(fade(0.0, 100.0), 0.0);
        assert!((fade(0.5, 0.0) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn aging_keeps_the_projector_bits_and_a_fresh_decal_its_tag() {
        let used: u32 = 0xffff_0000 | 0x8000 | 0x300 | 0x87;
        assert_eq!((0x1f << FADE_SHIFT | 0xf << DRY_SHIFT) & used, 0);
        let tag = 0x8001_0102;
        assert_eq!(aged_tag(tag, 5.0, 0.0, Some(0.0)), tag);
        let ending = aged_tag(tag, 5.0, 5.0, Some(5.0));
        assert_eq!(ending & used, tag);
        assert_eq!(ending >> FADE_SHIFT & 0x1f, 31);
        assert_eq!(aged_tag(1, 0.0, 30.0, None), 1);
    }
}
