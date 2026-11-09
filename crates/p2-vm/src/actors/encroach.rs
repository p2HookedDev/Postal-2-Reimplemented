use p2_math::Vector;
use crate::volumes::{OldZone, ZoneEvents};
use crate::{Error, Kind, ObjectId, LoaderRef, Prop, Value, Vm};
use super::{Encroachment, cylinders_meet, encroachment_tested, far_move_tests};

impl<L: LoaderRef> Vm<L> {
    /// With the touch notify on. Only the query and overlap test run at `at`: events see `actor` where it
    /// stands, as retail, which places it afterwards. An encroacher's push is zero here, moving nothing.
    pub(super) fn actor_encroachment(&mut self, actor: ObjectId, at: Vector) -> Result<bool, Error> {
        let encroacher = self.is_encroacher(actor);
        let tested = encroachment_tested([
            self.store.flag_of(actor, Prop::bCollideActors),
            self.store.flag_of(actor, Prop::bBlockActors),
            self.store.flag_of(actor, Prop::bBlockPlayers),
            encroacher,
        ]);
        if !tested {
            return Ok(false);
        }
        let found: Vec<ObjectId> = self
            .at_location(actor, at, |vm| vm.encroachment_found(actor))
            .into_iter()
            .filter(|&other| Some(other) != self.level && !self.is_based_on(other, actor))
            .collect();
        for &other in &found {
            if self.store.is_deleted(other) || !self.is_blocked_by(actor, other) {
                continue;
            }
            if encroacher && !self.is_encroacher(other) {
                let still = self.at_location(actor, at, |vm| if vm.store.is_a(other, Kind::Volume) { vm.shapes_overlap(actor, other) } else { vm.primitive_overlaps(actor, other) });
                if !still {
                    continue;
                }
            }
            if self.event_bool(actor, "EncroachingOn", [Value::Object(Some(other))])? {
                return Ok(true);
            }
        }
        self.end_touches(actor)?;
        for other in found {
            if self.store.is_deleted(actor) {
                break;
            }
            if self.store.is_deleted(other) {
                continue;
            }
            if self.is_blocked_by(actor, other) {
                self.event(other, "EncroachedBy", [Value::Object(Some(actor))])?;
            } else {
                self.begin_touch(actor, other)?;
            }
        }
        Ok(false)
    }

    fn at_location<T>(&mut self, actor: ObjectId, at: Vector, f: impl FnOnce(&mut Self) -> T) -> T {
        let from = self.store.vector(actor, Prop::Location);
        self.set_vector(actor, Prop::Location, at);
        let result = f(self);
        self.set_vector(actor, Prop::Location, from);
        result
    }

    fn encroachment_found(&mut self, actor: ObjectId) -> Vec<ObjectId> {
        let encroacher = self.is_encroacher(actor);
        self.overlapping(actor)
            .into_iter()
            .filter(|&other| {
                self.should_trace(other, Some(actor), crate::world::trace::ALL_COLLIDING)
                    && !(encroacher && self.store.flag_of(other, Prop::bWorldGeometry))
            })
            .collect()
    }

    pub(crate) fn encroached(&mut self, actor: ObjectId) -> Option<ObjectId> {
        let blocks = |vm: &Self, object: ObjectId| {
            vm.store.flag_of(object, Prop::bCollideActors) && vm.store.flag_of(object, Prop::bBlockActors) && !vm.store.is_deleted(object)
        };
        if !blocks(self, actor) {
            return None;
        }
        let at = self.store.vector(actor, Prop::Location);
        let nearby = self.colliders_near([at[0], at[1]], [at[0], at[1]], self.store.float_of(actor, Prop::CollisionRadius));
        let found = nearby
            .iter()
            .map(|e| e.actor)
            .find(|&other| other != actor && self.store.is_a(other, Kind::Actor) && blocks(self, other) && self.shapes_overlap(actor, other));
        self.give_entries(nearby);
        found
    }

    pub(crate) fn cylinders_overlap(&self, a: ObjectId, b: ObjectId) -> bool {
        let float = |object: ObjectId, name: Prop| self.store.float_of(object, name);
        let (here, there) = (self.store.vector(a, Prop::Location), self.store.vector(b, Prop::Location));
        cylinders_meet(
            here,
            there,
            float(a, Prop::CollisionRadius) + float(b, Prop::CollisionRadius),
            float(a, Prop::CollisionHeight) + float(b, Prop::CollisionHeight),
        )
    }

    /// `Encroachment::Ignored` is retail's `bIgnoreEncroachment`. No kill here: `EncroachedBy` and `EncroachingOn` are
    /// script's (`Pawn`: the telefrag, and `bWarping` refusing it).
    pub(crate) fn far_move_actor(&mut self, actor: ObjectId, location: Vector, encroachment: Encroachment) -> Result<bool, Error> {
        if self.store.flag_of(actor, Prop::bStatic) || !self.store.flag_of(actor, Prop::bMovable) {
            return Ok(false);
        }
        let flags = [Prop::bCollideWorld, Prop::bCollideWhenPlacing, Prop::bSkipEncroachmentCheck].map(|name| self.store.flag_of(actor, name));
        let (fit, encroach) = far_move_tests(encroachment, flags);
        let mut to = location;
        if fit {
            let (radius, height) = self.store.collision_size(actor);
            if !crate::find_spot::find_spot(self, Vector::new(radius, radius, height), &mut to) {
                return Ok(false);
            }
        }
        if encroach && self.actor_encroachment(actor, to)? {
            return Ok(false);
        }
        if !self.store.is_deleted(actor) {
            self.place_far_moved(actor, to, encroachment)?;
        }
        Ok(true)
    }

    /// `bJustTeleported` makes a warp zone's `ActorEntered` skip the actor, so one warped into the other
    /// side's zone is not sent back. `Attached` actors move on their own, result ignored; not turned.
    pub fn place_far_moved(&mut self, actor: ObjectId, location: Vector, encroachment: Encroachment) -> Result<(), Error> {
        self.store.set(actor, Prop::bJustTeleported, Value::Bool(true));
        let from = self.store.vector(actor, Prop::Location);
        for other in self.store.object_list(actor, Prop::Attached) {
            if !self.store.is_deleted(other) {
                let to = (self.store.vector(other, Prop::Location) + location) - from;
                self.far_move_actor(other, to, encroachment)?;
            }
        }
        self.set_vector(actor, Prop::Location, location);
        self.relocate(actor);
        self.set_zone(actor, ZoneEvents::Sent, OldZone::Region)?;
        Ok(())
    }
}
