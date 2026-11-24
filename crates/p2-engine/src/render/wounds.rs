use bevy::app::{App, Plugin, PostUpdate};
use bevy::asset::{Assets, Handle, RenderAssetUsages};
use bevy::ecs::prelude::{Commands, Entity, Query, Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::image::Image;
use bevy::light::ClusteredDecal;
use bevy::math::{Mat4, Quat, Vec2, Vec3};
use bevy::mesh::skinning::SkinnedMesh;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
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
/// A projector (bit 31) aimed at actors (bit 19), alpha blended (2), with the wound bit 15: it also takes the bodies
/// that refuse the game's projectors.
const WOUND_TAG: u32 = 1 << 31 | 1 << 19 | 1 << 15 | 2;
const WOUNDS_PER_BODY: usize = 8;
const WOUND_SIZE: f32 = 9.0;
/// A hit farther than this from every bone (a hit without a place) leaves no mark.
const BONE_REACH: f32 = 48.0;
const BLOT_TEXELS: u32 = 32;

/// 0 = wet, towards 1 = dry, `age` game seconds after the blood landed.
pub(crate) fn dryness(age: f32) -> f32 {
    1.0 - (-age.max(0.0) / DRYING_SECONDS).exp()
}

pub(crate) struct WoundsPlugin;

impl Plugin for WoundsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wounds>().add_systems(PostUpdate, present_wounds.in_set(MapSystems::Present));
    }
}

struct Wound {
    decal: Entity,
    joint: Entity,
    /// The decal's box in the joint's space.
    local: Mat4,
    age: f32,
}

#[derive(Resource, Default)]
struct Wounds {
    blot: Option<Handle<Image>>,
    bodies: FxHashMap<ObjectId, Vec<Wound>>,
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

/// The joint nearest `hit` among `joints` (joint-to-world) and a `size` box at `hit` in that joint's space.
fn wound_frame(joints: &[Mat4], hit: Vec3, size: f32) -> Option<(usize, Mat4)> {
    let distance = |joint: &Mat4| joint.w_axis.truncate().distance(hit);
    let (index, joint) = joints.iter().enumerate().min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))?;
    if distance(joint) > BONE_REACH {
        return None;
    }
    Some((index, joint.inverse() * Mat4::from_scale_rotation_translation(Vec3::splat(size), Quat::IDENTITY, hit)))
}

fn wound_tag(age: f32) -> u32 {
    WOUND_TAG | ((dryness(age) * DRY_STEPS).round() as u32) << DRY_SHIFT
}

/// Dark red, opaque at the middle and gone at the rim.
fn blot_alpha(texel: Vec2) -> f32 {
    let from_middle = (texel / BLOT_TEXELS as f32 * 2.0 - Vec2::ONE).length();
    (1.0 - from_middle).clamp(0.0, 1.0).powf(0.6)
}

fn blot() -> Image {
    let texels = (0..BLOT_TEXELS * BLOT_TEXELS)
        .flat_map(|i| {
            let alpha = blot_alpha(Vec2::new((i % BLOT_TEXELS) as f32 + 0.5, (i / BLOT_TEXELS) as f32 + 0.5));
            [90, 6, 4, (alpha * 255.0).round() as u8]
        })
        .collect();
    let size = Extent3d { width: BLOT_TEXELS, height: BLOT_TEXELS, depth_or_array_layers: 1 };
    Image::new(size, TextureDimension::D2, texels, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default())
}

/// Each hit on a person leaves a decal box on the bone nearest it, placed from that joint every frame.
fn present_wounds(
    (settings, play): (Option<Res<Settings>>, Res<Play>),
    mut wounds: ResMut<Wounds>,
    mut images: ResMut<Assets<Image>>,
    mut commands: Commands,
    skins: Query<(&FollowActor, &SkinnedMesh)>,
    joints: Query<&GlobalTransform>,
    mut decals: Query<(&mut Transform, &mut ClusteredDecal)>,
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
            let Ok((mut transform, mut decal)) = decals.get_mut(wound.decal) else { return false };
            let Ok(joint) = joints.get(wound.joint) else {
                commands.entity(wound.decal).despawn();
                return false;
            };
            wound.age += step;
            *transform = Transform::from_matrix(Mat4::from(joint.affine()) * wound.local);
            let tag = wound_tag(wound.age);
            if decal.tag != tag {
                decal.tag = tag;
            }
            true
        });
        !body.is_empty()
    });
    let origin = world.origin();
    for hit in world.person_hits() {
        let Some((_, skin)) = skins.iter().find(|(follow, _)| follow.actor == hit.person) else { continue };
        let placed: Vec<(Entity, Mat4)> = skin.joints.iter().filter_map(|&joint| Some((joint, Mat4::from(joints.get(joint).ok()?.affine())))).collect();
        let frames: Vec<Mat4> = placed.iter().map(|&(_, frame)| frame).collect();
        let [x, y, z]: [f32; 3] = hit.at.into();
        let Some((index, local)) = wound_frame(&frames, origin + Vec3::new(x, z, y), WOUND_SIZE) else { continue };
        let image = wounds.blot.get_or_insert_with(|| images.add(blot())).clone();
        let decal = ClusteredDecal { base_color_texture: Some(image), tag: wound_tag(0.0), ..Default::default() };
        let decal = commands.spawn((MapScoped, decal, Transform::from_matrix(frames[index] * local))).id();
        let wound = Wound { decal, joint: placed[index].0, local, age: 0.0 };
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
    fn a_wound_sits_on_the_nearest_joint_and_moves_with_it() {
        let joints = [Mat4::IDENTITY, Mat4::from_translation(Vec3::new(0.0, 50.0, 0.0)), Mat4::from_translation(Vec3::new(30.0, 40.0, 0.0))];
        let hit = Vec3::new(28.0, 42.0, 5.0);
        let (index, local) = wound_frame(&joints, hit, 9.0).unwrap();
        assert_eq!(index, 2);
        let (scale, _, at) = (joints[2] * local).to_scale_rotation_translation();
        assert!(at.distance(hit) < 1e-4 && (scale - Vec3::splat(9.0)).length() < 1e-4);
        let turned = Mat4::from_rotation_translation(Quat::from_rotation_y(1.0), Vec3::new(30.0, 40.0, 10.0));
        let moved = (turned * local).w_axis.truncate();
        assert!(moved.distance(turned.transform_point3(Vec3::new(-2.0, 2.0, 5.0))) < 1e-4);
        assert!(wound_frame(&joints, Vec3::new(500.0, 0.0, 0.0), 9.0).is_none());
        assert!(wound_frame(&[], hit, 9.0).is_none());
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
    fn a_wound_tag_dries_and_keeps_its_bits() {
        assert_eq!(wound_tag(0.0), WOUND_TAG);
        assert_eq!(wound_tag(60.0), WOUND_TAG | 15 << DRY_SHIFT);
        assert_eq!(WOUND_TAG & (0xf << DRY_SHIFT | 0x80), 0);
    }

    #[test]
    fn the_blot_is_solid_in_the_middle_and_clear_at_the_rim() {
        assert!(blot_alpha(Vec2::splat(BLOT_TEXELS as f32 / 2.0)) > 0.99);
        assert_eq!(blot_alpha(Vec2::ZERO), 0.0);
        assert_eq!(blot().data.map(|data| data.len()), Some((BLOT_TEXELS * BLOT_TEXELS * 4) as usize));
    }
}
