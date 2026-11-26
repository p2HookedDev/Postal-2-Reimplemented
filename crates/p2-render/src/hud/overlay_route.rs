//! Retail clears the depth before each first-person `DrawActor` call, so each slot gets its own overlay camera.

use bevy::camera::visibility::RenderLayers;

use crate::world::components::OVERLAY_LAYERS;

const SLOTS: usize = OVERLAY_LAYERS.len();

pub(crate) const HUD_SLOT: usize = SLOTS;

pub(crate) fn slot_of_layers(layers: &RenderLayers) -> Option<usize> {
    OVERLAY_LAYERS.iter().position(|&layer| layers.intersects(&RenderLayers::layer(layer)))
}

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
