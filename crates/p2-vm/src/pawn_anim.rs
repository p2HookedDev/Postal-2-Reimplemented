use p2_math::{Rotator, Vector};

use crate::enums::EPhysics;
use crate::interp::int;
use crate::anim::Playback;
use crate::{Encroachment, Error, LoaderRef, NameId, ObjectId, Value, Vm, Prop, Kind};

/// Channels: turning right / left in place, then `MovementAnims` forward, back, left, right; the four after those
/// hold the previous movement set while it blends out.
const RIGHT_TURN_CHANNEL: i32 = 2;
const LEFT_TURN_CHANNEL: i32 = 3;
const FIRST_MOVEMENT_CHANNEL: i32 = 4;
const END_CHANNEL: i32 = FIRST_MOVEMENT_CHANNEL + 8;
const NM_DEDICATED_SERVER: u8 = 1;
const ROLE_SIMULATED_PROXY: u8 = 2;
const ROLE_AUTHORITY: u8 = 4;
const PROBE_UPDATE_EYE_HEIGHT: u32 = 43;
const MIN_TURN_RATE: f32 = 1000.0;
const TURN_ANIM_YAW_RATE: f32 = 16384.0;
const MIN_BIASED_STRAFE: f32 = 0.1;
const TWIST_DRAWN_SECONDS: f32 = 1.0;
const TWIST_STILL_SPEED_SQUARED: f32 = 1000.0;
const TWIST_FOOT_RATE: f32 = 16384.0;
const TWIST_START: i32 = 0x2aac;
const TWIST_MAX: i32 = 12000;
const TWIST_FOLLOW: i32 = 0x800;

#[derive(Debug, Clone, Copy, PartialEq)]
struct FootTwist {
    rot: i32,
    turn_dir: i32,
    turning: bool,
    still: bool,
}

impl FootTwist {
    fn step(mut self, yaw: i32, standing: bool, delta: f32) -> Self {
        let mut twist = yaw.wrapping_sub(self.rot) & 0xffff;
        if twist > 0x8000 {
            twist -= 0x10000;
        }
        if !standing {
            if self.still {
                self.still = false;
                self.turning = true;
            }
        } else if !self.still {
            self.still = true;
            self.rot = yaw;
            twist = 0;
        }
        if !self.turning {
            if self.still && twist.abs() >= TWIST_START {
                self.turn_dir = twist.signum();
                self.turning = true;
            }
        } else if twist > TWIST_MAX {
            self.rot = yaw.wrapping_sub(TWIST_MAX) & 0xffff;
        } else if twist > TWIST_FOLLOW {
            self.rot = (self.rot as f32 + delta * TWIST_FOOT_RATE) as i32 & 0xffff;
        } else if twist < -TWIST_MAX {
            self.rot = yaw.wrapping_add(TWIST_MAX) & 0xffff;
        } else if twist < -TWIST_FOLLOW {
            self.rot = (self.rot as f32 - delta * TWIST_FOOT_RATE) as i32 & 0xffff;
        } else {
            self.turning = false;
            self.rot &= 0xffff;
        }
        self
    }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn pawn_tick_special(&mut self, pawn: ObjectId, delta: f32) -> Result<(), Error> {
        if !self.store.flag_of(pawn, Prop::bInterpolating) && self.store.flag_of(pawn, Prop::bPhysicsAnimUpdate) && self.store.object_of(pawn, Prop::Mesh).is_some() {
            self.update_movement_animation(pawn, delta)?;
        }
        if let Some(weapon) = self.store.object_of(pawn, Prop::Weapon).filter(|&w| !self.store.is_deleted(w)) {
            let controller = self.store.object_of(pawn, Prop::Controller);
            let first_person = controller.is_some_and(|c| {
                Some(c) == self.player()
                    && self.store.is_a(c, Kind::PlayerController)
                    && self.store.object_of(c, Prop::ViewTarget).unwrap_or(pawn) == pawn
                    && !self.store.flag_of(c, Prop::bBehindView)
            });
            if !first_person {
                let location = self.store.vector(pawn, Prop::Location);
                self.place_far_moved(weapon, location, Encroachment::Ignored)?;
            }
        }
        if self.store.flag_of(pawn, Prop::bUpdateEyeheight) && self.is_probing(pawn, PROBE_UPDATE_EYE_HEIGHT)? {
            self.event(pawn, "UpdateEyeHeight", [Value::Float(delta)])?;
        }
        let breath = self.store.float_of(pawn, Prop::BreathTime);
        if self.store.byte_of(pawn, Prop::Role) == ROLE_AUTHORITY && breath > 0.0 {
            let left = breath - delta;
            if left < 0.001 {
                self.store.set(pawn, Prop::BreathTime, Value::Float(0.0));
                self.event(pawn, "BreathTimer", Vec::new())?;
            } else {
                self.store.set(pawn, Prop::BreathTime, Value::Float(left));
            }
        }
        Ok(())
    }

    fn update_movement_animation(&mut self, pawn: ObjectId, delta: f32) -> Result<(), Error> {
        if self.level.is_some_and(|level| self.store.byte_of(level, Prop::NetMode) == NM_DEDICATED_SERVER) || !self.ensure_instance(pawn) {
            return Ok(());
        }
        if !self.store.flag_of(pawn, Prop::bInitializeAnimation) {
            self.store.set(pawn, Prop::bInitializeAnimation, Value::Bool(true));
            let (right, left) = (self.name_of(pawn, Prop::TurnRightAnim), self.name_of(pawn, Prop::TurnLeftAnim));
            self.play(pawn, RIGHT_TURN_CHANNEL, right, 1.0, 0.1, Playback::Looping);
            self.play(pawn, LEFT_TURN_CHANNEL, left, 1.0, 0.1, Playback::Looping);
            for i in 0..4 {
                let sequence = self.movement_anim(pawn, i);
                self.play(pawn, FIRST_MOVEMENT_CHANNEL + i as i32, sequence, 1.0, 0.1, Playback::Looping);
            }
            for channel in RIGHT_TURN_CHANNEL..END_CHANNEL {
                if let Some(state) = self.valid_channel(pawn, channel) {
                    state.notify_disabled = true;
                    state.blend_alpha = 0.0;
                }
            }
            if let Some(state) = self.valid_channel(pawn, FIRST_MOVEMENT_CHANNEL) {
                state.notify_disabled = false;
            }
        }
        let step = delta / self.store.float_of(pawn, Prop::BlendChangeTime);
        if self.store.flag_of(pawn, Prop::bTearOff) {
            if !self.store.flag_of(pawn, Prop::bPlayedDeath) {
                let args = ["HitDamageType", "TakeHitLocation"].map(|p| self.store.get(pawn, p).unwrap_or(Value::Object(None)));
                self.event(pawn, "PlayDying", args.to_vec())?;
            }
            self.zero_movement_alpha(pawn, RIGHT_TURN_CHANNEL, END_CHANNEL, step);
            return Ok(());
        }
        let (crouched, walking) = (self.store.flag_of(pawn, Prop::bIsCrouched), self.store.flag_of(pawn, Prop::bIsWalking));
        if crouched != self.store.flag_of(pawn, Prop::bWasCrouched) || walking != self.store.flag_of(pawn, Prop::bWasWalking) {
            self.event(pawn, "ChangeAnimation", Vec::new())?;
            self.store.set(pawn, Prop::bWasCrouched, Value::Bool(crouched));
            self.store.set(pawn, Prop::bWasWalking, Value::Bool(walking));
        }
        let old_physics = self.store.byte_of(pawn, Prop::OldPhysics);
        if (old_physics == EPhysics::Falling as u8) != (self.store.physics_of(pawn) == Some(EPhysics::Falling)) {
            let up = self.store.vector(pawn, Prop::Velocity)[2];
            if self.store.physics_of(pawn) == Some(EPhysics::Walking) {
                self.event(pawn, "PlayLandingAnimation", [Value::Float(up)])?;
            } else if self.store.flag_of(pawn, Prop::bWasOnGround) && up > 0.0 {
                self.event(pawn, "PlayJump", Vec::new())?;
            } else {
                self.event(pawn, "PlayFalling", Vec::new())?;
            }
        }
        let physics = self.store.byte_of(pawn, Prop::Physics);
        if physics != old_physics && physics != EPhysics::Falling as u8 {
            self.event(pawn, "ChangeAnimation", Vec::new())?;
        }
        let physics = self.store.byte_of(pawn, Prop::Physics);
        self.store.set(pawn, Prop::OldPhysics, Value::Byte(physics));
        let physics = EPhysics::from_byte(physics);
        self.store.set(pawn, Prop::bWasOnGround, Value::Bool(physics == Some(EPhysics::Walking)));

        let now = self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds));
        let acceleration = self.store.vector(pawn, Prop::Acceleration);
        if physics == Some(EPhysics::Falling) || now < self.store.float_of(pawn, Prop::MovementBlendStartTime) {
            self.zero_movement_alpha(pawn, RIGHT_TURN_CHANNEL, END_CHANNEL, step);
        } else if acceleration != Vector::ZERO {
            self.zero_movement_alpha(pawn, RIGHT_TURN_CHANNEL, FIRST_MOVEMENT_CHANNEL, step);
            self.blend_movement(pawn, physics, acceleration, step);
        } else {
            if self.store.vector(pawn, Prop::OldAcceleration) != Vector::ZERO && self.store.physics_of(pawn) != Some(EPhysics::Falling) {
                self.event(pawn, "ChangeAnimation", Vec::new())?;
            }
            self.zero_movement_alpha(pawn, FIRST_MOVEMENT_CHANNEL, END_CHANNEL, step);
            self.turn_in_place(pawn, delta, step);
        }
        if self.store.flag_of(pawn, Prop::bDoTorsoTwist) {
            self.update_twist_look(pawn, delta);
        }
        Ok(())
    }

    /// `ViewPitch` is a signed 16-bit rotation; the half turn stays positive.
    fn update_twist_look(&mut self, pawn: ObjectId, delta: f32) {
        let yaw = self.store.rotator_of(pawn, Prop::Rotation).yaw;
        let now = self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds));
        if now - self.store.float_of(pawn, Prop::LastRenderTime) > TWIST_DRAWN_SECONDS {
            self.store.set(pawn, Prop::FootTurning, Value::Bool(false));
            self.store.set(pawn, Prop::FootStill, Value::Bool(false));
            self.store.set(pawn, Prop::FootRot, Value::Int(yaw));
            return;
        }
        let int_of = |vm: &Self, property: Prop| vm.store.get(pawn, property).as_ref().map_or(0, int);
        let feet = FootTwist {
            rot: int_of(self, Prop::FootRot),
            turn_dir: int_of(self, Prop::TurnDir),
            turning: self.store.flag_of(pawn, Prop::FootTurning),
            still: self.store.flag_of(pawn, Prop::FootStill),
        };
        let velocity = self.store.vector(pawn, Prop::Velocity);
        let standing = velocity[0] * velocity[0] + velocity[1] * velocity[1] < TWIST_STILL_SPEED_SQUARED && self.store.physics_of(pawn) == Some(EPhysics::Walking);
        let feet = feet.step(yaw, standing, delta);
        self.store.set(pawn, Prop::FootRot, Value::Int(feet.rot));
        self.store.set(pawn, Prop::TurnDir, Value::Int(feet.turn_dir));
        self.store.set(pawn, Prop::FootTurning, Value::Bool(feet.turning));
        self.store.set(pawn, Prop::FootStill, Value::Bool(feet.still));
        let look = i32::from(self.store.byte_of(pawn, Prop::ViewPitch)) * 256;
        self.set_twist_look(pawn, if look > 0x8000 { look - 0x10000 } else { look });
    }

    fn blend_movement(&mut self, pawn: ObjectId, physics: Option<EPhysics>, acceleration: Vector, step: f32) {
        let mut look = self.store.rotator_of(pawn, Prop::Rotation).vector();
        if physics == Some(EPhysics::Walking) {
            look[2] = 0.0;
        } else if physics == Some(EPhysics::Ladder)
            && let Some(ladder) = self.store.object_of(pawn, Prop::OnLadder)
        {
            look = self.store.vector(ladder, Prop::ClimbDir);
        }
        let look = look.safe_normal();
        let direction = acceleration.safe_normal();
        let left = Vector::new(look[1], -look[0], 0.0).safe_normal();
        let mut forward = look.dot(direction);
        let sideways = left.dot(direction);
        let mut forward_weight = forward * forward;
        let mut side_weight = sideways * sideways;
        if physics == Some(EPhysics::Ladder) {
            if acceleration[2] > 0.0 {
                forward = 1.0;
            } else if acceleration[2] < 0.0 {
                forward = -1.0;
            }
            forward_weight = forward * forward;
        }
        if forward_weight > 0.0 {
            forward_weight = 1.0;
            if side_weight > MIN_BIASED_STRAFE {
                let bias = self.store.float_of(pawn, if forward > 0.0 { Prop::ForwardStrafeBias } else { Prop::BackwardStrafeBias });
                side_weight += bias * (1.0 - side_weight);
            }
        }
        let mut frame_rate = 0.0;
        let mut count = 0;
        if forward != 0.0 {
            count += 1;
            frame_rate = self.anim_rate_on_channel(pawn, FIRST_MOVEMENT_CHANNEL + if forward > 0.0 { 0 } else { 1 });
        }
        if sideways != 0.0 {
            count += 1;
            frame_rate += self.anim_rate_on_channel(pawn, FIRST_MOVEMENT_CHANNEL + if sideways > 0.0 { 2 } else { 3 });
        }
        if count == 2 {
            frame_rate *= 0.5;
        }
        let frame_rate = frame_rate * self.max_speed(pawn) / self.store.float_of(pawn, Prop::BaseMovementRate);
        for i in 0..4 {
            let channel = FIRST_MOVEMENT_CHANNEL + i as i32;
            let wanted = self.movement_anim(pawn, i);
            if self.channel(pawn, channel).map_or(NameId::NONE, |state| state.sequence) != wanted {
                if self.blend_alpha(pawn, channel + 4) < self.blend_alpha(pawn, channel) {
                    self.copy_animation(pawn, channel, channel + 4);
                }
                self.set_anim_sequence(pawn, channel, wanted);
            }
        }
        let (forward_target, back_target) = if forward < 0.0 { (0.0, forward_weight) } else { (forward_weight, 0.0) };
        let (left_target, right_target) = if sideways < 0.0 { (0.0, side_weight) } else { (side_weight, 0.0) };
        for (offset, target) in [forward_target, back_target, left_target, right_target, 0.0, 0.0, 0.0, 0.0].into_iter().enumerate() {
            self.update_blend_alpha(pawn, FIRST_MOVEMENT_CHANNEL + offset as i32, target, step);
        }
        for channel in FIRST_MOVEMENT_CHANNEL..END_CHANNEL {
            let rate = frame_rate / self.anim_rate_on_channel(pawn, channel);
            self.set_anim_rate(pawn, channel, rate);
        }
        let frame = self.channel(pawn, FIRST_MOVEMENT_CHANNEL).map_or(0.0, |state| state.frame);
        for channel in FIRST_MOVEMENT_CHANNEL + 1..END_CHANNEL {
            if let Some(state) = self.channel_mut(pawn, channel) {
                state.frame = frame;
            }
        }
    }

    fn turn_in_place(&mut self, pawn: ObjectId, delta: f32, step: f32) {
        let Rotator { pitch, yaw, roll } = self.store.rotator_of(pawn, Prop::Rotation);
        if yaw & 0xffff != yaw {
            self.store.set(pawn, Prop::Rotation, Value::Rotator([pitch, yaw & 0xffff, roll].into()));
        }
        let yaw = (yaw & 0xffff) as f32;
        let mut old_yaw = self.store.float_of(pawn, Prop::OldRotYaw);
        if yaw - old_yaw > 32768.0 {
            old_yaw += 65536.0;
        } else if old_yaw - yaw > 32768.0 {
            old_yaw -= 65536.0;
        }
        if self.store.physics_of(pawn) == Some(EPhysics::Walking) && self.store.byte_of(pawn, Prop::Role) > ROLE_SIMULATED_PROXY {
            let turn_rate = (yaw - old_yaw) / delta;
            for (channel, turning, anim) in [
                (RIGHT_TURN_CHANNEL, turn_rate > MIN_TURN_RATE, Prop::TurnRightAnim),
                (LEFT_TURN_CHANNEL, turn_rate < -MIN_TURN_RATE, Prop::TurnLeftAnim),
            ] {
                if turning {
                    let sequence = self.name_of(pawn, anim);
                    self.set_anim_sequence(pawn, channel, sequence);
                    self.update_blend_alpha(pawn, channel, 1.0, step);
                    self.set_anim_rate(pawn, channel, turn_rate.abs() / TURN_ANIM_YAW_RATE);
                } else {
                    self.zero_movement_alpha(pawn, channel, channel + 1, step);
                }
            }
        }
        self.store.set(pawn, Prop::OldRotYaw, Value::Float(yaw));
    }

    fn zero_movement_alpha(&mut self, pawn: ObjectId, start: i32, end: i32, step: f32) {
        let mut all_out = true;
        for channel in start..end {
            if self.blend_alpha(pawn, channel) > 0.0 {
                all_out = false;
                self.update_blend_alpha(pawn, channel, 0.0, step);
            }
        }
        if all_out {
            for channel in start..end {
                if let Some(state) = self.channel_mut(pawn, channel) {
                    state.rate = 0.0;
                    state.frame = 0.0;
                }
            }
        }
    }

    fn set_anim_rate(&mut self, pawn: ObjectId, channel: i32, rate: f32) {
        if let Some(state) = self.channel_mut(pawn, channel) {
            state.rate = state.orig_rate * rate;
        }
    }

    fn blend_alpha(&self, pawn: ObjectId, channel: i32) -> f32 {
        self.channel(pawn, channel).map_or(0.0, |state| state.blend_alpha)
    }

    fn movement_anim(&self, pawn: ObjectId, index: usize) -> NameId {
        match self.store.element_of(pawn, Prop::MovementAnims, index) {
            Some(Value::Name(name)) => name,
            _ => NameId::NONE,
        }
    }

    fn name_of(&self, object: ObjectId, property: Prop) -> NameId {
        match self.store.get(object, property) {
            Some(Value::Name(name)) => name,
            _ => NameId::NONE,
        }
    }
}

#[cfg(test)]
mod twist_tests {
    use super::{FootTwist, TWIST_MAX, TWIST_START};

    const STILL: FootTwist = FootTwist { rot: 1000, turn_dir: 0, turning: false, still: true };

    #[test]
    fn standing_feet_stay_until_the_body_is_far_enough_round() {
        assert_eq!(STILL.step(1000 + TWIST_START - 1, true, 0.1), STILL);
        let turned = STILL.step(1000 + TWIST_START, true, 0.1);
        assert!(turned.turning && turned.turn_dir == 1 && turned.rot == 1000);
        assert_eq!(STILL.step(1000 - TWIST_START, true, 0.1).turn_dir, -1);
    }

    #[test]
    fn turning_feet_follow_within_the_limit_and_stop_near_the_body() {
        let turning = FootTwist { turning: true, ..STILL };
        assert_eq!(turning.step(1000 + TWIST_MAX + 500, true, 0.1).rot, 1500);
        assert_eq!(turning.step(1000 + 4096, true, 0.5).rot, 1000 + 8192);
        let done = turning.step(1000 + 100, true, 0.5);
        assert!(!done.turning && done.rot == 1000);
    }

    #[test]
    fn the_short_way_round_counts_and_starting_to_stand_puts_the_feet_on_the_body() {
        let wrapped = FootTwist { rot: 65000, ..STILL }.step(100, true, 0.1);
        assert!(!wrapped.turning);
        let stood = FootTwist { rot: 0, turn_dir: 0, turning: false, still: false }.step(30000, true, 0.1);
        assert!(stood.still && !stood.turning && stood.rot == 30000);
        assert!(STILL.step(1000 + 4096, false, 0.1).turning);
    }
}
