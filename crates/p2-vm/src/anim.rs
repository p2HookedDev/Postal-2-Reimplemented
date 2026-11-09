use std::collections::HashMap;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use p2_assets::pose::LocalBone;
use p2_package::PackageId;
use p2_script::{ScriptObject, read_export};

use p2_math::Rotator;

use crate::{Error, LoaderRef, NameId, ObjectId, Value, Vm, Prop};

mod natives;
mod playback;
mod notify;
mod sequences;

const MAX_CHANNEL: i32 = 256;
const START_LOOP: f32 = 1e-4;
const START_ONCE: f32 = 1e-3;
const MAX_PASSES: usize = 4;
const MIN_SPEED_RATE: f32 = 0.3;
const DEFAULT_SEQUENCE_RATE: f32 = 30.0;

#[derive(Debug, Clone)]
struct Sequence {
    name: NameId,
    groups: Vec<NameId>,
    frames: f32,
    rate: f32,
    has_notifies: bool,
    /// Time as a fraction of the sequence, and the notify export (`None`: none, or package not loaded).
    notifies: Vec<(f32, Option<(PackageId, usize)>)>,
}

/// Frames are fractions of the sequence: 0 is its first frame, 1 is one past its last.
/// A negative frame tweens from the previous pose to frame 0.
#[derive(Debug, Clone, Default, PartialEq)]
#[expect(clippy::struct_excessive_bools, reason = "mirrors `MeshAnimChannel`")]
pub struct AnimChannel {
    pub sequence: NameId,
    pub animation: usize,
    /// Sequence fractions per second; negative means scaled by the actor's speed.
    pub rate: f32,
    pub frame: f32,
    pub last: f32,
    pub tween_rate: f32,
    /// The rate before the latest `PlayAnim`, used for a tween time of -1.
    pub old_rate: f32,
    pub orig_rate: f32,
    pub looping: bool,
    pub has_notifies: bool,
    pub notify_disabled: bool,
    pub blend_alpha: f32,
    pub blend_in_time: f32,
    pub blend_out_time: f32,
    pub blend_bone: NameId,
    pub global_pose: bool,
    blending: Option<(f32, f32)>,
    tween_from: Option<Arc<[LocalBone]>>,
    tween_start: f32,
}

impl AnimChannel {
    pub fn tween(&self) -> Option<(&Arc<[LocalBone]>, f32)> {
        self.tween_from.as_ref().filter(|_| self.frame < 0.0).map(|from| (from, 1.0 - self.frame / self.tween_start))
    }

    fn begin_tween(&mut self, from: Option<Arc<[LocalBone]>>) {
        (self.tween_from, self.tween_start) = if self.frame < 0.0 { (from, self.frame) } else { (None, 0.0) };
    }
}

#[derive(Debug, Clone)]
struct Instance {
    mesh: ObjectId,
    linked: Vec<ObjectId>,
    channels: Vec<AnimChannel>,
    bone_scales: Vec<(NameId, f32)>,
    bone_turns: Vec<(NameId, Rotator)>,
    rates: Vec<(NameId, f32)>,
}

impl Instance {
    fn new(mesh: ObjectId) -> Self {
        Self { mesh, linked: Vec::new(), channels: Vec::new(), bone_scales: Vec::new(), bone_turns: Vec::new(), rates: Vec::new() }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Anims {
    instances: FxHashMap<ObjectId, Instance>,
    sequences: FxHashMap<ObjectId, Option<Arc<[Sequence]>>>,
    default_anims: HashMap<ObjectId, Option<ObjectId>>,
    probe_masks: FxHashMap<(PackageId, usize), (u64, u64)>,
    notify_objects: HashMap<(PackageId, usize), Option<ObjectId>>,
    skeletal_meshes: FxHashMap<ObjectId, bool>,
}

impl Anims {
    pub(crate) fn forget(&mut self, actors: &std::collections::HashSet<ObjectId>) {
        self.instances.retain(|actor, _| !actors.contains(actor));
    }

    /// The loader changed: a mesh stub may have a skeletal mesh export now.
    pub(crate) fn forget_skeletal_meshes(&mut self) {
        self.skeletal_meshes.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Playback {
    Once,
    Looping,
}

impl<L: LoaderRef> Vm<L> {
    pub fn anim_channel(&self, actor: ObjectId, channel: usize) -> Option<(&AnimChannel, Option<ObjectId>)> {
        let instance = self.instance(actor)?;
        let state = instance.channels.get(channel)?;
        Some((state, instance.linked.get(state.animation).copied()))
    }

    pub fn bone_scales(&self, actor: ObjectId) -> impl Iterator<Item = (NameId, f32)> + '_ {
        self.instance(actor).into_iter().flat_map(|instance| instance.bone_scales.iter().copied()).filter(|&(bone, _)| bone != NameId::NONE)
    }

    pub fn bone_rotations(&self, actor: ObjectId) -> &[(NameId, Rotator)] {
        self.instance(actor).map_or(&[], |instance| instance.bone_turns.as_slice())
    }

    pub(crate) fn set_twist_look(&mut self, pawn: ObjectId, look: i32) {
        if !self.store.flag_of(pawn, Prop::bDoTorsoTwist) {
            return;
        }
        let bones = [Prop::HeadBone, Prop::SpineBone1, Prop::SpineBone2].map(|p| match self.store.get(pawn, p) {
            Some(Value::Name(name)) => name,
            _ => NameId::NONE,
        });
        let Some(instance) = self.instance_mut(pawn) else { return };
        for (bone, share) in bones.into_iter().zip([-0.45, -0.35, -0.2]) {
            let turn = [0, (f64::from(look) * share) as i32, 0];
            match instance.bone_turns.iter_mut().find(|(b, _)| *b == bone) {
                Some(slot) => slot.1 = turn.into(),
                None => instance.bone_turns.push((bone, turn.into())),
            }
        }
    }

    pub(crate) fn is_probing(&mut self, object: ObjectId, bit: u32) -> Result<bool, Error> {
        let Some(class) = self.store.object(object).class else { return Ok(false) };
        let class = (self.store.class(class).package, self.store.class(class).export);
        let node = self.script.states.objects.get(&object).map_or(class, |s| s.state);
        let (class_probe, _) = self.probe_masks(class)?;
        let (probe, ignore) = self.probe_masks(node)?;
        let disabled = self.script.states.probes_disabled.get(&object).copied().unwrap_or(0);
        Ok((class_probe | probe) & ignore & !disabled & (1 << bit) != 0)
    }

    fn probe_masks(&mut self, node: (PackageId, usize)) -> Result<(u64, u64), Error> {
        if let Some(&masks) = self.media.anims.probe_masks.get(&node) {
            return Ok(masks);
        }
        let loaded = self.loader.package(node.0);
        let masks = match read_export(&loaded.package, &loaded.data, node.1)? {
            Some(ScriptObject::State(state)) => (state.probe_mask, state.ignore_mask),
            Some(ScriptObject::Class(class)) => (class.base.probe_mask, class.base.ignore_mask),
            _ => (0, u64::MAX),
        };
        self.media.anims.probe_masks.insert(node, masks);
        Ok(masks)
    }

    pub(crate) fn ensure_instance(&mut self, actor: ObjectId) -> bool {
        self.instance_mut(actor).is_some()
    }

    fn instance(&self, actor: ObjectId) -> Option<&Instance> {
        let instance = self.media.anims.instances.get(&actor)?;
        (Some(instance.mesh) == self.skeletal_mesh(actor)).then_some(instance)
    }

    fn instance_mut(&mut self, actor: ObjectId) -> Option<&mut Instance> {
        let mesh = self.skeletal_mesh_kept(actor)?;
        let instance = self.media.anims.instances.entry(actor).or_insert_with(|| Instance::new(mesh));
        if instance.mesh != mesh {
            *instance = Instance::new(mesh);
        }
        Some(instance)
    }

    pub(crate) fn channel(&self, actor: ObjectId, channel: i32) -> Option<&AnimChannel> {
        self.instance(actor)?.channels.get(usize::try_from(channel).ok()?)
    }

    pub(crate) fn channel_mut(&mut self, actor: ObjectId, channel: i32) -> Option<&mut AnimChannel> {
        let mesh = self.skeletal_mesh_kept(actor)?;
        let instance = self.media.anims.instances.get_mut(&actor).filter(|i| i.mesh == mesh)?;
        instance.channels.get_mut(usize::try_from(channel).ok()?)
    }

    pub(crate) fn valid_channel(&mut self, actor: ObjectId, channel: i32) -> Option<&mut AnimChannel> {
        if !(0..=MAX_CHANNEL).contains(&channel) {
            return None;
        }
        let index = channel as usize;
        let instance = self.instance_mut(actor)?;
        if instance.channels.len() <= index {
            instance.channels.resize(index + 1, AnimChannel::default());
        }
        instance.channels.get_mut(index)
    }
}

fn channel_animating(state: &AnimChannel) -> bool {
    let rate = if state.frame < 0.0 { state.tween_rate } else { state.rate };
    state.sequence != NameId::NONE && rate != 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tween_runs_while_the_frame_is_negative() {
        let from: Arc<[LocalBone]> = Arc::new([([0.0, 0.0, 0.0, 1.0], [0.0; 3])]);
        let mut channel = AnimChannel { frame: -0.2, ..AnimChannel::default() };
        channel.begin_tween(Some(from));
        assert_eq!(channel.tween().map(|(_, weight)| weight), Some(0.0));
        channel.frame = -0.05;
        assert_eq!(channel.tween().map(|(_, weight)| weight), Some(0.75));
        channel.frame = 0.0;
        assert!(channel.tween().is_none());
    }

    #[test]
    fn a_start_without_a_tween_keeps_no_pose() {
        let from: Arc<[LocalBone]> = Arc::new([([0.0, 0.0, 0.0, 1.0], [0.0; 3])]);
        let mut channel = AnimChannel { frame: 0.001, ..AnimChannel::default() };
        channel.begin_tween(Some(from));
        assert!(channel.tween_from.is_none());
    }
}
