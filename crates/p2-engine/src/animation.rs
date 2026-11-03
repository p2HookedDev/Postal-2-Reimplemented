pub use crate::components::FollowBone;
use std::collections::HashMap;
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

use crate::mesh_view::{FollowActor, MeshViewSystems, SkinJoint};
use crate::play::{ActorPose, ChannelPose, FrameTimes, Play};

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnimationSystems {
    Pose,
}

pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        // After the actors' models took their actors' places (`play::follow_actors`), so attachments land on this
        // frame's bones.
        app.init_resource::<Animated>()
            .init_resource::<Poses>()
            .init_resource::<AttachmentPlaces>()
            .configure_sets(Update, AnimationSystems::Pose.after(MeshViewSystems::Camera))
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

#[derive(Clone, Copy)]
pub(crate) struct BoneFrame {
    pub(crate) location: Vector,
    pub(crate) axes: [Vector; 3],
}

fn axes_matrix(axes: [Vector; 3], origin: Vec3) -> Mat4 {
    let [x, y, z] = axes.map(|axis| Vec3::from_array(axis.into()).extend(0.0));
    Mat4::from_cols(x, y, z, origin.extend(1.0))
}

#[derive(Resource, Default)]
pub(crate) struct AttachmentPlaces(pub(crate) FxHashMap<ObjectId, (ObjectId, BoneFrame)>);

impl AttachmentPlaces {
    fn record(&mut self, actor: ObjectId, base: ObjectId, frame: Mat4) {
        let axes = [frame.x_axis, frame.y_axis, frame.z_axis].map(|axis| axis.truncate().to_array());
        self.0.insert(actor, (base, BoneFrame { location: frame.w_axis.truncate().to_array().into(), axes: axes.map(Vector::from) }));
    }

    pub(crate) fn frame(&self, actor: ObjectId) -> Option<Mat4> {
        let (_, place) = self.0.get(&actor)?;
        Some(axes_matrix(place.axes, Vec3::from_array(place.location.into())))
    }
}

impl FollowBone {
    pub fn transform(&self, body: Mat4, bone: Mat4) -> Transform {
        self.transform_at(self.frame(body, bone))
    }

    fn frame(&self, body: Mat4, bone: Mat4) -> Mat4 {
        attachment_frame(body, bone, self.relative_location, self.relative_rotation)
    }

    fn transform_at(&self, frame: Mat4) -> Transform {
        skeletal_transform(frame, self.scale, self.pre_pivot, (self.mesh_scale, self.origin, self.rot_origin))
    }
}

fn skeletal_transform(frame: Mat4, scale: Vector, pre_pivot: Vector, (mesh_scale, origin, rot_origin): (Vector, Vector, Rotator)) -> Transform {
    let (_, rotation, position) = frame.to_scale_rotation_translation();
    unreal_to_bevy(unreal_mesh_matrix(position.to_array().into(), Mat4::from_quat(rotation), scale, pre_pivot, mesh_scale, origin, rot_origin))
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
    vm_bone_maps: HashMap<Arc<str>, Arc<[Option<usize>]>>,
    subtrees: HashMap<Arc<str>, Arc<[bool]>>,
    last_pose: Option<u64>,
    bone_indices: FxHashMap<Arc<str>, Option<usize>>,
    bone_scales: Vec<f32>,
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
            vm_bone_maps: HashMap::new(),
            subtrees: HashMap::new(),
            last_pose: None,
            bone_indices: FxHashMap::default(),
            bone_scales: Vec::new(),
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
        bone_matrices(&self.parents, &self.local_pose(animation, sequence, frame, bone_map))
    }

    fn local_pose(&self, animation: &MeshAnimation, sequence: usize, frame: f32, bone_map: &[Option<usize>]) -> Vec<LocalBone> {
        let pose = animation.pose_at(sequence, frame);
        self.mesh
            .bones
            .iter()
            .zip(bone_map)
            .map(|(bone, track)| track.and_then(|t| pose.get(t)).unwrap_or((bone.orientation, bone.position)))
            .collect()
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

fn pose_channels(pose: &ActorPose) -> impl Iterator<Item = (&ChannelPose, f32, &str)> {
    std::iter::once((&pose.base, 1.0, "")).chain(pose.blends.iter().map(|(channel, alpha, bone)| (channel, *alpha, &**bone)))
}

fn animate(
    time: Res<Time>,
    mut animated: ResMut<Animated>,
    play: Res<Play>,
    mut poses: ResMut<Poses>,
    mut times: ResMut<FrameTimes>,
    mut generation: Local<u64>,
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
    for item in &mut animated.0 {
        let Some(actor) = item.actor else { continue };
        if let Some(world) = world
            && let Some(bones) = world.ragdoll(actor)
        {
            let mut key = std::hash::DefaultHasher::new();
            bones.iter().for_each(|b| b.axes.iter().chain([&b.origin]).flatten().for_each(|c| c.to_bits().hash(&mut key)));
            let key = Some(key.finish());
            if key != item.last_pose {
                item.last_pose = key;
                poses.bones.insert(actor, bones.iter().map(frame_matrix).collect());
                poses.changed.insert(actor);
                poses.note(actor, item);
            }
            continue;
        }
        // An actor the presented frame did not pose keeps its pose.
        let channels = match world {
            Some(world) => match world.pose(actor) {
                Some(pose) => pose,
                None => continue,
            },
            None => None,
        };
        let pose = match channels {
            Some(actor_pose) => {
                let ActorPose { scales, turns, .. } = actor_pose;
                let mut key = std::hash::DefaultHasher::new();
                for (channel, alpha, bone) in pose_channels(actor_pose) {
                    (Arc::as_ptr(&channel.animation), channel.sequence, channel.time.to_bits(), alpha.to_bits(), bone).hash(&mut key);
                    channel.tween.as_ref().map(|(from, weight)| (Arc::as_ptr(from).addr(), weight.to_bits())).hash(&mut key);
                }
                scales.iter().for_each(|(bone, scale)| (&**bone, scale.to_bits()).hash(&mut key));
                turns.iter().for_each(|(bone, turn)| (&**bone, turn).hash(&mut key));
                let key = Some(key.finish());
                if key == item.last_pose {
                    continue;
                }
                item.last_pose = key;
                let mut local = None;
                for (channel, alpha, bone) in pose_channels(actor_pose) {
                    let map = Arc::clone(item.vm_bone_maps.entry(Arc::clone(&channel.animation.path)).or_insert_with(|| map_bones(&item.bone_names, &channel.animation.bone_names).into()));
                    let mut pose = item.local_pose(&channel.animation.animation, channel.sequence, channel.time, &map);
                    if let Some((from, weight)) = &channel.tween {
                        tween_pose(from, &mut pose, *weight);
                    }
                    match &mut local {
                        None => local = Some(pose),
                        Some(base) => blend_local(base, &pose, alpha, &item.subtree(bone)),
                    }
                }
                item.scale_bones(scales);
                let mut local = local.unwrap_or_default();
                for (bone, turn) in turns {
                    if let Some(index) = item.bone_index(bone)
                        && let Some(posed) = local.get_mut(index)
                    {
                        posed.0 = p2_assets::pose::turn_local(posed.0, *turn);
                    }
                }
                let mut pose = poses.bones.get_mut(&actor).map(std::mem::take).unwrap_or_default();
                let factors = &item.bone_scales;
                scaled_bone_matrices_into(&item.parents, &local, |bone| factors.get(bone).copied().unwrap_or(1.0), &mut pose);
                pose
            }
            None => {
                let Some(sequence) = item.animation.sequences.get(item.sequence) else { continue };
                item.bone_pose(time.elapsed_secs() * sequence.rate)
            }
        };
        poses.bones.insert(actor, pose);
        poses.changed.insert(actor);
        poses.note(actor, item);
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

fn follow_bones(
    mut commands: Commands,
    poses: Res<Poses>,
    play: Res<Play>,
    mut places: ResMut<AttachmentPlaces>,
    mut attached: Query<(Entity, Ref<FollowBone>, &mut Transform, &mut Visibility)>,
) {
    let world = play.world();
    for (entity, follow, mut transform, mut shown) in &mut attached {
        let changed = world.is_some_and(|world| world.changed(follow.base) || world.changed(follow.actor));
        if !(follow.is_added() || changed || poses.changed.contains(&follow.base)) {
            continue;
        }
        let base = world.and_then(|world| world.actor(follow.base));
        let own = world.and_then(|world| world.actor(follow.actor));
        if base.is_some_and(|base| base.gone) || own.is_some_and(|own| own.gone) {
            commands.entity(entity).despawn();
            continue;
        }
        if let Some(own) = own
            && own.attachment.as_ref().is_none_or(|attachment| attachment.base != follow.base)
        {
            let skeletal = Some((follow.mesh_scale, follow.origin, follow.rot_origin));
            commands.entity(entity).remove::<FollowBone>().insert(FollowActor { actor: follow.actor, scale: follow.scale, pre_pivot: follow.pre_pivot, skeletal });
            continue;
        }
        if base.is_some() || own.is_some() {
            let hidden = base.is_some_and(|base| base.hidden) || own.is_some_and(|own| own.hidden);
            shown.set_if_neq(if hidden { Visibility::Hidden } else { Visibility::Inherited });
        }
        let Some(&bone) = poses.bones.get(&follow.base).and_then(|pose| pose.get(follow.bone)) else { continue };
        let body = match base {
            Some(base) => FollowActor { pre_pivot: base.pre_pivot, ..follow.base_follow }.mesh_matrix(base.location, base.rotation).unwrap_or(follow.body),
            None => follow.body,
        };
        let frame = follow.frame(body, bone);
        places.record(follow.actor, follow.base, frame);
        transform.set_if_neq(follow.transform_at(frame));
    }
}

/// They sit at the bone's current frame instead of their actor's `Location`, which the VM does not move with the bone.
fn place_attached(
    mut poses: ResMut<Poses>,
    play: Res<Play>,
    mut places: ResMut<AttachmentPlaces>,
    mut models: Query<(Entity, Ref<FollowActor>, &mut Transform)>,
    on_bones: Query<&FollowBone>,
    mut buffers: Local<AttachedBuffers>,
) {
    let Some(world) = play.world() else { return };
    let poses = &mut *poses;
    let AttachedBuffers { generation, bone_bodies, bases, due_models } = &mut *buffers;
    if *generation != play.generation() {
        *generation = play.generation();
        poses.slots.clear();
    }
    bone_bodies.clear();
    bases.clear();
    due_models.clear();
    // Bases themselves on a bone (a map pawn's head, drawn as `FollowBone`): without these a bolt-on hat stayed at its spawn
    // point in the air.
    bone_bodies.extend(on_bones.iter().filter_map(|follow| {
        let place = places.0.get(&follow.actor)?.1;
        let rotation = axes_matrix(place.axes, Vec3::ZERO);
        let body = unreal_mesh_matrix(place.location, rotation, follow.scale, follow.pre_pivot, follow.mesh_scale, follow.origin, follow.rot_origin);
        Some((follow.actor, body))
    }));
    let bone_bodies = &*bone_bodies;
    let changed = &poses.changed;
    let due = |follow: &FollowActor, added: bool| {
        let attachment = world.actor(follow.actor)?.attachment.as_ref()?;
        let base = attachment.base;
        (added || world.changed(follow.actor) || world.changed(base) || changed.contains(&base) || bone_bodies.contains_key(&base))
            .then_some(attachment)
    };
    // Emitters on a bone have no model but are placed the same way. The first-person muzzle flash is one:
    // left at the weapon's origin, its sprite filled the screen.
    let emitters: Vec<_> = world
        .watched()
        .filter_map(|actor| {
            let attachment = world.actor(actor)?.attachment.as_ref()?;
            (world.changed(actor)
                || world.changed(attachment.base)
                || changed.contains(&attachment.base)
                || bone_bodies.contains_key(&attachment.base))
                .then_some((actor, attachment))
        })
        .collect();
    for (entity, follow, _) in &models {
        if let Some(attachment) = due(&follow, follow.is_added()) {
            due_models.push(entity);
            bases.insert(attachment.base, None);
        }
    }
    bases.extend(emitters.iter().map(|(_, attachment)| (attachment.base, None)));
    if bases.is_empty() {
        return;
    }
    for (_, follow, _) in &models {
        if follow.skeletal.is_some()
            && let Some(base) = bases.get_mut(&follow.actor)
        {
            *base = Some(*follow);
        }
    }
    let (slots, names, aliases, bones) = (&mut poses.slots, &poses.names, &poses.aliases, &poses.bones);
    let mut frame_of = |attachment: &crate::actors::BoneAttachment| {
        let (index, tag) = bone_slot(slots, names, aliases, attachment)?;
        let bone = *bones.get(&attachment.base)?.get(index)? * tag;
        let body = match bases.get(&attachment.base).copied().flatten() {
            Some(base) => {
                let actor = world.actor(attachment.base)?;
                base.mesh_matrix(actor.location, actor.rotation)?
            }
            None => *bone_bodies.get(&attachment.base)?,
        };
        Some(attachment_frame(body, bone, attachment.relative_location, attachment.relative_rotation))
    };
    for &(actor, attachment) in &emitters {
        if let Some(frame) = frame_of(attachment) {
            places.record(actor, attachment.base, frame);
        }
    }
    for &entity in due_models.iter() {
        let Ok((_, follow, mut transform)) = models.get_mut(entity) else { continue };
        let Some(attachment) = world.actor(follow.actor).and_then(|actor| actor.attachment.as_ref()) else { continue };
        let Some(frame) = frame_of(attachment) else { continue };
        places.record(follow.actor, attachment.base, frame);
        transform.set_if_neq(attached_transform(&follow, frame));
    }
}

#[derive(Default)]
struct AttachedBuffers {
    generation: u64,
    bone_bodies: FxHashMap<ObjectId, Mat4>,
    bases: FxHashMap<ObjectId, Option<FollowActor>>,
    due_models: Vec<Entity>,
}

fn bone_slot(
    slots: &mut FxHashMap<ObjectId, Vec<(p2_vm::NameId, usize, Mat4)>>,
    names: &FxHashMap<ObjectId, Arc<[String]>>,
    aliases: &FxHashMap<ObjectId, Arc<[(String, usize, Mat4)]>>,
    attachment: &crate::actors::BoneAttachment,
) -> Option<(usize, Mat4)> {
    let known = slots.entry(attachment.base).or_default();
    if let Some(&(_, index, tag)) = known.iter().find(|(name, ..)| *name == attachment.bone_id) {
        return Some((index, tag));
    }
    let alias = aliases.get(&attachment.base).and_then(|aliases| aliases.iter().find(|(alias, ..)| **alias == *attachment.bone));
    let (index, tag) = match alias {
        Some(&(_, index, tag)) => (index, tag),
        None => (names.get(&attachment.base)?.iter().position(|name| **name == *attachment.bone)?, Mat4::IDENTITY),
    };
    known.push((attachment.bone_id, index, tag));
    Some((index, tag))
}

pub(crate) fn attached_transform(follow: &FollowActor, frame: Mat4) -> Transform {
    if let Some(mesh) = follow.skeletal {
        return skeletal_transform(frame, follow.scale, follow.pre_pivot, mesh);
    }
    let (_, rotation, position) = frame.to_scale_rotation_translation();
    unreal_to_bevy(
        Mat4::from_translation(position)
            * Mat4::from_quat(rotation)
            * Mat4::from_scale(Vec3::from_array(follow.scale.into()))
            * Mat4::from_translation(-Vec3::from_array(follow.pre_pivot.into())),
    )
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
) {
    for (mut aabb, mesh, skinned, global, follow) in &mut sections {
        if !(aabb.is_added() || poses.moved.contains(&follow.actor)) {
            continue;
        }
        let (Some(bindposes), Some(mesh)) = (inverse_bindposes.get(&skinned.inverse_bindposes), meshes.get(mesh)) else { continue };
        if let Ok(bounds) = entity_aabb_from_skinned_mesh_bounds(&joints, mesh, skinned, bindposes, Some(global)) {
            aabb.set_if_neq(bounds.into());
        }
    }
}

pub fn attachment_frame(body: Mat4, bone: Mat4, relative_location: Vector, relative_rotation: Rotator) -> Mat4 {
    let (_, rotation, position) = (body * bone).to_scale_rotation_translation();
    // A bone scaled to nothing has no rotation (NaN from glam).
    let rotation = if rotation.is_finite() { rotation } else { Quat::IDENTITY };
    Mat4::from_rotation_translation(rotation, position)
        * Mat4::from_translation(Vec3::from_array(relative_location.into()))
        * p2_bevy_assets::rotator_matrix(relative_rotation)
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