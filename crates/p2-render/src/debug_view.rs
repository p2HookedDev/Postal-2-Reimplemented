//! One part of the modern surface light drawn on its own, chosen in the F10 overlay: it rides in the materials'
//! modern flag, so a change rewrites material data and compiles no pipeline.

use bevy::ecs::prelude::Resource;

/// Above `POOL_MIRROR`. `surface.wesl` holds the same number.
pub const DEBUG_VIEW_SHIFT: u16 = 12;

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum DebugView {
    #[default]
    Off = 0,
    Lighting = 1,
    Sun = 2,
    SkySeen = 3,
    Lamps = 4,
    UnshadowedLamps = 5,
    Bounce = 6,
    Occlusion = 7,
    Normals = 8,
    Albedo = 9,
}

impl DebugView {
    pub const ALL: [Self; 10] =
        [Self::Off, Self::Lighting, Self::Sun, Self::SkySeen, Self::Lamps, Self::UnshadowedLamps, Self::Bounce, Self::Occlusion, Self::Normals, Self::Albedo];

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lighting => "lighting only",
            Self::Sun => "sun",
            Self::SkySeen => "sky seen",
            Self::Lamps => "lamps",
            Self::UnshadowedLamps => "unshadowed lamps",
            Self::Bounce => "bounce and probes",
            Self::Occlusion => "occlusion",
            Self::Normals => "normals",
            Self::Albedo => "albedo",
        }
    }

    pub fn flag_bits(self) -> u16 {
        u16::from(self as u8) << DEBUG_VIEW_SHIFT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_fits_its_four_bits_above_the_mirror_bit() {
        for view in DebugView::ALL {
            assert_eq!(view.flag_bits() >> DEBUG_VIEW_SHIFT, u16::from(view as u8));
            assert_eq!(view.flag_bits() & crate::modern::POOL_MIRROR, 0);
        }
        assert!((DebugView::ALL.len() as u16) <= 16);
    }

    #[test]
    fn the_shader_reads_the_view_where_it_is_packed() {
        assert_eq!(crate::shaders::shader_const(include_str!("surface.wesl"), "DEBUG_VIEW_SHIFT"), f64::from(DEBUG_VIEW_SHIFT));
    }
}
