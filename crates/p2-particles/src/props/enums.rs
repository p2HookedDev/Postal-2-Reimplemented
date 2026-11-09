#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrawStyle {
    Regular,
    AlphaBlend,
    Modulated,
    Translucent,
    AlphaModulate,
    Darken,
    Brighten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinateSystem {
    Independent,
    Relative,
    Absolute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartLocationShape {
    Box,
    Sphere,
    Polar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationSource {
    None,
    Actor,
    Offset,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocityDirection {
    None,
    StartPositionAndOwner,
    OwnerAndStartPosition,
    AddRadial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectionUsage {
    None,
    Up,
    Right,
    Forward,
    Normal,
    UpAndNormal,
    RightAndNormal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmitterKind {
    #[default]
    Sprite,
    SuperSprite,
    Strip,
    Mesh,
    Beam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeExtend {
    None,
    Line,
    Circle,
    Other,
}
