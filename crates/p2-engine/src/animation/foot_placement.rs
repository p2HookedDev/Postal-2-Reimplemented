//! Feet put down on stairs and slopes after the animation's pose, before skinning. Only the drawn pose changes.

use std::sync::Arc;

use bevy::app::{App, Plugin, Update};
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Mat4, Quat, Vec3};
use bevy::time::Time;
use p2_assets::SkeletalMesh;
use p2_assets::pose::subtree;
use p2_bevy_assets::rotator_matrix;
use p2_math::Vector;
use p2_vm::ObjectId;
use rustc_hash::{FxHashMap, FxHashSet};

use super::drawn_pose::{keep_for_game, mesh_place, posed_origin, restore, turn_about};
use super::{Animated, AnimatedMesh, AnimationSystems, Poses, animate, pose_joints};
use p2_host::display::{MODERN_FOOT_PLACEMENT, Settings};
use p2_render::level_collision::{LevelCollision, StaticCollision};
use crate::play::Play;

/// Unreal units; below the walk's step height, so a foot never reaches past what the walker could step on.
const STEP: f32 = 24.0;
const RANGE: f32 = 2048.0;
/// A foot this far above its planted height is mid-stride and left alone.
const PLANT_FULL: f32 = 4.0;
const PLANT_NONE: f32 = 16.0;
const MAX_TILT: f32 = 25.0 * std::f32::consts::PI / 180.0;
const MIN_GROUND_Z: f32 = 0.7;
/// Per second: about five frames at 60 to settle.
const BLEND_RATE: f32 = 15.0;
const SETTLED: f32 = 0.05;
/// Seconds over which a body that became a ragdoll loses the correction it stood with.
const HAND_OVER: f32 = 0.5;
pub(super) const SIDES: [[&str; 3]; 2] =[[" l thigh", " l calf", " l foot"], [" r thigh", " r calf", " r foot"]];

pub(super) struct FootPlacementPlugin;

impl Plugin for FootPlacementPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, place_feet.after(animate).before(pose_joints).in_set(AnimationSystems::Pose));
    }
}

struct Leg {
    thigh: usize,
    calf: usize,
    foot: usize,
    thigh_tree: Vec<bool>,
    calf_tree: Vec<bool>,
    foot_tree: Vec<bool>,
}

struct Rig {
    legs: [Leg; 2],
    /// The lowest point of the bind pose: where the animation stands on flat ground.
    sole: Vec3,
    bind_ankles: [Vec3; 2],
    bind_hips: [Vec3; 2],
}

type Shown = ([f32; 2], [Vec3; 2]);

const REST: Shown = ([0.0; 2], [Vec3::Z; 2]);

struct Person {
    mesh: Arc<SkeletalMesh>,
    rig: Option<Rig>,
    animated: Vec<Mat4>,
    place: Mat4,
    shown: Shown,
    applied: Option<Shown>,
    /// What was shown when a ragdoll took the body, and the game seconds since.
    handed: Option<(Shown, f32)>,
}

impl Person {
    fn new(item: &AnimatedMesh, bones: &[Mat4]) -> Self {
        Self { mesh: Arc::clone(&item.mesh), rig: leg_rig(item), animated: bones.to_vec(), place: Mat4::IDENTITY, shown: REST, applied: None, handed: None }
    }
}

#[derive(Default)]
pub(super) struct Feet {
    people: FxHashMap<ObjectId, Person>,
    visited: FxHashSet<ObjectId>,
    drawn: FxHashSet<ObjectId>,
    drawn_before: FxHashSet<ObjectId>,
    generation: u64,
}

pub(super) fn place_feet(
    time: Res<Time>,
    settings: Option<Res<Settings>>,
    play: Res<Play>,
    collision: Option<Res<LevelCollision>>,
    animated: Res<Animated>,
    mut poses: ResMut<Poses>,
    mut feet: Local<Feet>,
) {
    let poses = &mut *poses;
    let feet = &mut *feet;
    let on = settings.is_some_and(|settings| settings.modern_level(MODERN_FOOT_PLACEMENT) > 0);
    let statics = collision.as_ref().and_then(|collision| collision.0.as_deref());
    let (true, Some(world), Some(statics)) = (on, play.world(), statics) else {
        for (actor, person) in feet.people.drain() {
            restore(poses, actor, person.animated);
        }
        return;
    };
    if feet.generation != play.generation() {
        feet.generation = play.generation();
        feet.people.clear();
        feet.drawn.clear();
    }
    std::mem::swap(&mut feet.drawn, &mut feet.drawn_before);
    feet.drawn.clear();
    let eye = world.view_location().map(|location| Vec3::from_array(location.into()));
    let cutscene = world.cutscene();
    let blend = 1.0 - (-time.delta_secs() * BLEND_RATE).exp();
    let seconds = if world.game_paused() { 0.0 } else { time.delta_secs() * world.time_dilation() };
    feet.visited.clear();
    for item in &animated.0 {
        let Some(actor) = item.actor else { continue };
        if !feet.visited.insert(actor) {
            continue;
        }
        let near = |place: &Mat4| eye.is_some_and(|eye| eye.distance(place.w_axis.truncate()) <= RANGE);
        let drawn = world.drawn(actor);
        if drawn {
            feet.drawn.insert(actor);
        }
        let placed = if cutscene || !drawn { None } else { mesh_place(world, item, actor).filter(near) };
        let known = feet.people.contains_key(&actor);
        if placed.is_none() && !known {
            continue;
        }
        // Someone coming into view is first seen corrected; one already seen eases into it.
        let snap = !known && !feet.drawn_before.contains(&actor);
        let Some(bones) = poses.bones.get_mut(&actor) else {
            feet.people.remove(&actor);
            continue;
        };
        let fresh = poses.changed.contains(&actor);
        let person = feet.people.entry(actor).or_insert_with(|| Person::new(item, bones));
        if !Arc::ptr_eq(&person.mesh, &item.mesh) {
            *person = Person::new(item, bones);
        }
        if fresh {
            person.animated.clone_from(bones);
            person.applied = None;
        }
        let Some(rig) = &person.rig else {
            if placed.is_none() {
                feet.people.remove(&actor);
            }
            continue;
        };
        let target = match placed {
            Some(place) => {
                person.place = place;
                rig.targets(&person.animated, place, statics)
            }
            None => REST,
        };
        person.handed = match (world.ragdolled(actor), person.handed) {
            (false, _) => None,
            (true, Some((from, age))) => Some((from, age + seconds)),
            (true, None) => Some((person.shown, 0.0)),
        };
        person.shown = match person.handed {
            Some((from, age)) => hand_over(from, age),
            None if snap => target,
            None => ease(person.shown, target, blend),
        };
        let shown = person.shown;
        let rest = shown == REST;
        if person.applied != Some(shown) {
            person.applied = Some(shown);
            if !rest {
                keep_for_game(&mut poses.animated, actor, &person.animated);
                bones.clone_from(&person.animated);
                rig.apply(bones, person.place, shown);
                poses.changed.insert(actor);
            } else if !fresh {
                bones.clone_from(&person.animated);
                poses.changed.insert(actor);
            }
        }
        if rest && placed.is_none() {
            feet.people.remove(&actor);
        }
    }
    feet.people.retain(|actor, _| poses.bones.contains_key(actor));
}

fn leg_rig(item: &AnimatedMesh) -> Option<Rig> {
    let parents = &item.parents;
    let leg = |[thigh, calf, foot]: [&str; 3]| {
        let find = |suffix: &str| item.bone_names.iter().position(|name| name.ends_with(suffix));
        let (thigh, calf, foot) = (find(thigh)?, find(calf)?, find(foot)?);
        (parents.get(foot) == Some(&calf) && parents.get(calf) == Some(&thigh)).then(|| Leg {
            thigh,
            calf,
            foot,
            thigh_tree: subtree(parents, Some(thigh)),
            calf_tree: subtree(parents, Some(calf)),
            foot_tree: subtree(parents, Some(foot)),
        })
    };
    let legs = [leg(SIDES[0])?, leg(SIDES[1])?];
    let bind_ankle = |leg: &Leg| item.inverse_bind.get(leg.foot).map(|inverse| inverse.inverse().w_axis.truncate());
    let bind_ankles = [bind_ankle(&legs[0])?, bind_ankle(&legs[1])?];
    let bind_hip = |leg: &Leg| item.inverse_bind.get(leg.thigh).map(|inverse| inverse.inverse().w_axis.truncate());
    let bind_hips = [bind_hip(&legs[0])?, bind_hip(&legs[1])?];
    let upright = rotator_matrix(item.mesh.rot_origin) * Mat4::from_scale(Vec3::from_array(item.mesh.scale.into()));
    let height = |point: &[f32; 3]| upright.transform_point3(Vec3::from(*point)).z;
    let sole = item.mesh.positions.iter().min_by(|a, b| height(a).total_cmp(&height(b))).map(|point| Vec3::from(*point))?;
    Some(Rig { legs, sole, bind_ankles, bind_hips })
}

impl Rig {
    /// Both hips above half their bind height over the sole: a body lying down, dead or alive, is left alone.
    fn standing(&self, pose: &[Mat4], place: Mat4) -> bool {
        let height = |point: Vec3| place.transform_point3(point).z - place.transform_point3(self.sole).z;
        self.legs.iter().zip(self.bind_hips).all(|(leg, hip)| pose.get(leg.thigh).is_some_and(|bone| height(bone.w_axis.truncate()) >= 0.5 * height(hip)))
    }

    /// At most one trace per foot, and none for a foot in the air.
    fn targets(&self, pose: &[Mat4], place: Mat4, statics: &StaticCollision) -> Shown {
        let floor = place.transform_point3(self.sole).z;
        let mut shown = REST;
        if !self.standing(pose, place) {
            return shown;
        }
        for (side, leg) in self.legs.iter().enumerate() {
            let Some(bone) = pose.get(leg.foot) else { continue };
            let ankle = place.transform_point3(bone.w_axis.truncate());
            let weight = plant(ankle.z - place.transform_point3(self.bind_ankles[side]).z);
            if weight == 0.0 {
                continue;
            }
            let (start, end) = (Vector::new(ankle.x, ankle.y, floor + STEP), Vector::new(ankle.x, ankle.y, floor - STEP));
            let Some(hit) = statics.first_hit(start, end) else { continue };
            let normal = Vec3::from_array(hit.normal.into());
            if normal.z < MIN_GROUND_Z {
                continue;
            }
            shown.0[side] = foot_offset(hit.location.z, floor, weight);
            shown.1[side] = limit_tilt(normal, weight);
        }
        shown
    }

    /// The body drops by the lower foot's drop; each leg then reaches its own ground.
    fn apply(&self, bones: &mut [Mat4], place: Mat4, (offsets, normals): Shown) {
        let inverse = place.inverse();
        let up = inverse.transform_vector3(Vec3::Z);
        let Some(up_direction) = up.try_normalize() else { return };
        let ankles: [Option<Vec3>; 2] = std::array::from_fn(|side| posed_origin(bones, self.legs[side].foot));
        let body = offsets[0].min(offsets[1]).min(0.0);
        if body < 0.0 {
            let shift = Mat4::from_translation(up * body);
            bones.iter_mut().for_each(|bone| *bone = shift * *bone);
        }
        for (side, leg) in self.legs.iter().enumerate() {
            let Some(ankle) = ankles[side] else { continue };
            let normal = inverse.transform_vector3(normals[side]).normalize_or(up_direction);
            solve_leg(bones, leg, ankle + up * offsets[side], Quat::from_rotation_arc(up_direction, normal));
        }
    }
}

fn ease((offsets, normals): Shown, (to_offsets, to_normals): Shown, blend: f32) -> Shown {
    let offsets = std::array::from_fn(|side| {
        let offset = offsets[side] + (to_offsets[side] - offsets[side]) * blend;
        if offset.abs() < SETTLED && to_offsets[side] == 0.0 { 0.0 } else { offset }
    });
    let normals = std::array::from_fn(|side| {
        let normal = normals[side].lerp(to_normals[side], blend).normalize_or(Vec3::Z);
        if normal.dot(to_normals[side]) > 0.9999 && to_normals[side] == Vec3::Z { Vec3::Z } else { normal }
    });
    (offsets, normals)
}

/// The ragdoll starts from the uncorrected pose; the correction fades from a standstill so the body never rises
/// faster than it starts to fall.
fn hand_over((offsets, normals): Shown, age: f32) -> Shown {
    let t = (age / HAND_OVER).clamp(0.0, 1.0);
    let left = 1.0 - t * t * (3.0 - 2.0 * t);
    if left <= 0.0 {
        return REST;
    }
    (offsets.map(|offset| offset * left), normals.map(|normal| Vec3::Z.lerp(normal, left).normalize_or(Vec3::Z)))
}

fn plant(lift: f32) -> f32 {
    1.0 - ((lift - PLANT_FULL) / (PLANT_NONE - PLANT_FULL)).clamp(0.0, 1.0)
}

fn foot_offset(ground: f32, floor: f32, weight: f32) -> f32 {
    (ground - floor).clamp(-STEP, STEP) * weight
}

fn limit_tilt(normal: Vec3, weight: f32) -> Vec3 {
    let angle = Vec3::Z.angle_between(normal);
    if angle < 1e-4 || weight <= 0.0 {
        return Vec3::Z;
    }
    let turn = Quat::from_rotation_arc(Vec3::Z, normal);
    Quat::IDENTITY.slerp(turn, weight * angle.min(MAX_TILT) / angle) * Vec3::Z
}

/// The knee for a thigh and calf of unchanged length whose ankle reaches toward `target`, bent on the side it bent
/// before, and the point the ankle reaches (the target, or the nearest the leg allows).
fn knee_for(hip: Vec3, knee: Vec3, ankle: Vec3, target: Vec3) -> Option<(Vec3, Vec3)> {
    let (upper, lower) = (hip.distance(knee), knee.distance(ankle));
    if upper < 0.1 || lower < 0.1 {
        return None;
    }
    let toward = (target - hip).try_normalize()?;
    let reach = hip.distance(target).clamp((upper - lower).abs() + 0.01, upper + lower - 0.01);
    let along = (ankle - hip).normalize_or(toward);
    let pole = (knee - hip) - along * (knee - hip).dot(along);
    let bend = (pole - toward * pole.dot(toward)).normalize_or(toward.any_orthonormal_vector());
    let cos = ((upper * upper + reach * reach - lower * lower) / (2.0 * upper * reach)).clamp(-1.0, 1.0);
    Some((hip + toward * (upper * cos) + bend * (upper * (1.0 - cos * cos).sqrt()), hip + toward * reach))
}

/// Two-bone solve on world (mesh-space) bone matrices; the foot keeps its animated turn, then `tilt`.
fn solve_leg(bones: &mut [Mat4], leg: &Leg, target: Vec3, tilt: Quat) {
    let (Some(hip), Some(knee), Some(ankle)) = (posed_origin(bones, leg.thigh), posed_origin(bones, leg.calf), posed_origin(bones, leg.foot)) else { return };
    let Some((new_knee, reached)) = knee_for(hip, knee, ankle, target) else { return };
    let thigh_turn = Quat::from_rotation_arc((knee - hip).normalize(), (new_knee - hip).normalize());
    turn_about(bones, &leg.thigh_tree, hip, thigh_turn);
    let Some(ankle) = posed_origin(bones, leg.foot) else { return };
    let (Some(from), Some(to)) = ((ankle - new_knee).try_normalize(), (reached - new_knee).try_normalize()) else { return };
    let calf_turn = Quat::from_rotation_arc(from, to);
    turn_about(bones, &leg.calf_tree, new_knee, calf_turn);
    turn_about(bones, &leg.foot_tree, reached, tilt * (calf_turn * thigh_turn).inverse());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> (Vec<Mat4>, Leg) {
        let parents = [0, 0, 1, 2];
        let bones = [Vec3::new(0.0, 0.0, 100.0), Vec3::new(0.0, 0.0, 90.0), Vec3::new(5.0, 0.0, 50.0), Vec3::new(0.0, 0.0, 10.0)]
            .map(Mat4::from_translation)
            .to_vec();
        let leg = Leg {
            thigh: 1,
            calf: 2,
            foot: 3,
            thigh_tree: subtree(&parents, Some(1)),
            calf_tree: subtree(&parents, Some(2)),
            foot_tree: subtree(&parents, Some(3)),
        };
        (bones, leg)
    }

    fn at(bones: &[Mat4], bone: usize) -> Vec3 {
        bones[bone].w_axis.truncate()
    }

    #[test]
    fn a_raised_foot_reaches_its_target_with_the_bones_unchanged_in_length() {
        let (mut bones, leg) = chain();
        let lengths = |bones: &[Mat4]| (at(bones, 1).distance(at(bones, 2)), at(bones, 2).distance(at(bones, 3)));
        let before = lengths(&bones);
        let target = Vec3::new(0.0, 0.0, 22.0);
        solve_leg(&mut bones, &leg, target, Quat::IDENTITY);
        let after = lengths(&bones);
        assert!(at(&bones, 3).distance(target) < 1e-3, "{:?}", at(&bones, 3));
        assert!((before.0 - after.0).abs() < 1e-3 && (before.1 - after.1).abs() < 1e-3);
        assert!(at(&bones, 2).x > 5.0, "the knee bends further the way it bent");
        assert_eq!(at(&bones, 0), Vec3::new(0.0, 0.0, 100.0));
    }

    #[test]
    fn a_target_out_of_reach_straightens_the_leg_toward_it() {
        let (mut bones, leg) = chain();
        let full = at(&bones, 1).distance(at(&bones, 2)) + at(&bones, 2).distance(at(&bones, 3));
        solve_leg(&mut bones, &leg, Vec3::new(0.0, 0.0, -200.0), Quat::IDENTITY);
        let foot = at(&bones, 3);
        assert!((at(&bones, 1).distance(foot) - full).abs() < 0.05, "{foot:?}");
        assert!(foot.x.abs() < 0.5);
    }

    #[test]
    fn the_foot_keeps_its_turn_and_takes_the_tilt() {
        let (mut bones, leg) = chain();
        let tilt = Quat::from_rotation_x(0.2);
        solve_leg(&mut bones, &leg, Vec3::new(0.0, 0.0, 20.0), tilt);
        let (_, rotation, _) = bones[3].to_scale_rotation_translation();
        assert!(rotation.angle_between(tilt) < 1e-3, "{rotation:?}");
    }

    #[test]
    fn offsets_stop_at_the_step_height_and_a_lifted_foot_is_left_alone() {
        assert_eq!(foot_offset(100.0, 0.0, 1.0), STEP);
        assert_eq!(foot_offset(-100.0, 0.0, 1.0), -STEP);
        assert_eq!(foot_offset(10.0, 4.0, 1.0), 6.0);
        assert_eq!(plant(0.0), 1.0);
        assert_eq!(plant(PLANT_NONE + 1.0), 0.0);
        assert_eq!(foot_offset(10.0, 0.0, plant(PLANT_NONE)), 0.0);
    }

    #[test]
    fn a_steep_ground_turns_the_foot_by_at_most_the_limit() {
        let steep = Vec3::new(1.0, 0.0, 0.2).normalize();
        assert!(Vec3::Z.angle_between(limit_tilt(steep, 1.0)) <= MAX_TILT + 1e-4);
        assert_eq!(limit_tilt(steep, 0.0), Vec3::Z);
        assert_eq!(limit_tilt(Vec3::Z, 1.0), Vec3::Z);
    }

    #[test]
    fn a_body_lying_down_is_not_standing() {
        let (standing, _) = chain();
        let rig = Rig { legs: [chain().1, chain().1], sole: Vec3::ZERO, bind_ankles: [Vec3::new(0.0, 0.0, 10.0); 2], bind_hips: [Vec3::new(0.0, 0.0, 90.0); 2] };
        assert!(rig.standing(&standing, Mat4::IDENTITY));
        let lying: Vec<Mat4> = standing.iter().map(|bone| Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2) * *bone).collect();
        assert!(!rig.standing(&lying, Mat4::IDENTITY));
    }

    #[test]
    fn the_correction_settles_back_to_rest() {
        let mut shown = ([10.0, -8.0], [Vec3::new(0.3, 0.0, 1.0).normalize(); 2]);
        for _ in 0..200 {
            shown = ease(shown, REST, 0.25);
        }
        assert!(shown == REST);
    }

    #[test]
    fn a_ragdoll_takes_over_the_correction_without_a_jump_and_loses_it_by_the_end() {
        let from = ([-10.0, -4.0], [Vec3::new(0.2, 0.0, 1.0).normalize(); 2]);
        let first = hand_over(from, 0.0);
        assert!((first.0[0] - from.0[0]).abs() < 1e-5 && first.1[0].distance(from.1[0]) < 1e-5, "{first:?}");
        let next = hand_over(from, 1.0 / 60.0);
        assert!((next.0[0] - from.0[0]).abs() < 0.02 * from.0[0].abs(), "{next:?}");
        assert!(ease(from, REST, 1.0 - (-BLEND_RATE / 60.0f32).exp()).0[0] > next.0[0]);
        assert!(hand_over(from, HAND_OVER) == REST);
        assert!(hand_over(REST, 0.0) == REST && hand_over(REST, 0.2) == REST);
    }
}
