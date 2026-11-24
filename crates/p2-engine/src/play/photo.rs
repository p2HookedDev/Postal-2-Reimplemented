//! Photo mode's game side: what the script's photo mode settings mean to the VM, and the screenshot request handed to
//! the drawing side.

use bevy::ecs::prelude::{MessageWriter, ResMut};
use p2_vm::{LoaderRef, ObjectId, Prop, Vm, ini_truthy};

use super::{Play, PlayWorld};
use p2_host::display::{MODERN_PHOTO_ACTIVE, MODERN_PHOTO_HIDE_PLAYER};
use p2_render::photo::ScreenshotRequest;

pub(crate) fn photo_active<L: LoaderRef>(vm: &mut Vm<L>) -> bool {
    vm.get_command(MODERN_PHOTO_ACTIVE).is_ok_and(|value| ini_truthy(&value))
}

/// The key is saved with the settings, so a crash while photo mode was open would leave it on.
pub(crate) fn clear_photo_active<L: LoaderRef>(vm: &mut Vm<L>) {
    if photo_active(vm) {
        vm.set_command(&format!("{MODERN_PHOTO_ACTIVE} 0"));
    }
}

/// The player's pawn while photo mode is open with its hide row set.
pub(crate) fn photo_hidden_pawn<L: LoaderRef>(vm: &mut Vm<L>) -> Option<ObjectId> {
    if !photo_active(vm) || !vm.get_command(MODERN_PHOTO_HIDE_PLAYER).is_ok_and(|value| ini_truthy(&value)) {
        return None;
    }
    vm.store.object_of(vm.player()?, Prop::Pawn)
}

pub(super) fn request_screenshot(mut play: ResMut<Play>, mut shots: MessageWriter<ScreenshotRequest>) {
    if play.world_mut().is_some_and(PlayWorld::take_screenshot) {
        shots.write(ScreenshotRequest);
    }
}
