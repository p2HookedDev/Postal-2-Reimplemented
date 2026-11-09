use std::sync::Arc;
use p2_assets::pose::Frame;
use p2_physics::RagdollParams;
use p2_math::Vector;
use crate::enums::EPhysics;
use p2_math::matrix_to_quat;
use crate::{Encroachment, Error, LoaderRef, ObjectId, Value, Vm, Prop, Kind};
use super::{CORPSE_FRICTION, Corpse, MAX_JOINT_ERROR, MAX_SPEED, Ragdoll, ResolvedScales};
use super::rig::{build, cut_of, frame_scale, orthonormal, scale_frames};

impl<L: LoaderRef> Vm<L> {
    /// Now, so that `KSetSkelVel` and `KAddImpulse` right after it reach the bodies.
    pub(crate) fn ragdoll_physics_changed(&mut self, actor: ObjectId, old: Option<EPhysics>) {
        let physics = self.store.physics_of(actor);
        if old == Some(EPhysics::KarmaRagDoll) && physics != Some(EPhysics::KarmaRagDoll) {
            self.end_ragdoll(actor, Corpse::Kept);
        }
        if physics == Some(EPhysics::KarmaRagDoll) && !self.karma.ragdolls.live.contains_key(&actor) {
            let ragdolls = &mut self.karma.ragdolls;
            ragdolls.frozen.remove(&actor);
            ragdolls.failed.remove(&actor);
            let phase = self.profile_start();
            self.init_ragdoll(actor, None);
            self.profile_phase("ragdoll made", phase);
        }
    }

    pub(crate) fn ragdolls_before_step(&mut self, actors: Vec<ObjectId>) {
        let gone: Vec<ObjectId> = self
            .karma
            .ragdolls
            .live
            .keys()
            .copied()
            .filter(|&a| self.store.is_deleted(a) || self.store.physics_of(a) != Some(EPhysics::KarmaRagDoll))
            .collect();
        for actor in gone {
            let corpse = if self.store.is_deleted(actor) { Corpse::Gone } else { Corpse::Kept };
            self.end_ragdoll(actor, corpse);
        }
        let ragdolls = &self.karma.ragdolls;
        let destroyed: Vec<ObjectId> =
            ragdolls.kept.keys().chain(&ragdolls.frozen).chain(&ragdolls.failed).copied().filter(|&a| self.store.is_deleted(a)).collect();
        for actor in destroyed {
            let ragdolls = &mut self.karma.ragdolls;
            ragdolls.kept.remove(&actor);
            ragdolls.frozen.remove(&actor);
            ragdolls.failed.remove(&actor);
        }
        let ragdolls = &self.karma.ragdolls;
        let wanted: Vec<ObjectId> = actors
            .into_iter()
            .filter(|a| !ragdolls.live.contains_key(a) && !ragdolls.frozen.contains(a) && !ragdolls.failed.contains(a))
            .collect();
        for actor in wanted {
            let phase = self.profile_start();
            self.init_ragdoll(actor, None);
            self.profile_phase("ragdoll made", phase);
        }
        self.rebuild_cut_ragdolls();
    }

    fn rebuild_cut_ragdolls(&mut self) {
        let changed: Vec<(ObjectId, ResolvedScales)> = self
            .karma
            .ragdolls
            .live
            .iter()
            .filter_map(|(&actor, ragdoll)| {
                let scales: Vec<_> = self.bone_scales(actor).collect();
                (!ragdoll.scales.holds(&scales)).then(|| {
                    let own = self.scales_on_mesh(&scales, &ragdoll.mesh);
                    let cut = cut_of(own.as_deref(), &ragdoll.mesh.parents);
                    (actor, ResolvedScales { read: scales.iter().map(|&(bone, scale)| (bone, scale.to_bits())).collect(), own, cut })
                })
            })
            .collect();
        for (actor, scales) in changed {
            if let Some(ragdoll) = self.karma.ragdolls.live.get_mut(&actor) {
                ragdoll.scales = scales;
            }
        }
        let cut: Vec<(ObjectId, usize)> = self
            .karma
            .ragdolls
            .live
            .iter()
            .filter(|(_, ragdoll)| !ragdoll.cut_refused && !ragdoll.scales.cut.is_empty())
            .filter_map(|(&actor, ragdoll)| {
                let cut = &ragdoll.scales.cut;
                let is_cut = |bone: usize| cut.get(bone).copied().unwrap_or(false);
                let bodied = || ragdoll.bodies.iter().enumerate().filter(|(_, body)| body.is_some()).map(|(bone, _)| bone);
                bodied().any(is_cut).then(|| (actor, bodied().filter(|&b| !is_cut(b)).count()))
            })
            .collect();
        for (actor, kept) in cut {
            let Some(ragdoll) = self.karma.ragdolls.live.get_mut(&actor) else { continue };
            if kept < 2 {
                ragdoll.cut_refused = true;
                continue;
            }
            let frames = ragdoll.frames.to_vec();
            self.end_ragdoll(actor, Corpse::Gone);
            self.init_ragdoll(actor, Some(frames));
        }
    }

    pub(crate) fn ragdolls_after_step(&mut self) -> Vec<(String, Error)> {
        let mut errors = Vec::new();
        let actors: Vec<ObjectId> = self.karma.ragdolls.live.keys().copied().collect();
        for actor in actors {
            let Some((location, velocity, torn)) = self.pose_ragdoll(actor) else { continue };
            self.set_vector(actor, Prop::Velocity, velocity);
            self.store.set(actor, Prop::Rotation, Value::Rotator([0; 3].into()));
            if self.store.vector(actor, Prop::Location) != location
                && let Err(e) = self.place_far_moved(actor, location, Encroachment::Ignored).and_then(|()| self.update_touching(actor))
            {
                errors.push((self.store.object(actor).path.clone(), e));
            }
            if torn && let Err(e) = self.event(actor, "KExcessiveJointError", Vec::new()) {
                errors.push((self.store.object(actor).path.clone(), e));
            }
        }
        errors
    }

    fn pose_ragdoll(&mut self, actor: ObjectId) -> Option<(Vector, Vector, bool)> {
        let world = self.karma.world.as_mut()?;
        let ragdoll = self.karma.ragdolls.live.get_mut(&actor)?;
        ragdoll.fresh = false;
        let asleep = ragdoll.all.iter().all(|&id| world.is_sleeping(id));
        if asleep && ragdoll.posed_asleep.is_some() {
            let root = ragdoll.bodies.iter().flatten().next().copied()?;
            return Some(((world.pose(root)?.0), [0.0; 3].into(), ragdoll.posed_asleep.unwrap_or(false)));
        }
        for &id in ragdoll.all.iter() {
            world.limit_speed(id, MAX_SPEED);
        }
        let (mesh, scale) = (ragdoll.mesh.clone(), ragdoll.scale);
        let root = ragdoll.root_offset.and_then(|(anchor, offset)| {
            let (location, rotation) = world.pose(ragdoll.bodies[anchor]?)?;
            Some(Frame::from_quat(rotation, scale, location.into()).then(&offset))
        });
        let frames = Arc::make_mut(&mut ragdoll.frames);
        for bone in 0..frames.len() {
            let frame = match ragdoll.bodies[bone].and_then(|id| world.pose(id)) {
                Some((location, rotation)) => Frame::from_quat(rotation, scale, location.into()),
                None => {
                    let parent = mesh.parents.get(bone).copied().filter(|&p| p < bone);
                    let local = mesh.reference.get(bone).copied().unwrap_or(Frame::IDENTITY);
                    match parent {
                        Some(parent) => frames[parent].then(&local),
                        None => root.unwrap_or(frames[bone]),
                    }
                }
            };
            frames[bone] = frame;
        }
        let unit = |frame: &Frame| Frame { axes: frame.axes.map(|a| a.map(|c| c / scale)), origin: frame.origin };
        let torn = ragdoll.joints.iter().any(|joint| {
            let (a, b) = (unit(&ragdoll.frames[joint.child]).transform_point(joint.at_child.into()), unit(&ragdoll.frames[joint.parent]).transform_point(joint.at_parent.into()));
            (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>() > MAX_JOINT_ERROR * MAX_JOINT_ERROR
        });
        world.parts_into(&ragdoll.all, &mut ragdoll.parts);
        let root = ragdoll.bodies.iter().flatten().next().copied()?;
        ragdoll.posed_asleep = asleep.then_some(torn);
        let velocity = if asleep { [0.0; 3] } else { world.velocity(root)?.into() };
        Some((world.pose(root)?.0, velocity.into(), torn))
    }

    fn init_ragdoll(&mut self, actor: ObjectId, pose: Option<Vec<Frame>>) {
        // A corpse made a ragdoll again starts from where it lay, not from its animation.
        let pose = pose.or_else(|| self.karma.ragdolls.kept.get(&actor).map(|(_, frames)| frames.to_vec()));
        let params = self.store.object_of(actor, Prop::KParams).filter(|&k| self.store.is_a(k, Kind::KarmaParamsSkel));
        let Some(params) = params else {
            self.diagnostics.log.push("(Karma): KInitSkeletonKarma: No KParams, or KParams is not a KarmaParamsSkel.".to_owned());
            self.karma.ragdolls.failed.insert(actor);
            return;
        };
        let skeleton = match self.store.get(params, Prop::KSkeleton) {
            Some(Value::Name(name)) => self.store.name(name).to_ascii_lowercase(),
            _ => String::new(),
        };
        let asset = self.karma_assets().get(&skeleton).cloned();
        let (Some(asset), Some((mesh, animated))) = (asset, self.world_bone_frames(actor)) else {
            self.diagnostics.log.push("(Karma): KInitSkeletonKarma: Asset not found.".to_owned());
            self.karma.ragdolls.failed.insert(actor);
            return;
        };
        // Or the cut limb shows again until the first step.
        let frames: Arc<[Frame]> = match pose.filter(|pose| pose.len() == animated.len()) {
            Some(mut pose) => {
                if let Some(own) = self.bone_scale_factors(actor, &mesh) {
                    scale_frames(&mut pose, &own, &mesh.parents);
                }
                pose.into()
            }
            None => animated,
        };
        let own = self.bone_scale_factors(actor, &mesh);
        let cut = cut_of(own.as_deref(), &mesh.parents);
        let k = |name: Prop, default: f32| self.store.get(params, name).as_ref().map_or(default, crate::interp::float);
        let (linear_damping, angular_damping) = (k(Prop::KLinearDamping, 0.2), k(Prop::KAngularDamping, 0.2));
        // `KRestitution` made corpses hop.
        let (friction, restitution) = (CORPSE_FRICTION, 0.0);
        let asleep = !self.store.flag_of(params, Prop::KStartEnabled);
        let velocity = self.store.vector(actor, Prop::Velocity);
        let Some(built) = build(&asset, &mesh, &frames, velocity, &cut) else {
            self.diagnostics.log.push(format!("(Karma): KInitSkeletonKarma: no part of {} fits a bone of {}", asset.id, self.store.object(actor).path));
            self.karma.ragdolls.failed.insert(actor);
            return;
        };
        let params = RagdollParams { parts: built.parts, joints: built.joint_params, linear_damping, angular_damping, friction, restitution, asleep };
        self.karma_world(actor);
        let Some(world) = self.karma.world.as_mut() else { return };
        let all = world.add_ragdoll(&params);
        let parts = world.parts(&all);
        let bodies = built.part_of_bone.iter().map(|part| part.map(|p| all[p])).collect();
        for &id in &all {
            self.karma.actors.insert(id, actor);
        }
        let scale = frame_scale(&frames[0]);
        let root_offset = built.part_of_bone.iter().position(Option::is_some).and_then(|anchor| {
            let body = Frame::from_quat(matrix_to_quat(orthonormal(frames[anchor].axes.map(Vector::from))), scale, frames[anchor].origin);
            Some((anchor, body.inverse()?.then(&frames[0])))
        });
        let ragdoll =
            Ragdoll { bodies, all: all.into(), joints: built.joints, mesh, scale, frames, fresh: true, posed_asleep: None, parts, scales: ResolvedScales::default(), cut_refused: false, root_offset };
        self.karma.ragdolls.live.insert(actor, ragdoll);
        self.karma.ragdolls.kept.remove(&actor);
        for (property, value) in [("bCollideActors", true), ("bBlockActors", false), ("bBlockPlayers", false), ("bCollideWorld", false), ("bProjTarget", true)] {
            self.store.set(actor, property, Value::Bool(value));
        }
        self.relocate(actor);
        if let Err(e) = self.update_touching(actor) {
            self.diagnostics.log.push(format!("{}: {e}", self.store.object(actor).path));
        }
    }

    pub(super) fn end_ragdoll(&mut self, actor: ObjectId, corpse: Corpse) {
        let Some(ragdoll) = self.karma.ragdolls.live.remove(&actor) else { return };
        if let Some(world) = self.karma.world.as_mut() {
            world.remove_ragdoll(&ragdoll.all);
        }
        for id in ragdoll.all.iter() {
            self.karma.actors.remove(id);
        }
        if corpse == Corpse::Kept {
            self.karma.ragdolls.kept.insert(actor, (ragdoll.mesh, ragdoll.frames));
        }
    }
}
