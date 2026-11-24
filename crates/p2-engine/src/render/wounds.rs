use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::Handle;
use bevy::ecs::prelude::{Commands, Entity, Query, Res, ResMut, Resource, With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::{ClusteredDecal, SimulationLightSystems};
use bevy::math::{Affine3A, Mat4, Vec3};
use bevy::mesh::skinning::SkinnedMesh;
use bevy::transform::TransformSystems;
use bevy::transform::components::{GlobalTransform, Transform};
use p2_vm::ObjectId;
use rustc_hash::FxHashMap;

use crate::components::{FollowActor, MapScoped};
use crate::display::{MODERN_WOUNDS, Settings};
use crate::map::MapSystems;
use crate::play::Play;

/// Blood drying in projector tag bits 3..7 (the frame buffer blend keeps bits 0..3); `projector.wesl` reads the same.
pub(crate) const DRY_SHIFT: u32 = 3;
pub(crate) const DRY_STEPS: f32 = 15.0;
const DRYING_SECONDS: f32 = 4.0;
/// A projector (bit 31) aimed at actors (bit 19), with the wound bit 15: it also takes the bodies that refuse the
/// game's projectors, and only the faces turned to the shot.
const WOUND_TAG: u32 = 1 << 31 | 1 << 19 | 1 << 15;
/// What a wound keeps of a blood splat's tag: the frame buffer blend, the material blend and the alpha test.
const SPLAT_BLEND: u32 = 0x7 | 0x300 | 0x3fe0_0000;
const WOUNDS_PER_BODY: usize = 8;
/// A wound is this wide in world units, and up to `WOUND_GROWTH` wider: a splash across a chest, not a wall's.
const WOUND_SIZE: f32 = 12.0;
const WOUND_GROWTH: f32 = 8.0;
const WOUND_SIZES: u32 = 5;
/// A hit farther than this from every bone (a hit without a place) leaves no mark.
const BONE_REACH: f32 = 48.0;
const BACK_REACH: f32 = 2.0;

/// 0 = wet, towards 1 = dry, `age` game seconds after the blood landed.
pub(crate) fn dryness(age: f32) -> f32 {
    1.0 - (-age.max(0.0) / DRYING_SECONDS).exp()
}

pub(crate) struct WoundsPlugin;

impl Plugin for WoundsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wounds>().init_resource::<WoundSplats>().add_systems(
            PostUpdate,
            (present_wounds.in_set(MapSystems::Present), place_wounds.after(TransformSystems::Propagate).before(SimulationLightSystems::AssignLightsToClusters)),
        );
    }
}

/// The images of the game's wall blood decals, each with its blend as a projector tag: a wound draws one of them.
/// Never the floor drips or the fluids' sprites.
#[derive(Resource, Default)]
pub(crate) struct WoundSplats(pub(crate) Vec<(Handle<Image>, u32)>);

struct Wound {
    decal: Entity,
    joint: Entity,
    /// The decal's box in the joint's space.
    local: Mat4,
    tag: u32,
    age: f32,
}

#[derive(Resource, Default)]
pub(crate) struct Wounds {
    bodies: FxHashMap<ObjectId, Vec<Wound>>,
    made: u32,
}

/// Past [`WOUNDS_PER_BODY`] the oldest gives way and is returned.
fn add_oldest_out<T>(list: &mut Vec<T>, item: T, age: impl Fn(&T) -> f32) -> Option<T> {
    if list.len() < WOUNDS_PER_BODY {
        list.push(item);
        return None;
    }
    let oldest = list.iter_mut().max_by(|a, b| age(a).total_cmp(&age(b)))?;
    Some(std::mem::replace(oldest, item))
}

/// The joint nearest `hit` among `joints` (joint-to-world) and, in that joint's space, a projector box `size` across
/// shooting along `toward` (unit) from just before the hit to the joint's depth: the near half of the limb only.
/// `roll` turns the splash about the shot, as the game turns a wall splat about the wall's normal.
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

/// After this frame's joints are placed, the transform written straight through: a wound does not trail its body.
fn place_wounds(wounds: Res<Wounds>, joints: Query<&GlobalTransform, Without<ClusteredDecal>>, mut decals: Query<(&mut Transform, &mut GlobalTransform), With<ClusteredDecal>>) {
    for wound in wounds.bodies.values().flatten() {
        let (Ok(joint), Ok((mut transform, mut global))) = (joints.get(wound.joint), decals.get_mut(wound.decal)) else { continue };
        let placed = joint.affine() * Affine3A::from_mat4(wound.local);
        *transform = Transform::from_matrix(Mat4::from(placed));
        *global = GlobalTransform::from(placed);
    }
}

/// Each hit on a person that pushes it (a shot or a blow, not fire or a splash) leaves one of the game's wall blood
/// decals on the bone nearest it, each wound its own size and turn.
pub(crate) fn present_wounds(
    (settings, play, splats): (Option<Res<Settings>>, Res<Play>, Res<WoundSplats>),
    mut wounds: ResMut<Wounds>,
    mut commands: Commands,
    skins: Query<(&FollowActor, &SkinnedMesh)>,
    joints: Query<&GlobalTransform>,
    mut decals: Query<&mut ClusteredDecal>,
) {
    let wounds = &mut *wounds;
    if settings.is_none_or(|settings| settings.modern_level(MODERN_WOUNDS) == 0) {
        for wound in wounds.bodies.drain().flat_map(|(_, body)| body) {
            if let Ok(mut decal) = commands.get_entity(wound.decal) {
                decal.despawn();
            }
        }
        return;
    }
    let Some(world) = play.world() else { return };
    let step = if world.game_paused() { 0.0 } else { world.game_step() };
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
    let origin = world.origin();
    for hit in world.person_hits().iter().filter(|hit| hit.damage > 0.0) {
        let [x, y, z]: [f32; 3] = hit.momentum.into();
        let Some(toward) = Vec3::new(x, z, y).try_normalize() else { continue };
        let Some((_, skin)) = skins.iter().find(|(follow, _)| follow.actor == hit.person) else { continue };
        let placed: Vec<(Entity, Mat4)> = skin.joints.iter().filter_map(|&joint| Some((joint, Mat4::from(joints.get(joint).ok()?.affine())))).collect();
        let frames: Vec<Mat4> = placed.iter().map(|&(_, frame)| frame).collect();
        let [x, y, z]: [f32; 3] = hit.at.into();
        wounds.made = wounds.made.wrapping_add(1);
        let seed = wounds.made;
        let (image, splat) = &splats.0[seed as usize % splats.0.len()];
        let size = WOUND_SIZE + WOUND_GROWTH * (seed.wrapping_mul(7) % WOUND_SIZES) as f32 / (WOUND_SIZES - 1) as f32;
        let roll = seed.wrapping_mul(2_654_435_761) as f32 / u32::MAX as f32 * std::f32::consts::TAU;
        let Some((index, local)) = wound_frame(&frames, origin + Vec3::new(x, z, y), toward, size, roll) else { continue };
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
