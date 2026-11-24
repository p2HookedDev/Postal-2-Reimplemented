//! A retail material's class, given at load from its names and layers, and the roughness and reflectance the modern
//! lit shader takes from it. `Settings::material_level` scales how far a surface leaves today's shading.

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
}
