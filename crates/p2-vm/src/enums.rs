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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum ESoundOcclusion {
    #[default]
    Default = 0,
    None = 1,
    Bsp = 2,
    StaticMeshes = 3,
    All = 4,
}

impl ESoundOcclusion {
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::None,
            2 => Self::Bsp,
            3 => Self::StaticMeshes,
            4 => Self::All,
            _ => Self::Default,
        }
    }

    pub fn of<L: LoaderRef>(vm: &Vm<L>, actor: Option<ObjectId>) -> Self {
        actor.map_or(Self::Default, |actor| Self::from_byte(vm.store.byte_of(actor, Prop::SoundOcclusion)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FluidType {
    Gas = 1,
    Urine = 2,
    Blood = 3,
    Puke = 4,
    BloodyPuke = 5,
    Gonorrhea = 6,
    BloodyUrine = 7,
    Napalm = 8,
}

impl FluidType {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Gas,
            2 => Self::Urine,
            3 => Self::Blood,
            4 => Self::Puke,
            5 => Self::BloodyPuke,
            6 => Self::Gonorrhea,
            7 => Self::BloodyUrine,
            8 => Self::Napalm,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ERenderStyle {
    None = 0,
    Masked = 2,
    Translucent = 3,
    Modulated = 4,
}

impl ERenderStyle {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::None,
            2 => Self::Masked,
            3 => Self::Translucent,
            4 => Self::Modulated,
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
