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

impl DrawStyle {
    pub(crate) const BY_INDEX: [Self; 7] =
        [Self::Regular, Self::AlphaBlend, Self::Modulated, Self::Translucent, Self::AlphaModulate, Self::Darken, Self::Brighten];
}

impl CoordinateSystem {
    pub(crate) const BY_INDEX: [Self; 3] = [Self::Independent, Self::Relative, Self::Absolute];
}

impl StartLocationShape {
    pub(crate) const BY_INDEX: [Self; 3] = [Self::Box, Self::Sphere, Self::Polar];
}

impl RotationSource {
    pub(crate) const BY_INDEX: [Self; 4] = [Self::None, Self::Actor, Self::Offset, Self::Normal];
}

impl VelocityDirection {
    pub(crate) const BY_INDEX: [Self; 4] = [Self::None, Self::StartPositionAndOwner, Self::OwnerAndStartPosition, Self::AddRadial];
}

impl DirectionUsage {
    pub(crate) const BY_INDEX: [Self; 7] =
        [Self::None, Self::Up, Self::Right, Self::Forward, Self::Normal, Self::UpAndNormal, Self::RightAndNormal];
}

impl ShapeExtend {
    pub(crate) const BY_INDEX: [Self; 4] = [Self::None, Self::Line, Self::Circle, Self::Other];
}
