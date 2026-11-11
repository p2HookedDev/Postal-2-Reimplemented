pub use crate::components::FollowBone;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use bevy::app::{App, Plugin, PostUpdate, Update};
use bevy::asset::{Assets, Handle};
use bevy::camera::primitives::Aabb;
use bevy::ecs::change_detection::{DetectChanges, DetectChangesMut};
use bevy::ecs::prelude::{Added, Commands, Entity, Local, Query, Ref, Res, ResMut, Resource, SystemSet, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::camera::visibility::{Visibility, VisibilitySystems, calculate_bounds};
use bevy::transform::TransformSystems;
use bevy::transform::components::{GlobalTransform, Transform};
use bevy::math::{Mat4, Quat, Vec3};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes, entity_aabb_from_skinned_mesh_bounds};
use bevy::mesh::{Mesh, Mesh3d};
use bevy::time::Time;
use p2_math::{Rotator, Vector};
use p2_assets::pose::{blend_local, tween_pose};
use p2_assets::{MeshAnimation, SkeletalMesh};
use p2_bevy_assets::{bone_matrices, scaled_bone_matrices_into, unreal_mesh_matrix, unreal_to_bevy};
use p2_vm::ObjectId;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::mesh_view::{FollowActor, SkinJoint};
use crate::play::{ActorPose, ChannelPose, FrameTimes, Play, PlayWorld};
use attachments::{follow_bones, place_attached};
pub(crate) use attachments::{AttachmentPlaces, BoneFrame, attached_transform};
pub use attachments::attachment_frame;

mod attachments;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnimationSystems {
    Pose,
}

pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        crate::add_once(app, crate::FrameOrderPlugin);
        app.init_resource::<Animated>()
            .init_resource::<Poses>()
            .init_resource::<AttachmentPlaces>()
            .add_systems(Update, (animate, pose_joints, follow_bones, place_attached).chain().in_set(AnimationSystems::Pose))
            .add_systems(
                PostUpdate,
                skinned_bounds.in_set(VisibilitySystems::CalculateBounds).after(TransformSystems::Propagate).after(calculate_bounds),
            );
    }
}

#[derive(Resource, Default)]
pub struct Animated(pub Vec<AnimatedMesh>);

#[derive(Resource, Default)]
struct Poses {
    bones: FxHashMap<ObjectId, Vec<Mat4>>,
    changed: FxHashSet<ObjectId>,
    names: FxHashMap<ObjectId, Arc<[String]>>,
    aliases: FxHashMap<ObjectId, Arc<[(String, usize, Mat4)]>>,
    joints: FxHashMap<ObjectId, Vec<(usize, Entity)>>,
    slots: FxHashMap<ObjectId, Vec<(p2_vm::NameId, usize, Mat4)>>,
    moved: FxHashSet<ObjectId>,
}

impl Poses {
    fn note(&mut self, actor: ObjectId, item: &AnimatedMesh) {
        self.names.entry(actor).or_insert_with(|| Arc::clone(&item.bone_names));
        self.aliases.entry(actor).or_insert_with(|| Arc::clone(&item.aliases));
    }

    fn forget(&mut self, actor: ObjectId) {
        self.bones.remove(&actor);
        self.names.remove(&actor);
        self.aliases.remove(&actor);
        self.joints.remove(&actor);
        self.slots.remove(&actor);
    }
}

#[derive(Clone)]
pub struct AnimatedMesh {
    pub meshes: Vec<Handle<Mesh>>,
    pub mesh: Arc<SkeletalMesh>,
    pub animation: Arc<MeshAnimation>,
    pub sequence: usize,
    pub bone_map: Vec<Option<usize>>,
    pub inverse_bind: Vec<Mat4>,
    pub actor: Option<ObjectId>,
    parents: Arc<[usize]>,
    bone_names: Arc<[String]>,
    aliases: Arc<[(String, usize, Mat4)]>,
    vm_bone_maps: FxHashMap<Arc<str>, Arc<[Option<usize>]>>,
    subtrees: FxHashMap<Arc<str>, Arc<[bool]>>,
    last_pose: Option<u64>,
    bone_indices: FxHashMap<Arc<str>, Option<usize>>,
    bone_scales: Vec<f32>,
    local_scratch: Vec<LocalBone>,
    channel_scratch: Vec<LocalBone>,
}

impl AnimatedMesh {
    pub fn new(
        meshes: Vec<Handle<Mesh>>,
        mesh: Arc<SkeletalMesh>,
        animation: Arc<MeshAnimation>,
        sequence: usize,
        mesh_bone_name: impl Fn(usize) -> String,
        animation_bone_name: impl Fn(usize) -> String,
    ) -> Self {
        let animation_names: Vec<String> =
            (0..animation.bones.len()).map(|i| animation_bone_name(i).to_ascii_lowercase()).collect();
        let bone_names: Arc<[String]> = (0..mesh.bones.len()).map(|i| mesh_bone_name(i).to_ascii_lowercase()).collect();
        let bone_map = map_bones(&bone_names, &animation_names);
        let parents: Arc<[usize]> = mesh.bones.iter().map(|b| b.parent).collect();
        let local: Vec<_> = mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
        let inverse_bind = bone_matrices(&parents, &local).iter().map(Mat4::inverse).collect();
        Self {
            meshes,
            mesh,
            animation,
            sequence,
            bone_map,
            inverse_bind,
            actor: None,
            parents,
            bone_names,
            aliases: Arc::new([]),
            vm_bone_maps: FxHashMap::default(),
            subtrees: FxHashMap::default(),
            last_pose: None,
            bone_indices: FxHashMap::default(),
            bone_scales: Vec::new(),
            local_scratch: Vec::new(),
            channel_scratch: Vec::new(),
        }
    }

    pub fn with_aliases(mut self, name: impl Fn(usize) -> String) -> Self {
        self.aliases = self
            .mesh
            .attach_aliases
            .iter()
            .filter_map(|a| {
                let bone = name(a.bone).to_ascii_lowercase();
                let index = self.bone_names.iter().position(|n| *n == bone)?;
                let [origin, x, y, z] = a.coords.map(Vec3::from_array);
                let tag = Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), origin.extend(1.0));
                Some((name(a.alias).to_ascii_lowercase(), index, tag))
            })
            .collect();
        self
    }

    pub fn bone_pose(&self, frame: f32) -> Vec<Mat4> {
        self.pose_with(&self.animation, self.sequence, frame, &self.bone_map)
    }

    fn pose_with(&self, animation: &MeshAnimation, sequence: usize, frame: f32, bone_map: &[Option<usize>]) -> Vec<Mat4> {
        let mut local = Vec::new();
        self.local_pose(animation, sequence, frame, bone_map, &mut local);
        bone_matrices(&self.parents, &local)
    }

    fn local_pose(&self, animation: &MeshAnimation, sequence: usize, frame: f32, bone_map: &[Option<usize>], out: &mut Vec<LocalBone>) {
        let pose = animation.pose_at(sequence, frame);
        out.clear();
        out.extend(
            self.mesh
                .bones
                .iter()
                .zip(bone_map)
                .map(|(bone, track)| track.and_then(|t| pose.get(t)).unwrap_or((bone.orientation, bone.position))),
        );
    }

    fn bone_index(&mut self, name: &Arc<str>) -> Option<usize> {
        let bone_names = &self.bone_names;
        *self.bone_indices.entry(Arc::clone(name)).or_insert_with(|| bone_names.iter().position(|bone| **bone == **name))
    }

    fn scale_bones(&mut self, scales: &[(Arc<str>, f32)]) {
        self.bone_scales.clear();
        self.bone_scales.resize(self.bone_names.len(), 1.0);
        for (bone, scale) in scales {
            if let Some(index) = self.bone_index(bone)
                && let Some(factor) = self.bone_scales.get_mut(index)
            {
                *factor *= scale;
            }
        }
    }

    fn subtree(&mut self, start: &str) -> Arc<[bool]> {
        if let Some(inside) = self.subtrees.get(start) {
            return Arc::clone(inside);
        }
        let root = self.bone_names.iter().position(|name| name == start).unwrap_or(0);
        let mut inside = vec![false; self.mesh.bones.len()];
        for (bone, b) in self.mesh.bones.iter().enumerate() {
            inside[bone] = bone == root || (b.parent < bone && inside[b.parent]);
        }
        let inside: Arc<[bool]> = inside.into();
        self.subtrees.insert(Arc::from(start), Arc::clone(&inside));
        inside
    }
}

type LocalBone = ([f32; 4], [f32; 3]);

fn frame_matrix(frame: &p2_assets::pose::Frame) -> Mat4 {
    let [x, y, z] = frame.axes.map(Vec3::from);
    Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), Vec3::from(frame.origin).extend(1.0))
}

fn map_bones(bone_names: &[String], animation_names: &[String]) -> Vec<Option<usize>> {
    bone_names.iter().map(|name| animation_names.iter().position(|n| n == name)).collect()
}

/// Fewer animated meshes than this are posed on the calling thread: handing them out costs more than it saves.
const POSED_AT_ONCE: usize = 16;

#[derive(Default)]
struct PoseScratch {
    times: Vec<f32>,
}

enum PoseWork<'a> {
    Keep,
    Ragdoll(std::borrow::Cow<'a, [p2_assets::pose::Frame]>),
    /// The pose, and where its channels' shown times start in the frame's list of them.
    Channels(&'a ActorPose, usize),
    /// No game behind it (the viewer), or an actor that does not animate: its own sequence by the clock.
    Free,
}

/// The mesh's new bone matrices, or `None` while it shows the pose it already has.
fn pose_mesh(item: &mut AnimatedMesh, work: &PoseWork, shown_times: &[f32], elapsed: f32) -> Option<Vec<Mat4>> {
    match work {
        PoseWork::Keep => None,
        PoseWork::Ragdoll(bones) => {
            let mut key = rustc_hash::FxHasher::default();
            bones.iter().for_each(|b| b.axes.iter().chain([&b.origin]).flatten().for_each(|c| c.to_bits().hash(&mut key)));
            let key = Some(key.finish());
            if key == item.last_pose {
                return None;
            }
            item.last_pose = key;
            Some(bones.iter().map(frame_matrix).collect())
        }
        PoseWork::Channels(actor_pose, from) => {
            let ActorPose { scales, turns, .. } = actor_pose;
            let shown = |index: usize, channel: &ChannelPose| shown_times.get(from + index).copied().unwrap_or(channel.time);
            let mut key = std::hash::DefaultHasher::new();
            for (index, (channel, alpha, bone)) in pose_channels(actor_pose).enumerate() {
                (Arc::as_ptr(&channel.animation), channel.sequence, shown(index, channel).to_bits(), alpha.to_bits(), bone).hash(&mut key);
                channel.tween.as_ref().map(|(from, weight)| (Arc::as_ptr(from).addr(), weight.to_bits())).hash(&mut key);
            }
            scales.iter().for_each(|(bone, scale)| (&**bone, scale.to_bits()).hash(&mut key));
            turns.iter().for_each(|(bone, turn)| (&**bone, turn).hash(&mut key));
            let key = Some(key.finish());
            if key == item.last_pose {
                return None;
            }
            item.last_pose = key;
            let mut local = std::mem::take(&mut item.local_scratch);
            let mut channel_pose = std::mem::take(&mut item.channel_scratch);
            local.clear();
            for (index, (channel, alpha, bone)) in pose_channels(actor_pose).enumerate() {
                let map = Arc::clone(item.vm_bone_maps.entry(Arc::clone(&channel.animation.path)).or_insert_with(|| map_bones(&item.bone_names, &channel.animation.bone_names).into()));
                let pose = if index == 0 { &mut local } else { &mut channel_pose };
                item.local_pose(&channel.animation.animation, channel.sequence, shown(index, channel), &map, pose);
                if let Some((from, weight)) = &channel.tween {
                    tween_pose(from, pose, *weight);
                }
                if index > 0 {
                    blend_local(&mut local, &channel_pose, alpha, &item.subtree(bone));
                }
            }
            item.scale_bones(scales);
            for (bone, turn) in turns {
                if let Some(index) = item.bone_index(bone)
                    && let Some(posed) = local.get_mut(index)
                {
                    posed.0 = p2_assets::pose::turn_local(posed.0, *turn);
                }
            }
            let mut pose = Vec::new();
            let factors = &item.bone_scales;
            scaled_bone_matrices_into(&item.parents, &local, |bone| factors.get(bone).copied().unwrap_or(1.0), &mut pose);
            item.local_scratch = local;
            item.channel_scratch = channel_pose;
            Some(pose)
        }
        PoseWork::Free => {
            let sequence = item.animation.sequences.get(item.sequence)?;
            Some(item.bone_pose(elapsed * sequence.rate))
        }
    }
}

fn pose_channels(pose: &ActorPose) -> impl Iterator<Item = (&ChannelPose, f32, &str)> {
    std::iter::once((&pose.base, 1.0, "")).chain(pose.blends.iter().map(|(channel, alpha, bone)| (channel, *alpha, &**bone)))
}

fn pose_work<'a>(item: &AnimatedMesh, world: Option<&'a PlayWorld>, shown_times: &mut Vec<f32>) -> PoseWork<'a> {
    let Some(actor) = item.actor else { return PoseWork::Keep };
    if let Some(bones) = world.and_then(|world| world.shown_ragdoll(actor)) {
        return PoseWork::Ragdoll(bones);
    }
    match world.map_or(Some(None), |world| world.pose(actor)) {
        None => PoseWork::Keep,
        Some(None) => PoseWork::Free,
        Some(Some(pose)) => {
            let from = shown_times.len();
            shown_times.extend(pose_channels(pose).enumerate().map(|(index, (channel, ..))| world.map_or(channel.time, |world| world.shown_time(actor, index, channel))));
            PoseWork::Channels(pose, from)
        }
    }
}

/// The joints of a mesh made after this frame's `animate`, posed as the game shows its actor now, so its first frame
/// is not the bind pose. `None` while the game has no pose for it.
pub(crate) fn first_joints(item: &mut AnimatedMesh, world: &PlayWorld) -> Option<Arc<[Transform]>> {
    let mut shown_times = Vec::new();
    let work = pose_work(item, Some(world), &mut shown_times);
    if matches!(work, PoseWork::Free) {
        return None;
    }
    let bones = pose_mesh(item, &work, &shown_times, 0.0)?;
    // Posed again by the next `animate`, which also records it in `Poses` as for any new mesh.
    item.last_pose = None;
    Some(bones.into_iter().map(joint_transform).collect())
}

fn animate(
    time: Res<Time>,
    mut animated: ResMut<Animated>,
    play: Res<Play>,
    mut poses: ResMut<Poses>,
    mut times: ResMut<FrameTimes>,
    mut generation: Local<u64>,
    mut scratch: Local<PoseScratch>,
) {
    let started = std::time::Instant::now();
    let world = play.world();
    let poses = &mut *poses;
    poses.changed.clear();
    if let Some(world) = world {
        animated.0.retain(|item| match item.actor {
            Some(actor) if world.is_gone(actor) => {
                poses.forget(actor);
                false
            }
            _ => true,
        });
    }
    // A new VM's ids restart: names and aliases are taken again from the meshes animated now. Poses and joints stay for
    // those meshes (a pose not made again keeps its bones, a joint is noted once) and go for all others.
    if *generation != play.generation() {
        *generation = play.generation();
        poses.names.clear();
        poses.aliases.clear();
        let live: FxHashSet<ObjectId> = animated.0.iter().filter_map(|item| item.actor).collect();
        poses.bones.retain(|actor, _| live.contains(actor));
        poses.joints.retain(|actor, _| live.contains(actor));
        for item in &animated.0 {
            if let Some(actor) = item.actor {
                poses.note(actor, item);
            }
        }
    }
    // What each mesh is posed from is read here, where the world is at hand; the posing itself is spread over threads.
    let shown_times = &mut scratch.times;
    shown_times.clear();
    let work: Vec<PoseWork> = animated.0.iter().map(|item| pose_work(item, world, shown_times)).collect();
    let (shown_times, elapsed) = (&*shown_times, time.elapsed_secs());
    let mut posed: Vec<Option<Vec<Mat4>>> = Vec::new();
    posed.resize_with(work.len(), || None);
    let pose_all = |items: &mut [AnimatedMesh], work: &[PoseWork], posed: &mut [Option<Vec<Mat4>>]| {
        for ((item, work), posed) in items.iter_mut().zip(work).zip(posed) {
            *posed = pose_mesh(item, work, shown_times, elapsed);
        }
    };
    if work.len() < POSED_AT_ONCE {
        pose_all(&mut animated.0, &work, &mut posed);
    } else {
        let pool = bevy::tasks::ComputeTaskPool::get();
        let share = work.len().div_ceil(pool.thread_num().max(1)).max(POSED_AT_ONCE / 2);
        let pose_all = &pose_all;
        pool.scope(|scope| {
            for ((items, work), posed) in animated.0.chunks_mut(share).zip(work.chunks(share)).zip(posed.chunks_mut(share)) {
                scope.spawn(async move { pose_all(items, work, posed) });
            }
        });
    }
    drop(work);
    for (item, pose) in animated.0.iter().zip(posed) {
        if let (Some(actor), Some(pose)) = (item.actor, pose) {
            poses.bones.insert(actor, pose);
            poses.changed.insert(actor);
            poses.note(actor, item);
        }
    }
    times.add("animate", started);
}

/// A joint whose bone did not move keeps its transform unchanged: no change detection, so no propagation or skin upload.
fn pose_joints(
    mut poses: ResMut<Poses>,
    mut times: ResMut<FrameTimes>,
    new_joints: Query<(Entity, &SkinJoint), Added<SkinJoint>>,
    mut transforms: Query<&mut Transform, With<SkinJoint>>,
) {
    let started = std::time::Instant::now();
    let poses = &mut *poses;
    poses.moved.clear();
    for (entity, joint) in &new_joints {
        poses.joints.entry(joint.actor).or_default().push((joint.bone, entity));
        if let Some(&bone) = poses.bones.get(&joint.actor).and_then(|pose| pose.get(joint.bone))
            && let Ok(mut transform) = transforms.get_mut(entity)
        {
            transform.set_if_neq(joint_transform(bone));
        }
        poses.moved.insert(joint.actor);
    }
    for actor in &poses.changed {
        let (Some(pose), Some(joints)) = (poses.bones.get(actor), poses.joints.get_mut(actor)) else { continue };
        joints.retain(|&(bone, entity)| {
            let Ok(mut transform) = transforms.get_mut(entity) else { return false };
            if let Some(&matrix) = pose.get(bone) {
                transform.set_if_neq(joint_transform(matrix));
            }
            true
        });
        poses.moved.insert(*actor);
    }
    times.add("pose joints", started);
}

/// A bone scaled to nothing (`SetBoneScale` 0: a cut limb or head) keeps no rotation: glam divides by the zero scale and
/// makes it NaN, which made the mesh's bounds NaN and the whole body vanish or flicker.
fn joint_transform(matrix: Mat4) -> Transform {
    let transform = unreal_to_bevy(matrix);
    if transform.rotation.is_finite() { transform } else { Transform { rotation: Quat::IDENTITY, ..transform } }
}

/// Bevy's `DynamicSkinnedMeshBounds` does this for every skinned mesh every frame and writes the `Aabb` even when equal,
/// which also makes the renderer extract the mesh again; a mesh's bounds are in its own space, so only a new pose
/// changes them.
fn skinned_bounds(
    poses: Res<Poses>,
    inverse_bindposes: Res<Assets<SkinnedMeshInverseBindposes>>,
    meshes: Res<Assets<Mesh>>,
    mut sections: Query<(&mut Aabb, &Mesh3d, &SkinnedMesh, &GlobalTransform, &FollowActor)>,
    joints: Query<&GlobalTransform>,
    mut times: ResMut<FrameTimes>,
) {
    let started = std::time::Instant::now();
    for (mut aabb, mesh, skinned, global, follow) in &mut sections {
        if !(aabb.is_added() || poses.moved.contains(&follow.actor)) {
            continue;
        }
        let (Some(bindposes), Some(mesh)) = (inverse_bindposes.get(&skinned.inverse_bindposes), meshes.get(mesh)) else { continue };
        if let Ok(bounds) = entity_aabb_from_skinned_mesh_bounds(&joints, mesh, skinned, bindposes, Some(global)) {
            aabb.set_if_neq(bounds.into());
        }
    }
    times.add("skinned bounds", started);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_bone_matrices() {
        let local = [([0.1, 0.2, 0.3, 0.927], [1.0, 2.0, 3.0]), ([0.0, 0.0, 0.383, 0.924], [10.0, 0.0, 0.0]), ([0.5, 0.0, 0.0, 0.866], [0.0, 5.0, 0.0])];
        let parents = [0, 0, 1];
        let matrices = bone_matrices(&parents, &local);
        let frames = p2_assets::pose::bone_frames(&parents, &local, |_| 1.0);
        for (matrix, frame) in matrices.iter().zip(&frames) {
            assert!(matrix.abs_diff_eq(frame_matrix(frame), 1e-3), "{matrix} vs {:?}", frame_matrix(frame));
        }
    }

    #[test]
    fn a_bone_scaled_to_nothing_gives_a_finite_joint() {
        let matrix = Mat4::from_scale_rotation_translation(Vec3::ZERO, Quat::from_rotation_z(0.7), Vec3::new(1.0, 2.0, 3.0));
        assert!(!unreal_to_bevy(matrix).rotation.is_finite(), "glam no longer makes NaN here");
        let joint = joint_transform(matrix);
        assert!(joint.rotation.is_finite() && joint.scale == Vec3::ZERO && joint.translation.is_finite(), "{joint:?}");
        let attached = attachment_frame(Mat4::IDENTITY, matrix, [0.0; 3].into(), [0; 3].into());
        assert!(attached.is_finite(), "{attached}");
    }

    #[test]
    fn blending_reaches_only_the_marked_bones() {
        let base = vec![([0.0, 0.0, 0.0, 1.0], [0.0; 3]); 2];
        let over = vec![([0.0, 0.0, 0.0, 1.0], [10.0, 0.0, 0.0]); 2];
        let mut blended = base.clone();
        blend_local(&mut blended, &over, 0.5, &[false, true]);
        assert_eq!(blended[0], base[0]);
        assert_eq!(blended[1].1, [5.0, 0.0, 0.0]);
    }
}