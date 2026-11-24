//! What the pose corrections share. They change the drawn pose in `Poses::bones` only; the game is told about bones
//! from the animation's own pose, kept in `Poses::animated` before the first change.

use bevy::math::{Mat4, Quat, Vec3};
use p2_vm::ObjectId;
use rustc_hash::FxHashMap;

use super::{AnimatedMesh, Poses};
use p2_render::components::FollowActor;
use crate::play::PlayWorld;

/// The mesh's frame in the world for a person the script animates and no ragdoll holds.
pub(super) fn mesh_place(world: &PlayWorld, item: &AnimatedMesh, actor: ObjectId) -> Option<Mat4> {
    let frame = world.actor(actor).filter(|frame| !frame.hidden && !frame.gone)?;
    if world.ragdolled(actor) || !matches!(world.pose(actor), Some(Some(_))) {
        return None;
    }
    let (location, rotation) = world.place(actor)?;
    let mesh = &item.mesh;
    let follow = FollowActor { actor, scale: frame.scale, pre_pivot: frame.pre_pivot, skeletal: Some((mesh.scale, mesh.origin, mesh.rot_origin)) };
    follow.mesh_matrix(location, rotation)
}

/// Called before writing a corrected pose over `pose`: while no copy is kept, `pose` is still the animation's.
pub(super) fn keep_for_game(animated: &mut FxHashMap<ObjectId, Vec<Mat4>>, actor: ObjectId, pose: &[Mat4]) {
    animated.entry(actor).or_insert_with(|| pose.to_vec());
}

pub(super) fn told_bone(animated: &FxHashMap<ObjectId, Vec<Mat4>>, actor: ObjectId, bone: usize, drawn: Mat4) -> Mat4 {
    animated.get(&actor).and_then(|pose| pose.get(bone)).copied().unwrap_or(drawn)
}

/// Puts `pose` back unless the animation posed the actor again this frame.
pub(super) fn restore(poses: &mut Poses, actor: ObjectId, pose: Vec<Mat4>) {
    if !poses.changed.contains(&actor)
        && let Some(bones) = poses.bones.get_mut(&actor)
    {
        *bones = pose;
        poses.changed.insert(actor);
    }
}

pub(super) fn posed_origin(bones: &[Mat4], bone: usize) -> Option<Vec3> {
    Some(bones.get(bone)?.w_axis.truncate())
}

pub(super) fn turn_about(bones: &mut [Mat4], tree: &[bool], pivot: Vec3, rotation: Quat) {
    let about = Mat4::from_translation(pivot) * Mat4::from_quat(rotation) * Mat4::from_translation(-pivot);
    for (bone, _) in bones.iter_mut().zip(tree).filter(|(_, inside)| **inside) {
        *bone = about * *bone;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_game_keeps_the_first_pose_and_reads_it_before_the_drawn_one() {
        let actor = ObjectId::from_index(1);
        let (animated, corrected) = (Mat4::from_translation(Vec3::Z), Mat4::from_translation(Vec3::X));
        let mut kept = FxHashMap::default();
        assert_eq!(told_bone(&kept, actor, 0, corrected), corrected);
        keep_for_game(&mut kept, actor, &[animated]);
        keep_for_game(&mut kept, actor, &[corrected]);
        assert_eq!(told_bone(&kept, actor, 0, corrected), animated);
        assert_eq!(told_bone(&kept, actor, 5, corrected), corrected);
    }

    #[test]
    fn a_turn_moves_only_the_marked_bones() {
        let mut bones = [Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0)].map(Mat4::from_translation);
        turn_about(&mut bones, &[false, true], Vec3::ZERO, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2));
        assert_eq!(bones[0], Mat4::IDENTITY);
        assert!(bones[1].w_axis.truncate().distance(Vec3::new(0.0, -10.0, 0.0)) < 1e-4, "{}", bones[1]);
    }
}
