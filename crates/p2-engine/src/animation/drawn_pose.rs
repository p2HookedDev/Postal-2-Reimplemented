use bevy::ecs::prelude::{Res, ResMut};
use bevy::math::{Mat4, Quat, Vec3};
use p2_vm::ObjectId;
use rustc_hash::FxHashMap;

use super::foot_placement::Feet;
use super::hit_reaction::Flinches;
use super::{AnimatedMesh, Poses};
use p2_render::world::components::FollowActor;
use crate::play::PlayWorld;

pub(super) struct Layer<T> {
    pub(super) people: FxHashMap<ObjectId, T>,
    generation: u64,
}

impl<T> Default for Layer<T> {
    fn default() -> Self {
        Self { people: FxHashMap::default(), generation: 0 }
    }
}

impl<T> Layer<T> {
    pub(super) fn drop_all(&mut self, poses: &mut Poses) {
        poses.relayer.extend(self.people.drain().map(|(actor, _)| actor));
    }

    pub(super) fn restarted(&mut self, generation: u64) -> bool {
        let restarted = std::mem::replace(&mut self.generation, generation) != generation;
        if restarted {
            self.people.clear();
        }
        restarted
    }
}

pub(super) fn base_pose(poses: &Poses, actor: ObjectId) -> Option<&Vec<Mat4>> {
    poses.animated.get(&actor).or_else(|| poses.bones.get(&actor))
}

pub(super) fn apply_layers(feet: Res<Feet>, flinches: Res<Flinches>, mut poses: ResMut<Poses>) {
    let Poses { bones, animated, changed, relayer, .. } = &mut *poses;
    for actor in relayer.drain() {
        let Some(pose) = bones.get_mut(&actor) else { continue };
        let base = keep_for_game(animated, actor, pose);
        pose.clone_from(base);
        let feet_planted = feet.apply(actor, pose);
        if !flinches.apply(actor, pose) && !feet_planted {
            animated.remove(&actor);
        }
        changed.insert(actor);
    }
}

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

fn keep_for_game<'a>(animated: &'a mut FxHashMap<ObjectId, Vec<Mat4>>, actor: ObjectId, pose: &[Mat4]) -> &'a Vec<Mat4> {
    animated.entry(actor).or_insert_with(|| pose.to_vec())
}

pub(super) fn told_bone(animated: &FxHashMap<ObjectId, Vec<Mat4>>, actor: ObjectId, bone: usize, drawn: Mat4) -> Mat4 {
    animated.get(&actor).and_then(|pose| pose.get(bone)).copied().unwrap_or(drawn)
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
        assert_eq!(keep_for_game(&mut kept, actor, &[corrected]), &[animated]);
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

    #[test]
    fn a_restart_forgets_the_people_and_a_drop_marks_them_for_the_step() {
        let (one, two) = (ObjectId::from_index(1), ObjectId::from_index(2));
        let mut layer = Layer::<()>::default();
        layer.people.insert(one, ());
        assert!(!layer.restarted(0));
        assert!(layer.restarted(1));
        assert!(layer.people.is_empty());
        layer.people.insert(two, ());
        let mut poses = Poses::default();
        layer.drop_all(&mut poses);
        assert!(layer.people.is_empty());
        assert!(poses.relayer.contains(&two) && !poses.relayer.contains(&one));
    }
}
