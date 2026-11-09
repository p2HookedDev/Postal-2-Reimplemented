//! `PHYS_RootMotion` never moves an actor by its root bone. Retail gives NaN at delta 0; we write 0.
//! No script sets this mode; only maps or mods reach it (unverified).

use crate::{LoaderRef, ObjectId, Prop, Vm};

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn phys_root_motion(&mut self, actor: ObjectId) {
        if !self.ensure_instance(actor) {
            self.set_vector(actor, Prop::Velocity, [0.0; 3].into());
            self.set_vector(actor, Prop::Acceleration, [0.0; 3].into());
            return;
        }
        if !self.store.flag_of(actor, Prop::bJustTeleported) {
            self.set_vector(actor, Prop::Velocity, [0.0; 3].into());
        }
    }
}
