//! Each slot of `DrawActor` calls draws on its own overlay camera, as retail clears the depth before each first-person
//! call; the HUD's camera draws the last slot while it may.

use bevy::camera::visibility::RenderLayers;

use crate::components::OVERLAY_LAYERS;

const SLOTS: usize = OVERLAY_LAYERS.len();

/// The `OverlayCamera` index of the HUD's camera, past every slot's.
pub(crate) const HUD_SLOT: usize = SLOTS;

pub(crate) fn slot_of_layers(layers: &RenderLayers) -> Option<usize> {
    OVERLAY_LAYERS.iter().position(|&layer| layers.intersects(&RenderLayers::layer(layer)))
}

/// The camera that draws each slot: its own, but the HUD's for the last, which draws after every canvas layer and before
/// the 2D items painted after the last call.
pub(crate) fn overlay_routes(shown: usize, hud_carries: bool) -> [usize; SLOTS] {
    let mut cameras = std::array::from_fn(|slot| slot);
    if hud_carries && let Some(last) = shown.min(SLOTS).checked_sub(1) {
        cameras[last] = HUD_SLOT;
    }
    cameras
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hud_camera_carries_only_the_last_slot() {
        assert_eq!(overlay_routes(1, true), [HUD_SLOT, 1, 2, 3]);
        assert_eq!(overlay_routes(2, true), [0, HUD_SLOT, 2, 3]);
        assert_eq!(overlay_routes(9, true), [0, 1, 2, HUD_SLOT]);
        assert_eq!(overlay_routes(2, false), [0, 1, 2, 3]);
        assert_eq!(overlay_routes(0, true), [0, 1, 2, 3]);
    }
}
