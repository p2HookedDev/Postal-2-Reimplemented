//! A person flinches where a hit lands: a damped spring turn on the hit bone and, weaker, up its parents, added to the
//! drawn pose after the animation and before skinning. Nothing the game reads changes.

use bevy::app::{App, Plugin, Update};
use bevy::ecs::prelude::{Local, Res, ResMut};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Mat4, Quat, Vec3};
use bevy::time::Time;
use p2_assets::pose::subtree;
use p2_vm::{ObjectId, PersonHit};
use rustc_hash::FxHashMap;

use super::drawn_pose::{keep_for_game, mesh_place, posed_origin, restore, turn_about};
use super::foot_placement::place_feet;
use super::{Animated, AnimatedMesh, AnimationSystems, Poses, pose_joints};
use crate::display::{MODERN_HIT_REACTIONS, Settings};
use crate::play::{Play, PlayWorld};

/// A natural frequency of 16 per second at three quarters of critical damping: settled in about 0.45 seconds.
const STIFFNESS: f32 = 256.0;
const DAMPING: f32 = 24.0;
const SUBSTEP: f32 = 1.0 / 240.0;
const LONGEST_STEP: f32 = 0.1;
/// Radians per second at level 10: a full hit peaks near a fifth of a radian.
const KICK: f32 = 8.0;
const MAX_ANGLE: f32 = 0.45;
/// The momentum of a full kick (unverified against the weapons' values); a lighter hit still kicks this share.
const FULL_MOMENTUM: f32 = 50_000.0;
const LIGHTEST: f32 = 0.3;
const CHAIN: usize = 4;
const FALLOFF: f32 = 0.5;
/// Unreal units; a hit farther than this from every bone has no place on the body.
const REACH: f32 = 48.0;
const LIFE: f32 = 1.0;
const SETTLED: f32 = 1e-3;

pub(super) struct HitReactionPlugin;

impl Plugin for HitReactionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, flinch.after(place_feet).before(pose_joints).in_set(AnimationSystems::Pose));
    }
}

/// A turn as axis times angle in mesh space, and its rate.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Spring {
    angle: Vec3,
    speed: Vec3,
}

impl Spring {
    fn kick(&mut self, speed: Vec3, strength: f32) {
        self.speed = (self.speed + speed).clamp_length_max(2.0 * KICK * strength);
    }

    fn step(&mut self, seconds: f32, strength: f32) {
        let seconds = seconds.min(LONGEST_STEP);
        let steps = (seconds / SUBSTEP).ceil().max(1.0);
        let h = seconds / steps;
        for _ in 0..steps as u32 {
            self.speed -= (self.angle * STIFFNESS + self.speed * DAMPING) * h;
            self.angle = (self.angle + self.speed * h).clamp_length_max(MAX_ANGLE * strength);
        }
    }

    fn settled(&self) -> bool {
        self.angle.length() < SETTLED && self.speed.length() < SETTLED * 10.0
    }
}

struct Bone {
    index: usize,
    tree: Vec<bool>,
    spring: Spring,
}

struct Flinch {
    /// The pose without the flinch.
    base: Vec<Mat4>,
    /// By bone index, so a parent turns before its children.
    bones: Vec<Bone>,
    age: f32,
}

#[derive(Default)]
struct Flinches {
    people: FxHashMap<ObjectId, Flinch>,
    generation: u64,
}

fn flinch(
    time: Res<Time>,
    settings: Option<Res<Settings>>,
    play: Res<Play>,
    animated: Res<Animated>,
    mut poses: ResMut<Poses>,
    mut flinches: Local<Flinches>,
) {
    let poses = &mut *poses;
    let flinches = &mut *flinches;
    let level = settings.map_or(0, |settings| settings.modern_level(MODERN_HIT_REACTIONS));
    let (true, Some(world)) = (level > 0, play.world()) else {
        for (actor, person) in flinches.people.drain() {
            restore(poses, actor, person.base);
        }
        return;
    };
    if flinches.generation != play.generation() {
        flinches.generation = play.generation();
        flinches.people.clear();
    }
    if flinches.people.is_empty() && world.person_hits().is_empty() {
        return;
    }
    let strength = f32::from(level) / 10.0;
    flinches.people.retain(|actor, person| {
        let Some(bones) = poses.bones.get(actor).filter(|_| !world.ragdolled(*actor)) else { return false };
        if poses.changed.contains(actor) {
            person.base.clone_from(bones);
        }
        true
    });
    for hit in world.person_hits().iter().filter(|hit| hit.alive) {
        let Some(item) = animated.0.iter().find(|item| item.actor == Some(hit.person)) else { continue };
        let known = flinches.people.get(&hit.person).map(|person| &person.base);
        let Some(bones) = known.or_else(|| poses.bones.get(&hit.person)) else { continue };
        let kicks = kicks(world, item, bones, hit, strength);
        if kicks.is_empty() {
            continue;
        }
        let base = bones.clone();
        let person = flinches.people.entry(hit.person).or_insert_with(|| Flinch { base, bones: Vec::new(), age: 0.0 });
        person.age = 0.0;
        for (index, speed) in kicks {
            let at = person.bones.binary_search_by_key(&index, |bone| bone.index).unwrap_or_else(|at| {
                person.bones.insert(at, Bone { index, tree: subtree(&item.parents, Some(index)), spring: Spring::default() });
                at
            });
            person.bones[at].spring.kick(speed, strength);
        }
    }
    let seconds = if world.game_paused() { 0.0 } else { time.delta_secs() * world.time_dilation() };
    flinches.people.retain(|&actor, person| {
        person.age += seconds;
        person.bones.iter_mut().for_each(|bone| bone.spring.step(seconds, strength));
        let Some(bones) = poses.bones.get_mut(&actor) else { return false };
        let fresh = poses.changed.contains(&actor);
        if person.age > LIFE || person.bones.iter().all(|bone| bone.spring.settled()) {
            if !fresh {
                *bones = std::mem::take(&mut person.base);
                poses.changed.insert(actor);
            }
            return false;
        }
        if seconds > 0.0 || fresh {
            keep_for_game(&mut poses.animated, actor, bones);
            bones.clone_from(&person.base);
            turn_bones(bones, &person.bones);
            poses.changed.insert(actor);
        }
        true
    });
}

/// The bones a hit kicks, from the one nearest it up its parents, each with its turn rate in mesh space.
fn kicks(world: &PlayWorld, item: &AnimatedMesh, bones: &[Mat4], hit: &PersonHit, strength: f32) -> Vec<(usize, Vec3)> {
    let mut kicks = Vec::new();
    let Some(place) = mesh_place(world, item, hit.person) else { return kicks };
    let (at, momentum) = (Vec3::from(<[f32; 3]>::from(hit.at)), Vec3::from(<[f32; 3]>::from(hit.momentum)));
    let Some(push) = momentum.try_normalize() else { return kicks };
    let pivot = |bone: usize| bones.get(bone).map(|matrix| place.transform_point3(matrix.w_axis.truncate()));
    let nearest = (1..bones.len()).filter_map(|bone| Some((bone, pivot(bone)?.distance(at)))).min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((mut bone, _)) = nearest.filter(|&(_, distance)| distance <= REACH) else { return kicks };
    let inverse = place.inverse();
    let mut speed = KICK * strength * (momentum.length() / FULL_MOMENTUM).clamp(LIGHTEST, 1.0);
    while bone != 0 && kicks.len() < CHAIN {
        let Some(from) = pivot(bone) else { break };
        let axis = (at - from).cross(push).try_normalize().unwrap_or_else(|| Vec3::Z.cross(push).normalize_or_zero());
        kicks.push((bone, inverse.transform_vector3(axis).normalize_or_zero() * speed));
        speed *= FALLOFF;
        bone = item.parents.get(bone).copied().unwrap_or(0);
    }
    kicks
}

fn turn_bones(bones: &mut [Mat4], turned: &[Bone]) {
    for bone in turned {
        let Some(pivot) = posed_origin(bones, bone.index) else { continue };
        turn_about(bones, &bone.tree, pivot, Quat::from_scaled_axis(bone.spring.angle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(spring: &mut Spring, seconds: f32, strength: f32) {
        for _ in 0..(seconds * 60.0).round() as u32 {
            spring.step(1.0 / 60.0, strength);
        }
    }

    #[test]
    fn a_kick_turns_the_bone_and_settles_within_half_a_second() {
        let mut spring = Spring::default();
        spring.kick(Vec3::X * KICK, 1.0);
        run(&mut spring, 0.1, 1.0);
        assert!(spring.angle.x > 0.15, "{spring:?}");
        run(&mut spring, 0.4, 1.0);
        assert!(spring.angle.length() < 0.01, "{spring:?}");
        run(&mut spring, 0.3, 1.0);
        assert!(spring.settled(), "{spring:?}");
    }

    #[test]
    fn hits_add_up_and_stay_within_the_clamp() {
        let strength = 0.5;
        let mut spring = Spring::default();
        let mut single = Spring::default();
        single.kick(Vec3::Y * KICK * strength, strength);
        run(&mut single, 0.05, strength);
        for _ in 0..30 {
            spring.kick(Vec3::Y * KICK * strength, strength);
            assert!(spring.speed.length() <= 2.0 * KICK * strength + 1e-4);
            spring.step(1.0 / 120.0, strength);
            assert!(spring.angle.length() <= MAX_ANGLE * strength + 1e-4, "{spring:?}");
        }
        assert!(spring.angle.y > single.angle.y, "{spring:?} {single:?}");
    }

    #[test]
    fn no_time_no_change() {
        let mut spring = Spring { angle: Vec3::new(0.1, 0.0, 0.0), speed: Vec3::new(0.0, 2.0, 0.0) };
        let before = spring;
        spring.step(0.0, 1.0);
        assert_eq!(spring, before);
    }

    #[test]
    fn a_turn_moves_the_children_about_the_bone_and_leaves_its_parent() {
        let parents = [0, 0, 1];
        let mut bones = [Vec3::ZERO, Vec3::new(0.0, 0.0, 50.0), Vec3::new(0.0, 0.0, 80.0)].map(Mat4::from_translation);
        let spring = Spring { angle: Vec3::X * std::f32::consts::FRAC_PI_2, speed: Vec3::ZERO };
        turn_bones(&mut bones, &[Bone { index: 1, tree: subtree(&parents, Some(1)), spring }]);
        assert_eq!(bones[0], Mat4::IDENTITY);
        assert!(bones[1].w_axis.truncate().distance(Vec3::new(0.0, 0.0, 50.0)) < 1e-4);
        assert!(bones[2].w_axis.truncate().distance(Vec3::new(0.0, -30.0, 50.0)) < 1e-3, "{}", bones[2]);
    }
}
