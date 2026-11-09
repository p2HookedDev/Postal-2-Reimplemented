use crate::Rng;
use crate::math::Vector;

macro_rules! read {
    ($lookup:ident, $name:literal, $field:expr, $convert:expr) => {
        if let Some(value) = $lookup($name).as_ref().and_then($convert) {
            $field = value;
        }
    };
}

mod beam;
pub use beam::{BeamEnd, BeamEndPoint, BeamScale, BeamProperties};
mod emitter;
pub use emitter::{ColorKey, SizeKey, EmitterProperties};
mod enums;
pub use enums::{
    DrawStyle, CoordinateSystem, StartLocationShape, RotationSource, VelocityDirection, DirectionUsage, EmitterKind,
    ShapeExtend,
};
mod lookup;
#[cfg(test)]
mod tests;

/// A property value as the caller's object model holds it.
///
/// Structs are positional, members in declaration order: `Vector` (X, Y, Z), `Plane` (X, Y, Z, W), `Rotator`
/// (Pitch, Yaw, Roll), `Range` (Min, Max), `RangeVector` (X, Y, Z ranges), `Color` (B, G, R, A),
/// `ParticleTimeScale` (`RelativeTime`, `RelativeSize`), `ParticleColorScale` (`RelativeTime`, `Color`).
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Bool(bool),
    Int(i32),
    Float(f32),
    Struct(Vec<Self>),
    Array(Vec<Self>),
}

impl PropertyValue {
    fn float(&self) -> Option<f32> {
        match *self {
            Self::Float(v) => Some(v),
            Self::Int(v) => Some(v as f32),
            _ => None,
        }
    }

    fn int(&self) -> Option<i32> {
        match *self {
            Self::Int(v) => Some(v),
            Self::Float(v) => Some(v as i32),
            Self::Bool(v) => Some(i32::from(v)),
            _ => None,
        }
    }

    fn bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(v) => Some(v),
            Self::Int(v) => Some(v != 0),
            _ => None,
        }
    }

    fn members(&self) -> &[Self] {
        match self {
            Self::Struct(members) | Self::Array(members) => members,
            _ => &[],
        }
    }

    fn floats<const N: usize>(&self) -> Option<[f32; N]> {
        let members = self.members();
        (members.len() >= N).then(|| std::array::from_fn(|i| members[i].float().unwrap_or(0.0)))
    }

    fn range(&self) -> Option<Range> {
        self.floats::<2>().map(|[min, max]| Range { min, max })
    }

    fn range_vector(&self) -> Option<RangeVector> {
        let members = self.members();
        let [x, y, z] = [0, 1, 2].map(|i| members.get(i).and_then(Self::range));
        Some(RangeVector { x: x?, y: y?, z: z? })
    }

    fn color(&self) -> Option<[u8; 4]> {
        let members = self.members();
        let byte = |i: usize| members.get(i).and_then(Self::int).map(|v| v as u8);
        Some([byte(2)?, byte(1)?, byte(0)?, byte(3)?])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Range {
    pub min: f32,
    pub max: f32,
}

impl Range {
    pub const fn new(min: f32, max: f32) -> Self {
        Self { min, max }
    }

    pub fn rand(&self, rng: &mut Rng) -> f32 {
        rng.frand() * (self.min - self.max) + self.max
    }

    pub fn center(&self) -> f32 {
        (self.min + self.max) * 0.5
    }
}

/// A draw takes Z, then Y, then X.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RangeVector {
    pub x: Range,
    pub y: Range,
    pub z: Range,
}

impl RangeVector {
    pub const fn splat(min: f32, max: f32) -> Self {
        let r = Range::new(min, max);
        Self { x: r, y: r, z: r }
    }

    pub fn rand(&self, rng: &mut Rng) -> Vector {
        let z = self.z.rand(rng);
        let y = self.y.rand(rng);
        let x = self.x.rand(rng);
        [x, y, z].into()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EmitterSettings {
    pub auto_destroy: bool,
    pub auto_reset: bool,
    pub global_offset_range: RangeVector,
    pub time_till_reset_range: Range,
    pub disable_fogging: bool,
}

impl EmitterSettings {
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<PropertyValue>) -> Self {
        let mut s = Self::default();
        read!(lookup, "AutoDestroy", s.auto_destroy, PropertyValue::bool);
        read!(lookup, "AutoReset", s.auto_reset, PropertyValue::bool);
        read!(lookup, "GlobalOffsetRange", s.global_offset_range, PropertyValue::range_vector);
        read!(lookup, "TimeTillResetRange", s.time_till_reset_range, PropertyValue::range);
        read!(lookup, "DisableFogging", s.disable_fogging, PropertyValue::bool);
        s
    }
}
