//! A retail material's class, given at load from its names and layers, and the roughness and reflectance the modern
//! lit shader takes from it. `Settings::material_level` scales how far a surface leaves today's shading.

use crate::render::shader_layers::ShaderLayers;

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

/// Words that mark each class, or with a plural `s`, tried in this order. Only words that name bare metal are listed:
/// `car`, `iron` and `rust` also name painted things.
const NAME_PARTS: [(MaterialClass, &[&str]); 7] = [
    (MaterialClass::Cloth, &["cloth", "carpet", "fabric", "curtain", "rug", "towel", "mattress", "sofa", "couch"]),
    (MaterialClass::Foliage, &["foliage", "leaf", "leaves", "trees", "grass", "plant", "bush", "hedge", "flower", "ivy"]),
    (MaterialClass::Skin, &["skin", "flesh"]),
    (MaterialClass::TileOrGlass, &["glass", "window", "tile", "mirror", "porcelain", "ceramic", "marble", "chrome"]),
    (MaterialClass::Wood, &["wood", "plank", "crate", "board", "timber", "bark"]),
    (MaterialClass::Metal, &["metal", "steel", "rail", "pipe", "alum", "grate"]),
    (MaterialClass::ConcreteOrPlaster, &["concrete", "plaster", "brick", "stone", "rock", "cement", "stucco", "asphalt", "road", "sidewalk", "wall"]),
];

impl MaterialClass {
    pub const ALL: [Self; 8] =
        [Self::Default, Self::Metal, Self::TileOrGlass, Self::Wood, Self::Cloth, Self::Skin, Self::ConcreteOrPlaster, Self::Foliage];

    /// `names` are the material's package, its own name, then its groups from the innermost. The first of the own name,
    /// the groups and last the package that holds a class's word decides; a guess that is not sure stays the default.
    pub fn of(names: &[&str]) -> Self {
        names.iter().skip(1).chain(names.first()).find_map(|name| named_class(name)).unwrap_or_default()
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

/// The first class with a word among the name's whole words, which split at every non-letter and where a lower case
/// letter meets an upper case one: `Trailer` is not a `rail`.
fn named_class(name: &str) -> Option<MaterialClass> {
    let mut spaced = String::with_capacity(name.len() * 2);
    let mut after_lower = false;
    for c in name.chars() {
        if after_lower && c.is_ascii_uppercase() {
            spaced.push(' ');
        }
        spaced.push(if c.is_ascii_alphabetic() { c.to_ascii_lowercase() } else { ' ' });
        after_lower = c.is_ascii_lowercase();
    }
    let is_word = |part: &str| spaced.split_whitespace().any(|word| word == part || word.strip_suffix('s') == Some(part));
    NAME_PARTS.iter().find(|(_, parts)| parts.iter().any(|&part| is_word(part))).map(|&(class, _)| class)
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

    #[test]
    fn names_pick_the_class() {
        assert_eq!(MaterialClass::of(&["Furniture", "RedCarpet", "Carpets"]), MaterialClass::Cloth);
        assert_eq!(MaterialClass::of(&["Metals", "Railing01", "Rails"]), MaterialClass::Metal);
        assert_eq!(MaterialClass::of(&["Hospital", "WhiteTile", "Floors"]), MaterialClass::TileOrGlass);
        assert_eq!(MaterialClass::of(&["Buildings", "PlasterB", "Walls"]), MaterialClass::ConcreteOrPlaster);
        assert_eq!(MaterialClass::of(&["Nature", "Oak", "Trees"]), MaterialClass::Foliage);
        assert_eq!(MaterialClass::of(&["Houses", "OakPlanks", "Floor"]), MaterialClass::Wood);
        assert_eq!(MaterialClass::of(&["Characters", "DudeHead", "Skins"]), MaterialClass::Skin);
        assert_eq!(MaterialClass::of(&["Town", "Crosswalk", "Streets"]), MaterialClass::Default);
        assert_eq!(MaterialClass::of(&["Woods", "Thing", "Misc"]), MaterialClass::Wood);
        assert_eq!(MaterialClass::of(&[]), MaterialClass::Default);
    }

    #[test]
    fn only_whole_words_count_and_the_own_name_comes_first() {
        for unsure in [["Trailers", "TrailerSide", "Ext"], ["Signs", "Billboard01", "Ads"], ["Props", "Rocket", "Drugs"], ["Misc", "Textile", "Glasses"]] {
            assert_eq!(MaterialClass::of(&unsure), MaterialClass::Default, "{unsure:?}");
        }
        assert_eq!(MaterialClass::of(&["Tiles", "BrickWall", "Walls"]), MaterialClass::ConcreteOrPlaster);
        assert_eq!(MaterialClass::of(&["Tiles", "Thing", "Misc"]), MaterialClass::TileOrGlass);
        assert_eq!(MaterialClass::of(&["Town", "GuardRail", "Fences"]), MaterialClass::Metal);
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
