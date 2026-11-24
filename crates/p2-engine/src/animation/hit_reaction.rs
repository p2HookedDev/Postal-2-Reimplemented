use bevy::app::{App, Plugin, Update};
use bevy::ecs::prelude::{Res, ResMut, Resource};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::math::{Mat4, Quat, Vec3};
use bevy::time::Time;
use p2_assets::pose::subtree;
use p2_vm::{ObjectId, PersonHit};

use super::drawn_pose::{Layer, apply_layers, base_pose, mesh_place, posed_origin, turn_about};
use super::foot_placement::{Feet, SIDES, place_feet};
use super::{Animated, AnimatedMesh, AnimationSystems, Poses, animate};
use p2_host::display::{MODERN_HIT_REACTIONS, Settings, modern_on, modern_turned_off};
use crate::play::{Play, PlayWorld};

const STIFFNESS: f32 = 256.0;
const DAMPING: f32 = 24.0;
const SUBSTEP: f32 = 1.0 / 240.0;
const LONGEST_STEP: f32 = 0.1;
const KICK: f32 = 4.5;
const MAX_ANGLE: f32 = 0.2;
/// The momentum of a full kick (unverified against the weapons' values); a lighter hit still kicks this share.
const FULL_MOMENTUM: f32 = 50_000.0;
const LIGHTEST: f32 = 0.3;
const CHAIN: usize = 4;
const FALLOFF: f32 = 0.5;
const REACH: f32 = 48.0;
const LIFE: f32 = 1.0;
const SETTLED: f32 = 1e-3;

pub(super) struct HitReactionPlugin;

impl Plugin for HitReactionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Flinches>().add_systems(
            Update,
            (
                flinch.run_if(modern_on(MODERN_HIT_REACTIONS)).after(place_feet),
                (|mut flinches: ResMut<Flinches>, mut poses: ResMut<Poses>| flinches.layer.drop_all(&mut poses)).run_if(modern_turned_off(MODERN_HIT_REACTIONS)),
            )
                .after(animate)
                .before(apply_layers)
                .in_set(AnimationSystems::Pose),
        );
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Spring {
    angle: Vec3,
    speed: Vec3,
}

impl Spring {
    fn kick(&mut self, speed: Vec3, reach: f32) {
        self.speed = (self.speed + speed).clamp_length_max(2.0 * KICK * reach);
    }

    fn step(&mut self, seconds: f32, reach: f32) {
        let seconds = seconds.min(LONGEST_STEP);
        let steps = (seconds / SUBSTEP).ceil().max(1.0);
        let h = seconds / steps;
        for _ in 0..steps as u32 {
            self.speed -= (self.angle * STIFFNESS + self.speed * DAMPING) * h;
            self.angle = (self.angle + self.speed * h).clamp_length_max(2.0 * MAX_ANGLE * reach);
        }
    }

    fn shown(&self, reach: f32) -> Vec3 {
        let (angle, limit) = (self.angle.length(), MAX_ANGLE * reach);
        if angle < 1e-6 || limit <= 0.0 {
            return self.angle;
        }
        self.angle * (limit * (angle / limit).tanh() / angle)
    }

    fn settled(&self) -> bool {
        self.angle.length() < SETTLED && self.speed.length() < SETTLED * 10.0
    }
}

struct Bone {
    index: usize,
    tree: Vec<bool>,
    share: f32,
    spring: Spring,
}

#[derive(Default)]
struct Flinch {
    bones: Vec<Bone>,
    age: f32,
}

#[derive(Resource, Default)]
pub(super) struct Flinches {
    layer: Layer<Flinch>,
    strength: f32,
}

impl Flinches {
    pub(super) fn apply(&self, actor: ObjectId, bones: &mut [Mat4]) -> bool {
        let Some(person) = self.layer.people.get(&actor) else { return false };
        turn_bones(bones, &person.bones, self.strength);
        true
    }
}

fn flinch(
    time: Res<Time>,
    settings: Res<Settings>,
    play: Res<Play>,
    animated: Res<Animated>,
    feet: Res<Feet>,
    mut poses: ResMut<Poses>,
    mut flinches: ResMut<Flinches>,
) {
    let poses = &mut *poses;
    let flinches = &mut *flinches;
    let Some(world) = play.world() else {
        flinches.layer.drop_all(poses);
        return;
    };
    flinches.layer.restarted(play.generation());
    if flinches.layer.people.is_empty() && world.person_hits().is_empty() {
        return;
    }
    let strength = f32::from(settings.modern_level(MODERN_HIT_REACTIONS)) / 10.0;
    flinches.strength = strength;
    for hit in world.person_hits().iter().filter(|hit| hit.alive) {
        let Some(item) = animated.0.iter().find(|item| item.actor == Some(hit.person)) else { continue };
        let Some(base) = base_pose(poses, hit.person) else { continue };
        let mut standing = base.clone();
        feet.apply(hit.person, &mut standing);
        let kicks = kicks(world, item, &standing, hit, strength);
        if kicks.is_empty() {
            continue;
        }
        let person = flinches.layer.people.entry(hit.person).or_default();
        person.age = 0.0;
        for (index, speed, share) in kicks {
            let at = person.bones.binary_search_by_key(&index, |bone| bone.index).unwrap_or_else(|at| {
                person.bones.insert(at, Bone { index, tree: subtree(&item.parents, Some(index)), share, spring: Spring::default() });
                at
            });
            let bone = &mut person.bones[at];
            bone.share = bone.share.max(share);
            bone.spring.kick(speed, strength * bone.share);
        }
    }
    let seconds = world.game_seconds(time.delta_secs());
    flinches.layer.people.retain(|&actor, person| {
        person.age += seconds;
        person.bones.iter_mut().for_each(|bone| bone.spring.step(seconds, strength * bone.share));
        if !poses.bones.contains_key(&actor) {
            return false;
        }
        let settled = person.age > LIFE || person.bones.iter().all(|bone| bone.spring.settled());
        if settled || seconds > 0.0 || poses.changed.contains(&actor) {
            poses.relayer.insert(actor);
        }
        !settled
    });
}

fn kicks(world: &PlayWorld, item: &AnimatedMesh, bones: &[Mat4], hit: &PersonHit, strength: f32) -> Vec<(usize, Vec3, f32)> {
    let Some(place) = mesh_place(world, item, hit.person) else { return Vec::new() };
    let (at, momentum) = (Vec3::from(<[f32; 3]>::from(hit.at)), Vec3::from(<[f32; 3]>::from(hit.momentum)));
    let Some(push) = momentum.try_normalize() else { return Vec::new() };
    let pivot = |bone: usize| bones.get(bone).map(|matrix| place.transform_point3(matrix.w_axis.truncate()));
    let nearest = (1..bones.len()).filter_map(|bone| Some((bone, pivot(bone)?.distance(at)))).min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((bone, _)) = nearest.filter(|&(_, distance)| distance <= REACH) else { return Vec::new() };
    let speed = KICK * strength * (momentum.length() / FULL_MOMENTUM).clamp(LIGHTEST, 1.0);
    let feet: Vec<usize> = SIDES.iter().filter_map(|[.., foot]| item.bone_names.iter().position(|name| name.ends_with(foot))).collect();
    bone_chain(&item.parents, &feet, bone)
        .into_iter()
        .filter_map(|(bone, share)| {
            let axis = (at - pivot(bone)?).cross(push).try_normalize().unwrap_or_else(|| Vec3::Z.cross(push).normalize_or_zero());
            Some((bone, mesh_axis(place, axis) * speed * share, share))
        })
        .collect()
}

/// From the hit bone up its parents with each one's share. A bone holding a foot (a leg, the pelvis) or hanging
/// from the root is never turned: that would lift the planted feet and swing the whole body.
fn bone_chain(parents: &[usize], feet: &[usize], mut bone: usize) -> Vec<(usize, f32)> {
    let mut chain = Vec::new();
    let mut share = 1.0;
    while let Some(&parent) = parents.get(bone)
        && bone != 0
        && parent != 0
        && chain.len() < CHAIN
    {
        let tree = subtree(parents, Some(bone));
        if feet.iter().any(|&foot| tree.get(foot).copied().unwrap_or_default()) {
            break;
        }
        chain.push((bone, share));
        share *= FALLOFF;
        bone = parent;
    }
    chain
}

fn mesh_axis(place: Mat4, axis: Vec3) -> Vec3 {
    (place.inverse().transform_vector3(axis) * place.determinant().signum()).normalize_or_zero()
}

fn turn_bones(bones: &mut [Mat4], turned: &[Bone], strength: f32) {
    for bone in turned {
        let Some(pivot) = posed_origin(bones, bone.index) else { continue };
        turn_about(bones, &bone.tree, pivot, Quat::from_scaled_axis(bone.spring.shown(strength * bone.share)));
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
    fn a_kick_turns_the_bone_a_few_degrees_and_settles_within_half_a_second() {
        let mut spring = Spring::default();
        spring.kick(Vec3::X * KICK, 1.0);
        run(&mut spring, 0.07, 1.0);
        let peak = spring.shown(1.0).x;
        assert!(peak > 0.08 && peak < 0.15, "{spring:?}");
        run(&mut spring, 0.43, 1.0);
        assert!(spring.angle.length() < 0.01, "{spring:?}");
        run(&mut spring, 0.3, 1.0);
        assert!(spring.settled(), "{spring:?}");
    }

    #[test]
    fn the_return_passes_rest_at_most_once_and_only_a_little() {
        let mut spring = Spring::default();
        spring.kick(Vec3::X * KICK, 1.0);
        let (mut lowest, mut peak, mut back) = (0.0f32, 0.0f32, 0.0f32);
        for _ in 0..120 {
            spring.step(1.0 / 120.0, 1.0);
            let now = spring.angle.x;
            if lowest < 0.0 {
                back = back.max(now);
            }
            (lowest, peak) = (lowest.min(now), peak.max(now));
        }
        assert!(-lowest < 0.05 * peak, "{lowest} {peak}");
        assert!(back < 0.002 * peak, "{back} {peak}");
    }

    #[test]
    fn hits_add_up_and_the_drawn_turn_stays_below_the_limit() {
        let strength = 1.0;
        let mut spring = Spring::default();
        let mut single = Spring::default();
        single.kick(Vec3::Y * KICK * strength, strength);
        run(&mut single, 0.05, strength);
        for _ in 0..30 {
            spring.kick(Vec3::Y * KICK * strength, strength);
            assert!(spring.speed.length() <= 2.0 * KICK * strength + 1e-4);
            spring.step(1.0 / 120.0, strength);
            assert!(spring.shown(strength).length() < MAX_ANGLE * strength, "{spring:?}");
        }
        assert!(spring.angle.y > single.angle.y, "{spring:?} {single:?}");
        let half = Spring { angle: Vec3::Y * MAX_ANGLE, speed: Vec3::ZERO };
        assert!(half.shown(0.5).length() < MAX_ANGLE * 0.5);
        let small = Spring { angle: Vec3::Y * 0.01, speed: Vec3::ZERO };
        assert!((small.shown(1.0).y - 0.01).abs() < 1e-4);
    }

    #[test]
    fn the_chain_climbs_the_spine_and_never_turns_the_pelvis_or_a_leg() {
        let parents = [0, 0, 1, 2, 3, 4, 1, 6, 7];
        let feet = [8];
        assert_eq!(bone_chain(&parents, &feet, 5), vec![(5, 1.0), (4, FALLOFF), (3, FALLOFF * FALLOFF), (2, FALLOFF * FALLOFF * FALLOFF)]);
        assert_eq!(bone_chain(&parents, &feet, 3), vec![(3, 1.0), (2, FALLOFF)]);
        assert!(bone_chain(&parents, &feet, 7).is_empty());
        assert!(bone_chain(&parents, &feet, 1).is_empty());
    }

    #[test]
    fn a_mirrored_mesh_turns_the_way_the_world_does() {
        let place = Mat4::from_rotation_z(0.7) * Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
        let (axis, point) = (Vec3::new(0.3, 0.5, 0.8).normalize(), Vec3::new(4.0, 1.0, 2.0));
        let in_world = Quat::from_axis_angle(axis, 0.3) * place.transform_point3(point);
        let in_mesh = place.transform_point3(Quat::from_axis_angle(mesh_axis(place, axis), 0.3) * point);
        assert!(in_world.distance(in_mesh) < 1e-4, "{in_world} {in_mesh}");
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
        turn_bones(&mut bones, &[Bone { index: 1, tree: subtree(&parents, Some(1)), share: 1.0, spring }], 1000.0);
        assert_eq!(bones[0], Mat4::IDENTITY);
        assert!(bones[1].w_axis.truncate().distance(Vec3::new(0.0, 0.0, 50.0)) < 1e-4);
        assert!(bones[2].w_axis.truncate().distance(Vec3::new(0.0, -30.0, 50.0)) < 1e-3, "{}", bones[2]);
    }
}
