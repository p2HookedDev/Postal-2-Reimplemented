use p2_math::{Rotator, Vector};
use crate::ai_move::LatentMove;
use crate::world::trace;
use crate::{LoaderRef, ObjectId, Prop, Value, Vm};

/// `FindStairRotation` keeps the pitch at frames longer than this; compared as a double, as retail.
const STAIR_MAX_FRAME: f64 = 0.33;
const STAIR_PITCH_UP: i32 = 3600;
const STAIR_PITCH_DOWN: i32 = -4000;
const STAIR_STEP: f32 = 10.0;

fn signed_pitch(pitch: i32) -> i32 {
    if pitch > 0x8000 { (pitch & 0xffff) - 0x10000 } else { pitch }
}

fn stair_pitch_rising(pitch: i32, first_down: f32, second_down: f32) -> i32 {
    if second_down < first_down - STAIR_STEP { STAIR_PITCH_UP } else { pitch.max(0) }
}

fn stair_pitch_falling(pitch: i32, first_down: f32, second_down: f32) -> i32 {
    if first_down + STAIR_STEP < second_down { STAIR_PITCH_DOWN } else { pitch.min(0) }
}

/// The new pitch, and whether `GroundPitchTime` restarts. Retail tests `stair` itself against 10, without its absolute
/// value, so a settled downward pitch restarts the time too.
fn stair_step(pitch: i32, stair: i32, delta: f32, since_ground_pitch: f32) -> (i32, bool) {
    let difference = (pitch - stair).abs();
    if difference > 0 && since_ground_pitch > 0.25 {
        let rate = if difference < 1000 { (4000 / difference) as f32 } else { 4.0 };
        let turn = (rate * delta).min(1.0);
        return (((1.0 - turn) * pitch as f32 + stair as f32 * turn).round_ties_even() as i32, false);
    }
    (pitch, difference < 10 && stair < 10)
}

pub(crate) fn suggest_jump_velocity(to: Vector, mut xy_speed: f32, base_z: f32, jump_z: f32, ground_speed: f32, gravity_z: f32) -> Vector {
    let flat = Vector::new(to.x, to.y, 0.0);
    let distance = flat.size();
    if distance == 0.0 || xy_speed == 0.0 {
        return Vector::new(0.0, 0.0, base_z.max(jump_z));
    }
    let mut z = 0.0;
    if gravity_z < 0.0 {
        let time = distance / xy_speed;
        z = to.z / time - gravity_z * 0.5 * time;
        if z < base_z {
            let root = f64::from(gravity_z * 2.0 * to.z + base_z * base_z).sqrt();
            let mut flight = ((root - f64::from(base_z)) / f64::from(gravity_z)) as f32;
            if flight < 0.05 {
                flight = 0.05;
            }
            // Written out: a negative root gives NaN, and then retail takes `ground_speed`.
            xy_speed = if distance / flight < ground_speed { distance / flight } else { ground_speed };
            z = base_z;
        }
        if jump_z + base_z < z {
            z = jump_z + base_z;
        }
    }
    let horizontal = flat / distance * xy_speed;
    Vector::new(horizontal.x, horizontal.y, z)
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn find_stair_rotation(&mut self, controller: ObjectId, pawn: Option<ObjectId>, delta: f32) -> i32 {
        let mut rotation = self.store.rotator_of(controller, Prop::Rotation);
        let Some(pawn) = pawn.filter(|_| f64::from(delta) <= STAIR_MAX_FRAME) else { return rotation.pitch };
        rotation.pitch = signed_pitch(rotation.pitch);
        self.store.set(controller, Prop::Rotation, Value::Rotator(rotation));
        let stair = self.stair_pitch(controller, pawn, rotation);
        let now = self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds));
        let since = now - self.store.float_of(controller, Prop::GroundPitchTime);
        let (pitch, restart) = stair_step(rotation.pitch, stair, delta, since);
        if restart {
            self.store.set(controller, Prop::GroundPitchTime, Value::Float(now));
        }
        pitch
    }

    fn stair_pitch(&mut self, controller: ObjectId, pawn: ObjectId, rotation: Rotator) -> i32 {
        let location = self.store.vector(pawn, Prop::Location);
        let eye_height = self.store.float_of(pawn, Prop::BaseEyeHeight);
        let eyes = location + Vector::new(0.0, 0.0, eye_height);
        let height = self.store.float_of(pawn, Prop::CollisionHeight) + eye_height;
        let radius = self.store.float_of(pawn, Prop::CollisionRadius);
        let extent = Vector::new(radius, radius, 1.0);
        let direction = Rotator::new(0, rotation.yaw, rotation.roll).vector();
        let source = Some(controller);
        let time = |vm: &mut Self, start: Vector, end: Vector| vm.first_hit(start, end, extent, source, trace::WORLD).map_or(1.0, |(_, hit)| hit.time);

        let reach = height * 2.0;
        let distance = reach * time(self, eyes, eyes + direction * reach);
        if f64::from(height) * 0.8 >= f64::from(distance) {
            return 0;
        }
        let down = height * 3.0;
        let drop = Vector::new(0.0, 0.0, down);
        let spot = eyes + direction * (distance * 0.5);
        let first = time(self, spot, spot - drop);
        if first >= 1.0 {
            return 0;
        }
        let first_down = down * first;
        let level = height * 0.7;
        let far = eyes + direction * distance;
        if first_down < level - 6.0 {
            let second_down = down * time(self, far, far - drop);
            stair_pitch_rising(rotation.pitch, first_down, second_down)
        } else if first_down > level + 6.0 {
            let clear_to = location + direction * ((f64::from(distance) * 0.9) as f32);
            if self.any_hit(location, clear_to, Vector::ZERO, source, trace::WORLD) {
                return 0;
            }
            let second_down = down * time(self, far, far - drop);
            stair_pitch_falling(rotation.pitch, first_down, second_down)
        } else {
            0
        }
    }

    pub(crate) fn end_climb_ladder(&mut self, controller: ObjectId, pawn: Option<ObjectId>) {
        let moving = self.script.states.objects.get(&controller).and_then(|s| s.latent_move) == Some(LatentMove::MoveToward);
        let target = self.store.object_of(controller, Prop::MoveTarget).filter(|&target| self.store.is_a(target, "Ladder"));
        let (true, Some(pawn), Some(target)) = (moving, pawn, target) else { return };
        if self.may_overlap(pawn, target) && self.shapes_overlap(pawn, target) {
            self.store.set(pawn, Prop::Anchor, Value::Object(Some(target)));
        }
        if let Some(entry) = self.script.states.objects.get_mut(&controller) {
            entry.latent_move = None;
        }
    }

    pub(crate) fn stop_waiting(&mut self, controller: ObjectId) {
        let Some(entry) = self.script.states.objects.get_mut(&controller) else { return };
        if entry.sleep > 0.0 {
            entry.sleep = 0.0;
            self.store.set(controller, Prop::LatentFloat, Value::Float(-1.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_wraps_only_above_half_a_turn() {
        assert_eq!(signed_pitch(0xf000), -4096);
        assert_eq!(signed_pitch(0x8000), 0x8000);
        assert_eq!(signed_pitch(-100), -100);
    }

    #[test]
    fn stair_targets_need_a_second_step() {
        assert_eq!(stair_pitch_rising(500, 50.0, 30.0), STAIR_PITCH_UP);
        assert_eq!(stair_pitch_rising(500, 50.0, 45.0), 500);
        assert_eq!(stair_pitch_rising(-500, 50.0, 45.0), 0);
        assert_eq!(stair_pitch_falling(-500, 50.0, 70.0), STAIR_PITCH_DOWN);
        assert_eq!(stair_pitch_falling(-500, 50.0, 55.0), -500);
        assert_eq!(stair_pitch_falling(500, 50.0, 55.0), 0);
    }

    #[test]
    fn stair_step_turns_after_the_wait() {
        assert_eq!(stair_step(0, 3600, 0.1, 1.0), (1440, false));
        assert_eq!(stair_step(500, 0, 0.01, 1.0), (460, false));
        assert_eq!(stair_step(0, 3600, 0.3, 1.0), (3600, false));
        assert_eq!(stair_step(500, 0, 0.1, 0.1), (500, false));
    }

    #[test]
    fn stair_step_restarts_the_time_when_settled() {
        assert_eq!(stair_step(5, 0, 0.1, 0.1), (5, true));
        assert_eq!(stair_step(STAIR_PITCH_DOWN, STAIR_PITCH_DOWN, 0.1, 0.1), (STAIR_PITCH_DOWN, true));
        assert_eq!(stair_step(STAIR_PITCH_UP, STAIR_PITCH_UP, 0.1, 0.1), (STAIR_PITCH_UP, false));
    }

    #[test]
    fn jump_straight_up_without_distance_or_speed() {
        let velocity = suggest_jump_velocity(Vector::new(0.0, 0.0, 50.0), 400.0, 100.0, 340.0, 500.0, -950.0);
        assert_eq!(velocity, Vector::new(0.0, 0.0, 340.0));
        let velocity = suggest_jump_velocity(Vector::new(100.0, 0.0, 0.0), 0.0, 500.0, 340.0, 500.0, -950.0);
        assert_eq!(velocity, Vector::new(0.0, 0.0, 500.0));
    }

    #[test]
    fn jump_without_downward_gravity_is_horizontal() {
        let velocity = suggest_jump_velocity(Vector::new(100.0, 0.0, 50.0), 200.0, 100.0, 340.0, 500.0, 0.0);
        assert_eq!(velocity, Vector::new(200.0, 0.0, 0.0));
    }

    #[test]
    fn jump_vertical_speed_is_capped() {
        let velocity = suggest_jump_velocity(Vector::new(400.0, 0.0, 0.0), 400.0, 100.0, 340.0, 500.0, -950.0);
        assert_eq!(velocity, Vector::new(400.0, 0.0, 440.0));
    }

    #[test]
    fn jump_below_base_speed_slows_the_horizontal() {
        let velocity = suggest_jump_velocity(Vector::new(100.0, 0.0, 0.0), 1000.0, 300.0, 340.0, 600.0, -950.0);
        assert_eq!(velocity, Vector::new(600.0, 0.0, 300.0));
    }
}
