//! Reads script byte properties as enums; a byte that names no value reads as `None` and stays a byte.

pub(crate) use p2_assets::EDrawType;

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
pub enum ERenderStyle {
    None = 0,
    Translucent = 3,
}

impl ERenderStyle {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::None,
            3 => Self::Translucent,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
#[repr(u8)]
pub(crate) enum ENetRole {
    SimulatedProxy = 2,
    AutonomousProxy = 3,
    Authority = 4,
}

impl ENetRole {
    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            2 => Self::SimulatedProxy,
            3 => Self::AutonomousProxy,
            4 => Self::Authority,
            _ => return None,
        })
    }
}

impl<L: LoaderRef> Vm<L> {

    pub fn draw_type_of(&self, actor: ObjectId) -> Option<EDrawType> {
        EDrawType::from_byte(self.store.byte_of(actor, Prop::DrawType))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum EProbe {
    AnimEnd = 24,
    Tick = 36,
    SeePlayer = 40,
    EnemyNotVisible = 41,
    HearNoise = 42,
    UpdateEyeHeight = 43,
    SeeMonster = 44,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ESAStatus {
    Running = 1,
    Ending = 2,
    Expired = 3,
}

impl ESAStatus {
    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Running,
            2 => Self::Ending,
            3 => Self::Expired,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ECamOrientation {
    LookAtActor = 1,
    FacePath = 2,
    Interpolate = 3,
}

impl ECamOrientation {
    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::LookAtActor,
            2 => Self::FacePath,
            3 => Self::Interpolate,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum EInputAction {
    Press = 1,
    Release = 3,
    Axis = 4,
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
        for byte in [1, 2, 8, 10, 11] {
            assert_eq!(EDrawType::from_byte(byte).map(|draw_type| draw_type as u8), Some(byte));
        }
        assert_eq!(EDrawType::from_byte(0), None);
    }
}
