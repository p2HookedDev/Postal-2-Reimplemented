use super::*;

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

pub(super) fn follow_bones(
    mut commands: Commands,
    poses: Res<Poses>,
    play: Res<Play>,
    mut places: ResMut<AttachmentPlaces>,
    mut attached: Query<(Entity, Ref<FollowBone>, &mut Transform, &mut Visibility)>,
) {
    let world = play.world();
    for (entity, follow, mut transform, mut shown) in &mut attached {
        let changed = world.is_some_and(|world| world.changed(follow.base) || world.changed(follow.actor) || world.glides(follow.base));
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
            Some(base) => {
                let (location, rotation) = world.map_or((base.location, base.rotation), |world| world.shown_place(follow.base, base));
                FollowActor { pre_pivot: base.pre_pivot, ..follow.base_follow }.mesh_matrix(location, rotation).unwrap_or(follow.body)
            }
            None => follow.body,
        };
        let frame = follow.frame(body, bone);
        places.record(follow.actor, follow.base, frame);
        transform.set_if_neq(follow.transform_at(frame));
    }
}

/// They sit at the bone's current frame instead of their actor's `Location`, which the VM does not move with the bone.
pub(super) fn place_attached(
    mut poses: ResMut<Poses>,
    play: Res<Play>,
    mut places: ResMut<AttachmentPlaces>,
    mut models: Query<(Entity, Ref<FollowActor>, &mut Transform)>,
    on_bones: Query<&FollowBone>,
    mut buffers: Local<AttachedBuffers>,
) {
    let Some(world) = play.world() else { return };
    let poses = &mut *poses;
    let AttachedBuffers { generation, bone_bodies, bases, due_models, emitters } = &mut *buffers;
    if *generation != play.generation() {
        *generation = play.generation();
        poses.slots.clear();
    }
    bone_bodies.clear();
    bases.clear();
    due_models.clear();
    emitters.clear();
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
    emitters.extend(world.watched().filter_map(|actor| {
        let attachment = world.actor(actor)?.attachment.as_ref()?;
        (world.changed(actor) || world.changed(attachment.base) || changed.contains(&attachment.base) || bone_bodies.contains_key(&attachment.base))
            .then_some((actor, attachment.base))
    }));
    for (entity, follow, _) in &models {
        if let Some(attachment) = due(&follow, follow.is_added()) {
            due_models.push(entity);
            bases.insert(attachment.base, None);
        }
    }
    bases.extend(emitters.iter().map(|&(_, base)| (base, None)));
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
    for &(actor, _) in emitters.iter() {
        if let Some(attachment) = world.actor(actor).and_then(|actor| actor.attachment.as_ref())
            && let Some(frame) = frame_of(attachment)
        {
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
pub(super) struct AttachedBuffers {
    generation: u64,
    bone_bodies: FxHashMap<ObjectId, Mat4>,
    bases: FxHashMap<ObjectId, Option<FollowActor>>,
    due_models: Vec<Entity>,
    emitters: Vec<(ObjectId, ObjectId)>,
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

pub fn attachment_frame(body: Mat4, bone: Mat4, relative_location: Vector, relative_rotation: Rotator) -> Mat4 {
    let (_, rotation, position) = (body * bone).to_scale_rotation_translation();
    // A bone scaled to nothing has no rotation (NaN from glam).
    let rotation = if rotation.is_finite() { rotation } else { Quat::IDENTITY };
    Mat4::from_rotation_translation(rotation, position)
        * Mat4::from_translation(Vec3::from_array(relative_location.into()))
        * p2_bevy_assets::rotator_matrix(relative_rotation)
}
