use crate::native_id::Native;
use crate::physics::Bases;
use crate::senses::SightCheck;
use crate::{Encroachment, Error, LoaderRef, ObjectId, Prop, Value, Vm};
use super::{bool_arg, float_arg, int_arg, is_struct_value, name_arg, object_arg, rot3, vec3};

pub(super) fn placement_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::SetLocation => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            if matches!(args.first(), Some(Value::Map)) {
                Value::Bool(true)
            } else {
                Value::Bool(vm.far_move_actor(this, vec3(args.first()), if matches!(args.get(1), Some(Value::Bool(true))) { Encroachment::Ignored } else { Encroachment::Checked })?)
            }
        }
        Native::Move => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            vm.move_actor(this, vec3(args.first()), Bases::Block)?;
            Value::Bool(true)
        }
        Native::SetBase => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            let base = match args.first() {
                Some(&Value::Object(base)) => base,
                _ => None,
            };
            let floor = match args.get(1) {
                Some(value) if is_struct_value(value) => vec3(Some(value)),
                _ => [0.0, 0.0, 1.0].into(),
            };
            vm.set_base(this, base, floor)?;
            Value::Bool(true)
        }
        Native::SetOwner => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            let owner = match args.first() {
                Some(&Value::Object(owner)) => owner,
                _ => None,
            };
            vm.set_owner(this, owner)?;
            Value::Bool(true)
        }
        Native::SetCollision | Native::SetCollisionSize | Native::SetDrawScale | Native::SetDrawScale3D | Native::SetStaticMesh | Native::SetDrawType
        | Native::SetRotation | Native::SetRelativeLocation | Native::SetRelativeRotation | Native::LinkMesh => {
            let Some(this) = this else { return Ok(Some(Value::Bool(false))) };
            set_actor_properties(vm, native, this, args)?;
            Value::Bool(true)
        }
        Native::AttachToBone | Native::DetachFromBone => {
            let (Some(this), Some(&Value::Object(Some(attachment)))) = (this, args.first()) else {
                return Ok(Some(Value::Bool(false)));
            };
            // The bone first: `SetBase` keeps the offset only of an actor that is not on a bone.
            if native == Native::AttachToBone {
                // Only attaches to a bone or alias of a skeletal mesh; a loose bolt-on is destroyed a second later.
                // A mesh whose package is not loaded has its bones unknown, not missing, so only a read mesh refuses.
                let bone = name_arg(args, 1);
                let wanted = vm.store.name(bone).to_ascii_lowercase();
                let matched = vm.skeletal_mesh(this).is_some_and(|mesh| {
                    vm.posed_mesh(mesh).is_none_or(|mesh| mesh.match_ref_bone(&wanted).is_some())
                });
                if !matched || vm.store.is_deleted(attachment) {
                    return Ok(Some(Value::Bool(false)));
                }
                vm.store.set(attachment, Prop::AttachmentBone, Value::Name(bone));
                vm.set_base(attachment, Some(this), [0.0, 0.0, 1.0].into())?;
            } else if vm.store.object_of(attachment, Prop::Base) == Some(this) {
                vm.set_base(attachment, None, [0.0, 0.0, 1.0].into())?;
            }
            Value::Bool(true)
        }
        Native::OnlyAffectPawns => {
            if let Some(this) = this {
                vm.store.set(this, Prop::bOnlyAffectPawns, Value::Bool(bool_arg(args, 0)));
            }
            return Ok(None);
        }
        Native::Warp | Native::UnWarp => {
            if let Some(this) = this
                && args.len() >= 3
            {
                let coords = crate::volumes::coords_of(vm.store.get(this, "WarpCoords").as_ref());
                let carry = if native == Native::Warp { crate::volumes::warp } else { crate::volumes::carry };
                let (location, velocity, rotation) = carry(coords, vec3(args.first()), vec3(args.get(1)), rot3(args.get(2)));
                args[0] = Value::Vector(location);
                args[1] = Value::Vector(velocity);
                args[2] = Value::Rotator(rotation);
            }
            return Ok(None);
        }
        Native::SetPhysics => {
            if let Some(this) = this {
                vm.set_physics_byte(this, int_arg(args, 0) as u8, None)?;
            }
            return Ok(None);
        }
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

pub(super) fn sense_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &mut [Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::AddController | Native::RemoveController => {
            controller_list(vm, native, this);
            return Ok(None);
        }
        Native::MakeNoise => {
            if let Some(this) = this {
                vm.make_noise(this, float_arg(args, 0))?;
            }
            return Ok(None);
        }
        Native::LineOfSightTo => Value::Bool(match (this, object_arg(args, 0)) {
            (Some(this), Some(other)) => vm.line_of_sight_to(this, other, SightCheck::Full),
            _ => false,
        }),
        Native::PlayerCanSeeMe => Value::Bool(this.is_some_and(|this| vm.player_can_see_me(this))),
        Native::Encompasses => Value::Bool(match (this, object_arg(args, 0)) {
            (Some(this), Some(other)) => {
                let location = vm.store.vec3_of(other, Prop::Location);
                vm.encompasses(this, location)
            }
            _ => false,
        }),
        Native::PickTarget => Value::Object(this.and_then(|this| vm.pick_target(this, args))),
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

pub(super) fn mesh_native<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>, args: &[Value]) -> Result<Option<Value>, Error> {
    let value = match native {
        Native::GetRenderBoundingSphere => match this {
            Some(this) => vm.render_bounding_sphere(this),
            None => return Ok(None),
        },
        Native::GetBoneCoords => match this {
            Some(this) => vm.get_bone_coords(this, name_arg(args, 0)),
            None => return Ok(None),
        },
        Native::GetBoneRotation => match this {
            Some(this) => vm.get_bone_rotation(this, name_arg(args, 0)),
            None => return Ok(None),
        },
        Native::SetTwistLook => {
            if let Some(this) = this {
                vm.set_twist_look(this, int_arg(args, 1));
            }
            return Ok(None);
        }
        _ => return Err(Error::UnknownNative(format!("{native:?}"))),
    };
    Ok(Some(value))
}

fn set_actor_properties<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: ObjectId, args: &[Value]) -> Result<(), Error> {
    let properties: &[Prop] = match native {
        Native::SetCollision => &[Prop::bCollideActors, Prop::bBlockActors, Prop::bBlockPlayers],
        Native::SetCollisionSize => &[Prop::CollisionRadius, Prop::CollisionHeight],
        Native::SetDrawScale => &[Prop::DrawScale],
        Native::SetDrawScale3D => &[Prop::DrawScale3D],
        Native::SetStaticMesh => &[Prop::StaticMesh],
        Native::SetDrawType => &[Prop::DrawType],
        Native::SetRotation => &[Prop::Rotation],
        Native::SetRelativeLocation => &[Prop::RelativeLocation],
        Native::SetRelativeRotation => &[Prop::RelativeRotation],
        Native::LinkMesh => &[Prop::Mesh],
        _ => &[],
    };
    // Collision turned off untouches before the flags change, as retail.
    let collided = (native == Native::SetCollision).then(|| vm.store.flag_of(this, Prop::bCollideActors));
    // As retail, `SetRotation` is a zero-delta move: the actors based on it turn with it.
    let turned_from = (native == Native::SetRotation).then(|| vm.store.rotator_of(this, Prop::Rotation));
    if let Some(was) = collided {
        let now = match args.first() {
            Some(Value::Bool(now)) => *now,
            _ => was,
        };
        vm.set_collision_before(this, was, now)?;
    }
    for (&property, value) in properties.iter().zip(args) {
        if !matches!(value, Value::Map) {
            vm.store.set(this, property, value.clone());
        }
    }
    if matches!(native, Native::SetRotation | Native::SetDrawScale | Native::SetDrawScale3D) {
        vm.volume_moved(this);
    }
    // A changed primitive (scale, mesh, rotation) of a moving actor changes its reach: re-placed too (static ones keep
    // theirs).
    let collision = matches!(native, Native::SetCollision | Native::SetCollisionSize);
    let reshaped = matches!(native, Native::SetDrawScale | Native::SetDrawScale3D | Native::SetStaticMesh | Native::SetDrawType | Native::SetRotation | Native::LinkMesh);
    if collision || (reshaped && !vm.store.flag_of(this, Prop::bStatic)) {
        vm.relocate(this);
    }
    if let Some(old_rotation) = turned_from {
        vm.turn_attached(this, old_rotation)?;
    }
    if let Some(was) = collided {
        let now = vm.store.flag_of(this, Prop::bCollideActors);
        vm.set_collision_after(this, was, now)?;
    } else if collision {
        vm.update_touching(this)?;
    }
    Ok(())
}

fn controller_list<L: LoaderRef>(vm: &mut Vm<L>, native: Native, this: Option<ObjectId>) {
    let Some(this) = this else { return };
    let Some(level) = vm.store.object_of(this, Prop::Level) else { return };
    let mut previous: Option<ObjectId> = None;
    let mut current = vm.store.object_of(level, Prop::ControllerList);
    let mut guard = 0;
    while let Some(controller) = current {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        let after = vm.store.object_of(controller, Prop::nextController);
        if controller == this {
            match previous {
                Some(previous) => vm.store.set(previous, Prop::nextController, Value::Object(after)),
                None => vm.store.set(level, Prop::ControllerList, Value::Object(after)),
            };
            break;
        }
        previous = Some(controller);
        current = after;
    }
    if native == Native::AddController {
        let first = vm.store.object_of(level, Prop::ControllerList);
        vm.store.set(this, Prop::nextController, Value::Object(first));
        vm.store.set(level, Prop::ControllerList, Value::Object(Some(this)));
    } else {
        vm.store.set(this, Prop::nextController, Value::Object(None));
    }
}
