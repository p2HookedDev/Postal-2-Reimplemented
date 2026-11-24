use p2_bevy_assets::material_class::MaterialClass;

use crate::shader_layers::ShaderLayers;

/// The class goes in the layers' `blend`: y its index, z its roughness, w its reflectance. A zero `blend.z` is a
/// material made before classes, which the shader shades as the default class.
pub fn with_class(mut layers: ShaderLayers, class: MaterialClass) -> ShaderLayers {
    layers.blend.y = f32::from(class as u8);
    layers.blend.z = class.roughness();
    layers.blend.w = class.reflectance();
    layers
}

pub fn class_of(layers: &ShaderLayers) -> MaterialClass {
    MaterialClass::from_index(layers.blend.y)
}

/// Above the three effect bits of the modern flag: the materials level in bits 3 to 6, the lamp size level in 7
/// to 10.
pub const LEVEL_SHIFT_MATERIALS: u16 = 3;
pub const LEVEL_SHIFT_LAMP_SIZE: u16 = 7;

pub fn level_bits(materials: u8, lamp_size: u8) -> u16 {
    (u16::from(materials.min(15)) << LEVEL_SHIFT_MATERIALS) | (u16::from(lamp_size.min(15)) << LEVEL_SHIFT_LAMP_SIZE)
}

pub fn levels_of(flag: u16) -> (u8, u8) {
    (((flag >> LEVEL_SHIFT_MATERIALS) & 15) as u8, ((flag >> LEVEL_SHIFT_LAMP_SIZE) & 15) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_class_round_trips_through_the_layers() {
        for class in MaterialClass::ALL {
            let layers = with_class(ShaderLayers::default(), class);
            assert_eq!(class_of(&layers), class);
            assert_eq!((layers.blend.z, layers.blend.w), (class.roughness(), class.reflectance()));
            assert!(layers.blend.z > 0.0);
        }
        assert_eq!(class_of(&ShaderLayers::default()), MaterialClass::Default);
    }

    #[test]
    fn levels_sit_above_the_effect_bits() {
        assert_eq!(level_bits(0, 0), 0);
        assert_eq!(level_bits(10, 10) & 7, 0);
        assert_eq!(levels_of(level_bits(5, 10) | 7), (5, 10));
        assert!(f32::from(level_bits(15, 15) | 7) < 16_777_216.0);
    }
}
