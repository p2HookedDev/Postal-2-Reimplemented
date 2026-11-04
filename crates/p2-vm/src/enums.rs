//! Reads script byte properties as enums; a byte that names no value reads as `None` and stays a byte.

use crate::{LoaderRef, ObjectId, Prop, Vm};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum EPhysics {
    None = 0,
    Walking = 1,
    Falling = 2,
    Swimming = 3,
    Flying = 4,
    Rotating = 5,
    Projectile = 6,
    Interpolating = 7,
    MovingBrush = 8,
    Spider = 9,
    Trailer = 10,
    Ladder = 11,
    RootMotion = 12,
    Karma = 13,
    KarmaRagDoll = 14,
}

impl EPhysics {
    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::None,
            1 => Self::Walking,
            2 => Self::Falling,
            3 => Self::Swimming,
            4 => Self::Flying,
            5 => Self::Rotating,
            6 => Self::Projectile,
            7 => Self::Interpolating,
            8 => Self::MovingBrush,
            9 => Self::Spider,
            10 => Self::Trailer,
            11 => Self::Ladder,
            12 => Self::RootMotion,
            13 => Self::Karma,
            14 => Self::KarmaRagDoll,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum EDrawType {
    Sprite = 1,
    Mesh = 2,
    StaticMesh = 8,
    Particle = 10,
}

impl EDrawType {
    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Sprite,
            2 => Self::Mesh,
            8 => Self::StaticMesh,
            10 => Self::Particle,
            _ => return None,
        })
    }
}

impl<L: LoaderRef> Vm<L> {
    #[inline]
    pub(crate) fn physics_of(&self, actor: ObjectId) -> Option<EPhysics> {
        self.store.physics_of(actor)
    }

    pub(crate) fn draw_type_of(&self, actor: ObjectId) -> Option<EDrawType> {
        EDrawType::from_byte(self.byte_of(actor, Prop::DrawType))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip_and_unknown_ones_read_as_none() {
        for byte in 0..=14 {
            assert_eq!(EPhysics::from_byte(byte).map(|physics| physics as u8), Some(byte));
        }
        assert_eq!(EPhysics::from_byte(15), None);
        for byte in [1, 2, 8, 10] {
            assert_eq!(EDrawType::from_byte(byte).map(|draw_type| draw_type as u8), Some(byte));
        }
        assert_eq!(EDrawType::from_byte(0), None);
    }
}
