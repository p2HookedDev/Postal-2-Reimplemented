//! A retail material's class, given at load from its names and layers, and the roughness and reflectance the modern
//! lit shader takes from it. The `MODERN_MATERIALS` level scales how far a surface leaves today's shading.

use crate::render::shader_layers::{ShaderLayers, SpecularCoords, SpecularParams};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MaterialClass {
    #[default]
    Default = 0,
    Metal = 1,
    TileOrGlass = 2,
    Wood = 3,
    Cloth = 4,
    Skin = 5,
    ConcreteOrPlaster = 6,
    Foliage = 7,
}

/// Name parts that mark each class, tried in this order: `carpet` holds `car`, so cloth comes before metal.
const NAME_PARTS: [(MaterialClass, &[&str]); 7] = [
    (MaterialClass::Cloth, &["cloth", "carpet", "fabric", "curtain", "rug", "towel", "mattress", "sofa", "couch"]),
    (MaterialClass::Foliage, &["foliage", "leaf", "leaves", "trees", "grass", "plant", "bush", "hedge", "flower", "ivy"]),
    (MaterialClass::Skin, &["skin", "flesh"]),
    (MaterialClass::TileOrGlass, &["glass", "window", "tile", "mirror", "porcelain", "ceramic", "marble", "chrome"]),
    (MaterialClass::Wood, &["wood", "plank", "crate", "board", "timber", "bark"]),
    (MaterialClass::Metal, &["metal", "steel", "iron", "rail", "pipe", "alum", "car", "rust", "grate"]),
    (MaterialClass::ConcreteOrPlaster, &["concrete", "plaster", "brick", "stone", "rock", "cement", "stucco", "asphalt", "road", "sidewalk", "wall"]),
];

impl MaterialClass {
    pub const ALL: [Self; 8] =
        [Self::Default, Self::Metal, Self::TileOrGlass, Self::Wood, Self::Cloth, Self::Skin, Self::ConcreteOrPlaster, Self::Foliage];

    /// `names` are the material's package, group and object names; `specular` its shader's specular layer, if any.
    pub fn of(names: &[&str], specular: Option<SpecularParams>) -> Self {
        let lower: Vec<String> = names.iter().map(|name| name.to_ascii_lowercase()).collect();
        let named = NAME_PARTS.iter().find(|(_, parts)| lower.iter().any(|name| parts.iter().any(|part| name.contains(part))));
        match (named, specular) {
            (Some(&(class, _)), _) => class,
            (None, Some(SpecularParams { coords: SpecularCoords::Cube, world: true, .. })) => Self::Metal,
            (None, Some(_)) => Self::TileOrGlass,
            (None, None) => Self::Default,
        }
    }

    /// The highlight's perceptual roughness. The default class keeps the shader's own.
    pub fn roughness(self) -> f32 {
        match self {
            Self::Default => 0.85,
            Self::Metal => 0.35,
            Self::TileOrGlass => 0.15,
            Self::Wood => 0.6,
            Self::Cloth => 0.95,
            Self::Skin => 0.55,
            Self::ConcreteOrPlaster => 0.9,
            Self::Foliage => 0.75,
        }
    }

    /// Reflectance at normal incidence; 1 is a metal, whose reflectance is its texture's colour.
    pub fn reflectance(self) -> f32 {
        match self {
            Self::Metal => 1.0,
            Self::TileOrGlass => 0.05,
            Self::Cloth => 0.03,
            Self::Skin => 0.028,
            Self::Foliage => 0.035,
            Self::Default | Self::Wood | Self::ConcreteOrPlaster => 0.04,
        }
    }

    pub fn from_index(index: f32) -> Self {
        Self::ALL.get(index.round().max(0.0) as usize).copied().unwrap_or_default()
    }
}

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

/// Above the three effect bits of the modern flag: the materials level in bits 3 to 6, the lamp size level in 7 to 10.
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
    use crate::render::shader_layers::SpecularMode;

    fn specular(coords: SpecularCoords, world: bool) -> SpecularParams {
        SpecularParams { mode: SpecularMode::Add, world, constant: 1.0, coords }
    }

    #[test]
    fn names_pick_the_class() {
        assert_eq!(MaterialClass::of(&["Furniture", "Carpets", "RedCarpet"], None), MaterialClass::Cloth);
        assert_eq!(MaterialClass::of(&["Metals", "Rails", "Railing01"], None), MaterialClass::Metal);
        assert_eq!(MaterialClass::of(&["Hospital", "Floors", "WhiteTile"], None), MaterialClass::TileOrGlass);
        assert_eq!(MaterialClass::of(&["Buildings", "Walls", "PlasterB"], None), MaterialClass::ConcreteOrPlaster);
        assert_eq!(MaterialClass::of(&["Nature", "Trees", "Oak"], None), MaterialClass::Foliage);
        assert_eq!(MaterialClass::of(&["Houses", "Floor", "OakPlanks"], None), MaterialClass::Wood);
        assert_eq!(MaterialClass::of(&["Characters", "Skins", "DudeHead"], None), MaterialClass::Skin);
        assert_eq!(MaterialClass::of(&["Town", "Streets", "Crosswalk"], None), MaterialClass::Default);
    }

    #[test]
    fn layers_pick_the_class_when_names_say_nothing() {
        assert_eq!(MaterialClass::of(&["P2Misc", "Misc", "Thing"], None), MaterialClass::Default);
        assert_eq!(MaterialClass::of(&["P2Misc", "Misc", "Thing"], Some(specular(SpecularCoords::Cube, true))), MaterialClass::Metal);
        assert_eq!(MaterialClass::of(&["P2Misc", "Misc", "Thing"], Some(specular(SpecularCoords::EnvMap, false))), MaterialClass::TileOrGlass);
        assert_eq!(MaterialClass::of(&["Woods", "Misc", "Thing"], Some(specular(SpecularCoords::Cube, true))), MaterialClass::Wood);
    }

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
