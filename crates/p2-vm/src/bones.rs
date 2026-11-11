use std::sync::Arc;

use rustc_hash::FxHashMap;

use p2_assets::pose::{Frame, LocalBone, blend_local, bone_frames, channel_weight, mesh_to_world, subtree, turn_local, tween_pose};
use p2_assets::{MeshAnimation, SkeletalMesh, read_mesh_animation, read_primitive_sphere, read_skeletal_mesh};

use p2_math::{Rotator, Vector};
use p2_package::{Loader, PackageId};

use crate::enums::EDrawType;
use crate::anim::AnimChannel;
use crate::collision::Placement;
use crate::object_table::ObjectTable;
use crate::{LoaderRef, NameId, ObjectId, ObjectStore, Value, Vm, Prop};

pub(crate) struct PosedMesh {
    pub(crate) mesh: SkeletalMesh,
    pub(crate) names: Vec<String>,
    pub(crate) parents: Vec<usize>,
    pub(crate) aliases: Vec<(String, String)>,
    /// Bind pose relative to the parent; a ragdoll's bones without a body take it.
    pub(crate) reference: Vec<Frame>,
}

impl PosedMesh {
    /// `name` is lower case.
    pub(crate) fn match_ref_bone(&self, name: &str) -> Option<usize> {
        let name = self.aliases.iter().find(|(alias, _)| alias == name).map_or(name, |(_, bone)| bone.as_str());
        self.names.iter().position(|n| n == name)
    }
}

pub(crate) struct PosedAnimation {
    animation: Arc<MeshAnimation>,
    bones: Vec<String>,
    sequences: Vec<String>,
}

#[derive(Default)]
pub(crate) struct BoneData {
    meshes: FxHashMap<ObjectId, Option<Arc<PosedMesh>>>,
    animations: FxHashMap<ObjectId, Option<Arc<PosedAnimation>>>,
    pub(crate) parsed: MeshAnimations,
    poses: ObjectTable<Box<(PoseKey, Arc<[Frame]>)>>,
    bone_maps: FxHashMap<(ObjectId, ObjectId), Arc<[Option<usize>]>>,
    sequences: FxHashMap<(ObjectId, NameId), Option<usize>>,
    mesh_bones: FxHashMap<(ObjectId, NameId), Option<usize>>,
    ref_bones: FxHashMap<(ObjectId, NameId), Option<usize>>,
    spheres: FxHashMap<(ObjectId, &'static str), [f32; 4]>,
}

const POSE_CACHE_LIMIT: usize = 256;

/// Floats compare by bits, so a kept pose is reused only when a recompute reads the very same floats
/// (`-0.0` and `0.0` differ, a NaN matches itself).
#[derive(Debug, PartialEq, Eq)]
struct PoseKey {
    mesh: ObjectId,
    channels: Vec<(NameId, NameId, Option<ObjectId>, [u32; 2], Option<usize>)>,
    scales: Vec<(NameId, u32)>,
    turns: Vec<(NameId, Rotator)>,
    rotation: Rotator,
    placement: [[u32; 3]; 3],
}

fn channel_key(&(c, a): &(&AnimChannel, Option<ObjectId>)) -> (NameId, NameId, Option<ObjectId>, [u32; 2], Option<usize>) {
    (c.sequence, c.blend_bone, a, [c.frame.to_bits(), c.blend_alpha.to_bits()], c.tween().map(|(from, _)| Arc::as_ptr(from).addr()))
}

fn scale_key((bone, scale): (NameId, f32)) -> (NameId, u32) {
    (bone, scale.to_bits())
}

fn placement_key(Placement { location, scale, pre_pivot, .. }: Placement) -> [[u32; 3]; 3] {
    [location, scale, pre_pivot].map(|v: Vector| <[f32; 3]>::from(v).map(f32::to_bits))
}

impl PoseKey {
    fn new(
        mesh: ObjectId,
        channels: &[(&AnimChannel, Option<ObjectId>)],
        scales: impl IntoIterator<Item = (NameId, f32)>,
        turns: &[(NameId, Rotator)],
        placement: Placement,
    ) -> Self {
        Self {
            mesh,
            channels: channels.iter().map(channel_key).collect(),
            scales: scales.into_iter().map(scale_key).collect(),
            turns: turns.to_vec(),
            rotation: placement.rotation,
            placement: placement_key(placement),
        }
    }

    fn matches(
        &self,
        mesh: ObjectId,
        channels: &[(&AnimChannel, Option<ObjectId>)],
        scales: impl IntoIterator<Item = (NameId, f32)>,
        turns: &[(NameId, Rotator)],
        placement: Placement,
    ) -> bool {
        self.mesh == mesh
            && self.rotation == placement.rotation
            && self.placement == placement_key(placement)
            && self.turns == turns
            && self.channels.iter().copied().eq(channels.iter().map(channel_key))
            && self.scales.iter().copied().eq(scales.into_iter().map(scale_key))
    }
}

/// `None`: unreadable. Cloning shares the map, so the host's renderer reads each animation once a map.
#[derive(Clone, Default)]
pub struct MeshAnimations(Arc<FxHashMap<(PackageId, usize), Option<Arc<MeshAnimation>>>>);

impl MeshAnimations {
    pub fn get(&self, package: PackageId, export: usize) -> Option<Option<Arc<MeshAnimation>>> {
        self.0.get(&(package, export)).cloned()
    }

    fn insert(&mut self, key: (PackageId, usize), read: Option<Arc<MeshAnimation>>) {
        Arc::make_mut(&mut self.0).insert(key, read);
    }
}

fn transform_sphere(frame: &Frame, [x, y, z, radius]: [f32; 4]) -> [f32; 4] {
    let [x, y, z] = frame.transform_point([x, y, z]);
    let longest = frame.axes.iter().map(|[x, y, z]| x * x + y * y + z * z).fold(0.0, f32::max);
    [x, y, z, longest.sqrt() * radius]
}

const UNIT: Frame = Frame::IDENTITY;

impl BoneData {
    fn keep_pose(&mut self, store: &ObjectStore, actor: ObjectId, key: PoseKey, frames: Arc<[Frame]>) {
        if self.poses.get(actor).is_none() && self.poses.count() >= POSE_CACHE_LIMIT {
            let mut poses = std::mem::take(&mut self.poses);
            poses.retain(|posed| !store.is_deleted(posed));
            if poses.count() >= POSE_CACHE_LIMIT {
                poses = ObjectTable::default();
            }
            self.poses = poses;
        }
        self.poses.insert(actor, Box::new((key, frames)));
    }

    fn ref_bone(&mut self, store: &ObjectStore, mesh_object: ObjectId, mesh: &PosedMesh, bone: NameId) -> Option<usize> {
        *self.ref_bones.entry((mesh_object, bone)).or_insert_with(|| mesh.match_ref_bone(&store.name(bone).to_ascii_lowercase()))
    }

    fn mesh_bone(&mut self, store: &ObjectStore, mesh_object: ObjectId, mesh: &PosedMesh, bone: NameId) -> Option<usize> {
        *self.mesh_bones.entry((mesh_object, bone)).or_insert_with(|| {
            let name = store.name(bone).to_ascii_lowercase();
            mesh.names.iter().position(|n| *n == name)
        })
    }

    fn sequence_index(&mut self, store: &ObjectStore, animation_object: ObjectId, animation: &PosedAnimation, sequence: NameId) -> Option<usize> {
        *self.sequences.entry((animation_object, sequence)).or_insert_with(|| {
            let name = store.name(sequence).to_ascii_lowercase();
            animation.sequences.iter().position(|s| *s == name)
        })
    }

    fn bone_map(&mut self, mesh_object: ObjectId, mesh: &PosedMesh, animation_object: ObjectId, animation: &PosedAnimation) -> Arc<[Option<usize>]> {
        let key = (mesh_object, animation_object);
        if let Some(map) = self.bone_maps.get(&key) {
            return map.clone();
        }
        let map: Arc<[Option<usize>]> = mesh.names.iter().map(|name| animation.bones.iter().position(|b| b == name)).collect();
        self.bone_maps.insert(key, map.clone());
        map
    }

    fn saved_sphere(&mut self, store: &ObjectStore, loader: &Loader, mesh: ObjectId, class: &'static str) -> Option<[f32; 4]> {
        if let Some(&sphere) = self.spheres.get(&(mesh, class)) {
            return Some(sphere);
        }
        let (package, export) = loader.loaded_export_by_path_of_class(&store.object(mesh).path, class)?;
        let loaded = loader.package(package);
        let sphere = read_primitive_sphere(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok()?;
        self.spheres.insert((mesh, class), sphere);
        Some(sphere)
    }

    fn read_posed_mesh(&mut self, store: &ObjectStore, loader: &Loader, mesh: ObjectId) -> Option<Arc<PosedMesh>> {
        let read = loader.loaded_export_by_path_of_class(&store.object(mesh).path, "SkeletalMesh").and_then(|(id, export)| {
            let loaded = loader.package(id);
            let mesh = read_skeletal_mesh(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok()?;
            let names = mesh.bones.iter().map(|b| loaded.package.name(b.name).to_ascii_lowercase()).collect();
            let parents = mesh.bones.iter().map(|b| b.parent).collect();
            let name = |index: usize| loaded.package.name(index).to_ascii_lowercase();
            let aliases = mesh.attach_aliases.iter().map(|a| (name(a.alias), name(a.bone))).collect();
            let reference = mesh.bones.iter().enumerate().map(|(bone, b)| reference_frame(bone, b.orientation, b.position)).collect();
            Some(Arc::new(PosedMesh { mesh, names, parents, aliases, reference }))
        });
        self.meshes.insert(mesh, read.clone());
        read
    }

    fn prewarm_animations(&mut self, loader: &Loader) {
        let exports: Vec<(PackageId, usize)> = loader
            .package_names()
            .filter_map(|name| loader.loaded_id(name))
            .flat_map(|id| {
                let pkg = &loader.package(id).package;
                (0..pkg.exports.len()).filter(move |&e| pkg.object_name(pkg.exports[e].class_index) == Some("MeshAnimation")).map(move |e| (id, e))
            })
            .filter(|key| !self.parsed.0.contains_key(key))
            .collect();
        let read: Vec<((PackageId, usize), Option<Arc<MeshAnimation>>)> = {
            use rayon::prelude::*;
            exports
                .into_par_iter()
                .map(|(id, export)| {
                    let loaded = loader.package(id);
                    ((id, export), read_mesh_animation(&loaded.package, &loaded.data, &loaded.package.exports[export]).ok().map(Arc::new))
                })
                .collect()
        };
        Arc::make_mut(&mut self.parsed.0).extend(read);
    }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn get_bone_coords(&mut self, actor: ObjectId, bone: NameId) -> Value {
        let coords = |frame: Frame| {
            let axes = frame.axes.map(|axis| normalized(axis.into()));
            Value::Struct(vec![Value::Vector(frame.origin.into()), Value::Vector(axes[0]), Value::Vector(axes[1]), Value::Vector(axes[2])])
        };
        if self.store.is_deleted(actor) {
            return coords(UNIT);
        }
        match self.bone_frame(actor, bone) {
            Some(Some(frame)) => coords(frame),
            Some(None) => coords(UNIT),
            None => Value::Struct(vec![Value::Vector([0.0; 3].into()); 4]),
        }
    }

    /// `GetBoneRotation(BoneName, Space)`: `Space` other than global is not modelled (unverified).
    pub(crate) fn get_bone_rotation(&mut self, actor: ObjectId, bone: NameId) -> Value {
        let Some(Some(frame)) = self.bone_frame(actor, bone) else { return Value::Rotator([0; 3].into()) };
        let [x, y, z] = frame.axes.map(|axis| normalized(axis.into()));
        Value::Rotator(crate::natives::orthogonal_rotation(x, y, z))
    }

    /// `None` without a skeletal mesh, `Some(None)` when the mesh has no such bone.
    fn bone_frame(&mut self, actor: ObjectId, bone: NameId) -> Option<Option<Frame>> {
        let mesh_object = self.store.object_of(actor, Prop::Mesh)?;
        let mesh = self.posed_mesh(mesh_object)?;
        let Some(index) = self.media.bone_data.ref_bone(&self.store, mesh_object, &mesh, bone) else { return Some(None) };
        if let Some(frames) = self.ragdoll_world_frames(actor) {
            return Some(frames.get(index).copied());
        }
        let (_, frames) = self.world_bone_frames(actor)?;
        Some(frames.get(index).copied())
    }

    /// Scale included in the axes. Kept per actor and reused while nothing it is posed from changed.
    pub(crate) fn world_bone_frames(&mut self, actor: ObjectId) -> Option<(Arc<PosedMesh>, Arc<[Frame]>)> {
        let mesh_object = self.store.object_of(actor, Prop::Mesh)?;
        let mesh = self.posed_mesh(mesh_object)?;
        let key = {
            let mut channels = Vec::new();
            while let Some(channel) = self.anim_channel(actor, channels.len()) {
                channels.push(channel);
            }
            let placement = self.drawn_placement(actor);
            if let Some((kept, frames)) = self.media.bone_data.poses.get(actor).map(Box::as_ref)
                && kept.matches(mesh_object, &channels, self.bone_scales(actor), self.bone_rotations(actor), placement)
            {
                return Some((mesh, frames.clone()));
            }
            PoseKey::new(mesh_object, &channels, self.bone_scales(actor), self.bone_rotations(actor), placement)
        };
        let frames: Arc<[Frame]> = self.pose_world_bone_frames(actor, mesh_object, &mesh).into();
        self.media.bone_data.keep_pose(&self.store, actor, key, frames.clone());
        Some((mesh, frames))
    }

    /// `None` when no channel plays yet: a first animation starts at once.
    pub(crate) fn kept_pose(&mut self, actor: ObjectId) -> Option<Arc<[LocalBone]>> {
        let mesh_object = self.store.object_of(actor, Prop::Mesh)?;
        let mesh = self.posed_mesh(mesh_object)?;
        self.local_bone_pose(actor, mesh_object, &mesh).map(Into::into)
    }

    fn local_bone_pose(&mut self, actor: ObjectId, mesh_object: ObjectId, mesh: &PosedMesh) -> Option<Vec<LocalBone>> {
        let bind: Vec<LocalBone> = mesh.mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
        let mut local: Option<Vec<LocalBone>> = None;
        for channel in 0.. {
            let Some((state, animation)) = self.anim_channel(actor, channel).map(|(s, a)| (s.clone(), a)) else { break };
            if channel > 0 && state.blend_alpha <= 0.0 {
                continue;
            }
            let Some(animation_object) = animation else { continue };
            let Some(animation) = self.posed_animation(animation_object) else { continue };
            let Some(sequence) = self.media.bone_data.sequence_index(&self.store, animation_object, &animation, state.sequence) else { continue };
            let frames = animation.animation.sequences[sequence].num_frames.max(1) as f32;
            let pose = animation.animation.pose_at(sequence, state.frame.max(0.0) * frames);
            let tracks = self.media.bone_data.bone_map(mesh_object, mesh, animation_object, &animation);
            let mut posed: Vec<LocalBone> = tracks.iter().zip(&bind).map(|(&track, &bound)| track.and_then(|t| pose.get(t)).unwrap_or(bound)).collect();
            if let Some((from, weight)) = state.tween() {
                tween_pose(from, &mut posed, weight);
            }
            match &mut local {
                None => local = Some(posed),
                Some(base) => {
                    let start = (state.blend_bone != NameId::NONE).then(|| self.media.bone_data.mesh_bone(&self.store, mesh_object, mesh, state.blend_bone)).flatten();
                    blend_local(base, &posed, channel_weight(state.blend_alpha, state.frame, state.blend_in_time), &subtree(&mesh.parents, start));
                }
            }
        }
        local
    }

    fn pose_world_bone_frames(&mut self, actor: ObjectId, mesh_object: ObjectId, mesh: &PosedMesh) -> Vec<Frame> {
        let mut local = self.local_bone_pose(actor, mesh_object, mesh).unwrap_or_else(|| mesh.mesh.bones.iter().map(|b| (b.orientation, b.position)).collect());
        for (bone, turn) in self.bone_rotations(actor).to_vec() {
            if let Some(index) = self.media.bone_data.mesh_bone(&self.store, mesh_object, mesh, bone) {
                local[index].0 = turn_local(local[index].0, turn.into());
            }
        }
        let scales: Vec<(String, f32)> =
            self.bone_scales(actor).map(|(bone, scale)| (self.store.name(bone).to_ascii_lowercase(), scale)).collect();
        let scale = |bone: usize| scales.iter().filter(|(name, _)| *name == mesh.names[bone]).map(|(_, s)| s).product::<f32>();
        let frames = bone_frames(&mesh.parents, &local, scale);
        let world = self.mesh_to_world(actor, mesh);
        frames.iter().map(|frame| world.then(frame)).collect()
    }

    pub(crate) fn mesh_to_world(&self, actor: ObjectId, mesh: &PosedMesh) -> Frame {
        let Placement { location, rotation, scale, pre_pivot } = self.drawn_placement(actor);
        mesh_to_world(
            location,
            rotation,
            scale,
            pre_pivot,
            mesh.mesh.scale,
            mesh.mesh.origin,
            mesh.mesh.rot_origin,
        )
    }

    /// A `Plane` with W the radius. Skeletal mesh: the saved sphere, never a posed one.
    /// Primitives: centre zero, radius the length of `CollisionRadius`, `CollisionHeight` (unverified).
    pub(crate) fn render_bounding_sphere(&mut self, actor: ObjectId) -> Value {
        let Placement { location, rotation, scale, pre_pivot } = self.drawn_placement(actor);
        let local_to_world = mesh_to_world(location, rotation, scale, pre_pivot, [1.0; 3].into(), [0.0; 3].into(), [0; 3].into());
        let draw_type = self.draw_type_of(actor);
        let skeletal = if draw_type == Some(EDrawType::Mesh) { self.skeletal_mesh(actor).and_then(|mesh| Some((mesh, self.posed_mesh(mesh)?))) } else { None };
        let (frame, saved) = if let Some((mesh, posed)) = skeletal {
            (self.mesh_to_world(actor, &posed), self.media.bone_data.saved_sphere(&self.store, &self.loader, mesh, "SkeletalMesh"))
        } else if draw_type == Some(EDrawType::StaticMesh)
            && let Some(mesh) = self.store.object_of(actor, Prop::StaticMesh)
        {
            (local_to_world, self.media.bone_data.saved_sphere(&self.store, &self.loader, mesh, "StaticMesh"))
        } else {
            (local_to_world, None)
        };
        let sphere = saved.unwrap_or_else(|| {
            let (radius, height) = self.store.collision_size(actor);
            [0.0, 0.0, 0.0, (radius * radius + height * height).sqrt()]
        });
        let [x, y, z, w] = transform_sphere(&frame, sphere);
        // `Plane`'s own member comes before the inherited ones in a struct's fields (`MemLayouts::of_class`).
        Value::Struct([w, x, y, z].map(Value::Float).to_vec())
    }

    pub(crate) fn posed_mesh(&mut self, mesh: ObjectId) -> Option<Arc<PosedMesh>> {
        if let Some(found) = self.media.bone_data.meshes.get(&mesh) {
            return found.clone();
        }
        let phase = self.profile_start();
        let read = self.media.bone_data.read_posed_mesh(&self.store, &self.loader, mesh);
        self.profile_phase("load: skeletal mesh bones", phase);
        read
    }

    fn posed_animation(&mut self, animation: ObjectId) -> Option<Arc<PosedAnimation>> {
        if let Some(found) = self.media.bone_data.animations.get(&animation) {
            return found.clone();
        }
        let loader = self.loader.clone();
        let read = loader.loaded_export_by_path_of_class(&self.store.object(animation).path, "MeshAnimation").and_then(|(id, export)| {
            let parsed = self.mesh_animation_at(id, export)?;
            let pkg = &loader.package(id).package;
            let bones = parsed.bones.iter().map(|b| pkg.name(b.name).to_ascii_lowercase()).collect();
            let sequences = parsed.sequences.iter().map(|s| pkg.name(s.name).to_ascii_lowercase()).collect();
            Some(Arc::new(PosedAnimation { animation: parsed, bones, sequences }))
        });
        self.media.bone_data.animations.insert(animation, read.clone());
        read
    }

    pub(crate) fn mesh_animation_at(&mut self, package: PackageId, export: usize) -> Option<Arc<MeshAnimation>> {
        if let Some(found) = self.media.bone_data.parsed.get(package, export) {
            return found;
        }
        let phase = self.profile_start();
        let loaded = self.loader.package(package);
        let read = loaded.package.exports.get(export).and_then(|e| read_mesh_animation(&loaded.package, &loaded.data, e).ok()).map(Arc::new);
        self.media.bone_data.parsed.insert((package, export), read.clone());
        self.profile_phase("load: mesh animation", phase);
        read
    }

    pub fn mesh_animations(&self) -> MeshAnimations {
        self.media.bone_data.parsed.clone()
    }

    pub fn mesh_animation(&mut self, animation: ObjectId) -> Option<Arc<MeshAnimation>> {
        let (package, export) = self.loader.loaded_export_by_path_of_class(&self.store.object(animation).path, "MeshAnimation")?;
        self.mesh_animation_at(package, export)
    }

    /// Reads ahead what is slow on first use (tens of ms on the VM's thread mid-game). For the loading thread, once the map
    /// has begun play; caches only, nothing a script sees changes.
    pub fn prewarm(&mut self) {
        self.media.bone_data.prewarm_animations(&self.loader);
        self.karma_assets();
    }
}

pub(crate) fn reference_frame(bone: usize, q: [f32; 4], position: [f32; 3]) -> Frame {
    let q = if bone == 0 { q } else { [-q[0], -q[1], -q[2], q[3]] };
    let length = q.iter().map(|c| c * c).sum::<f32>().sqrt().max(1e-8);
    Frame::from_quat(q.map(|c| c / length), 1.0, position)
}

fn normalized(v: Vector) -> Vector {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < 1e-8 { v } else { v.map(|c| c / length) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(frame: f32, location: f32, bone_scale: f32, animation: u32) -> PoseKey {
        turned_key(frame, location, bone_scale, animation, &[])
    }

    fn turned_key(frame: f32, location: f32, bone_scale: f32, animation: u32, turns: &[(NameId, Rotator)]) -> PoseKey {
        let mut channel = AnimChannel::default();
        channel.sequence = NameId(1);
        channel.frame = frame;
        let placement = Placement { location: [location, 0.0, 0.0].into(), rotation: Rotator::default(), scale: [1.0; 3].into(), pre_pivot: Vector::default() };
        PoseKey::new(ObjectId(7), &[(&channel, Some(ObjectId(animation)))], [(NameId(2), bone_scale)], turns, placement)
    }

    #[test]
    fn pose_key_matches_as_it_compares() {
        let mut channel = AnimChannel::default();
        channel.sequence = NameId(1);
        let placement = Placement { location: [10.0, 0.0, 0.0].into(), rotation: Rotator::default(), scale: [1.0; 3].into(), pre_pivot: Vector::default() };
        let kept = key(0.0, 10.0, 1.0, 8);
        assert!(kept.matches(ObjectId(7), &[(&channel, Some(ObjectId(8)))], [(NameId(2), 1.0)], &[], placement));
        assert!(!kept.matches(ObjectId(7), &[(&channel, Some(ObjectId(8)))], [(NameId(2), 2.0)], &[], placement));
        assert!(!kept.matches(ObjectId(7), &[(&channel, Some(ObjectId(9)))], [(NameId(2), 1.0)], &[], placement));
        assert!(!kept.matches(ObjectId(7), &[], [(NameId(2), 1.0)], &[], placement));
    }

    #[test]
    fn pose_key_follows_bone_turns() {
        let turn = |yaw: i32| [(NameId(3), Rotator::from([0, yaw, 0]))];
        assert_ne!(key(0.5, 10.0, 1.0, 8), turned_key(0.5, 10.0, 1.0, 8, &turn(-1200)));
        assert_ne!(turned_key(0.5, 10.0, 1.0, 8, &turn(-1200)), turned_key(0.5, 10.0, 1.0, 8, &turn(-1300)));
        assert_eq!(turned_key(0.5, 10.0, 1.0, 8, &turn(-1200)), turned_key(0.5, 10.0, 1.0, 8, &turn(-1200)));
    }

    #[test]
    fn a_turn_changes_the_pose() {
        let identity = [0.0, 0.0, 0.0, 1.0];
        let local = [(identity, [0.0; 3]), (identity, [10.0, 0.0, 0.0]), (identity, [5.0, 0.0, 0.0])];
        let plain = bone_frames(&[0, 0, 1], &local, |_| 1.0);
        let mut turned_local = local;
        turned_local[1].0 = turn_local(identity, [0, -1200, 0]);
        let turned = bone_frames(&[0, 0, 1], &turned_local, |_| 1.0);
        assert_eq!(plain[0], turned[0]);
        assert_eq!(plain[1].origin, turned[1].origin);
        assert_ne!(plain[2].origin, turned[2].origin);
    }

    #[test]
    fn pose_key_matches_only_the_same_inputs() {
        assert_eq!(key(0.5, 10.0, 1.0, 8), key(0.5, 10.0, 1.0, 8));
        assert_ne!(key(0.5, 10.0, 1.0, 8), key(0.25, 10.0, 1.0, 8));
        assert_ne!(key(0.5, 10.0, 1.0, 8), key(0.5, 11.0, 1.0, 8));
        assert_ne!(key(0.5, 10.0, 1.0, 8), key(0.5, 10.0, 2.0, 8));
        assert_ne!(key(0.5, 10.0, 1.0, 8), key(0.5, 10.0, 1.0, 9));
        assert_ne!(key(0.0, 10.0, 1.0, 8), key(-0.0, 10.0, 1.0, 8));
        assert_eq!(key(f32::NAN, 10.0, 1.0, 8), key(f32::NAN, 10.0, 1.0, 8));
    }
}

#[cfg(test)]
mod sphere_tests {
    use super::{Frame, UNIT, transform_sphere};

    fn near(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1e-4)
    }

    #[test]
    fn the_identity_keeps_a_sphere() {
        assert!(near(transform_sphere(&UNIT, [1.0, 2.0, 3.0, 4.0]), [1.0, 2.0, 3.0, 4.0]));
    }

    #[test]
    fn a_sphere_moves_with_the_frame_and_grows_by_its_longest_axis() {
        let frame = Frame { axes: [[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 1.0]], origin: [10.0, 20.0, 30.0] };
        assert!(near(transform_sphere(&frame, [1.0, 1.0, 1.0, 5.0]), [12.0, 23.0, 31.0, 15.0]));
    }

    #[test]
    fn a_turn_does_not_change_the_radius() {
        let frame = Frame { axes: [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]], origin: [0.0; 3] };
        assert!(near(transform_sphere(&frame, [1.0, 0.0, 0.0, 7.0]), [0.0, 1.0, 0.0, 7.0]));
    }
}
