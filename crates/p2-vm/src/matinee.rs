use p2_math::{Rotator, Vector};
use crate::interp::{float, int};
use crate::natives::vec3;

use crate::{Error, Kind, LoaderRef, NameId, ObjectId, Prop, Value, Vm};

const SAMPLES: usize = 100;
const LAST_SAMPLE: i32 = SAMPLES as i32 - 1;
const MIN_PCT: f32 = 1e-4;
const MAX_PCT: f32 = 100.0;
const SKIP_MARGIN: f32 = 0.01;
const REVERSE_TURN: f32 = 65535.0;
const SASTATUS_RUNNING: u8 = 1;
const SASTATUS_ENDING: u8 = 2;
const SASTATUS_EXPIRED: u8 = 3;
const CAMORIENT_LOOK_AT_ACTOR: u8 = 1;
const CAMORIENT_FACE_PATH: u8 = 2;
const CAMORIENT_INTERPOLATE: u8 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenOverlay {
    /// `FlashScale.X`: how much of the scene shows, 1 all, 0 only fog colour. Blending is (unverified).
    pub flash_scale: f32,
    pub flash_fog: [f32; 3],
    pub titles: Vec<TitleOverlay>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TitleOverlay {
    pub material: Option<ObjectId>,
    pub color: [u8; 4],
    pub size: f32,
    pub x: f32,
    pub y: f32,
    pub style: u8,
    pub alpha: f32,
}

/// Truncated, not wrapped, with 65535 units per turn, as retail.
fn rotation(Vector { x, y, z }: Vector) -> [f32; 3] {
    let units = |angle: f64| (angle * 65535.0 / std::f64::consts::TAU) as i32 as f32;
    let horizontal = f64::from(x * x + y * y).sqrt();
    [units(f64::from(z).atan2(horizontal)), units(f64::from(y).atan2(f64::from(x))), 0.0]
}

fn progress(value: f32) -> f32 {
    if value.is_nan() || value < MIN_PCT {
        MIN_PCT
    } else {
        value.min(1.0)
    }
}

fn bezier(points: [Vector; 4], count: usize) -> (Vec<Vector>, f32) {
    let [p0, p1, p2, p3] = points;
    let mut samples = Vec::with_capacity(count);
    let mut length = 0.0;
    for i in 0..count {
        let t = i as f32 / (count - 1).max(1) as f32;
        let u = 1.0 - t;
        let point: Vector = [0, 1, 2].map(|k| u * u * u * p0[k] + 3.0 * t * u * u * p1[k] + 3.0 * t * t * u * p2[k] + t * t * t * p3[k]).into();
        if let Some(&last) = samples.last() {
            length += (point - last).size();
        }
        samples.push(point);
    }
    (samples, length)
}

impl<L: LoaderRef> Vm<L> {
    fn objects_in(&self, object: ObjectId, property: impl crate::PropertyName) -> Vec<ObjectId> {
        match self.get(object, property) {
            Some(Value::Array(list)) => list.iter().filter_map(|v| if let &Value::Object(o) = v { o } else { None }).collect(),
            _ => Vec::new(),
        }
    }

    fn vectors_in(&self, object: ObjectId, property: impl crate::PropertyName) -> Vec<Vector> {
        match self.get(object, property) {
            Some(Value::Array(list)) => list.iter().map(|v| vec3(Some(v))).collect(),
            _ => Vec::new(),
        }
    }

    fn float_at(&self, object: ObjectId, path: &[&str]) -> f32 {
        self.get_path(object, path).as_ref().map_or(0.0, float)
    }

    /// The interpolator is done at value 1, so an orientation without ease-in time applies at once.
    pub(crate) fn construct_scene_manager(&mut self, scene: ObjectId) {
        for orientation in ["CamOrientation", "PrevOrientation"] {
            self.set_path(scene, &[orientation, "CamOrientation"], Value::Byte(CAMORIENT_INTERPOLATE));
        }
        self.set_path(scene, &["RotInterpolator", "bDone"], Value::Int(1));
        self.set_path(scene, &["RotInterpolator", "_value"], Value::Float(1.0));
    }

    pub(crate) fn total_scene_time(&self, scene: ObjectId) -> f32 {
        self.objects_in(scene, Prop::Actions).into_iter().map(|action| self.float_of(action, Prop::Duration)).sum()
    }

    fn prev_action(&self, scene: ObjectId, action: ObjectId) -> Option<ObjectId> {
        let actions = self.objects_in(scene, Prop::Actions);
        match actions.iter().position(|&a| a == action) {
            Some(index) if index > 0 => Some(actions[index - 1]),
            _ => actions.last().copied(),
        }
    }

    fn next_action(&self, scene: ObjectId, action: ObjectId) -> Option<ObjectId> {
        let actions = self.objects_in(scene, Prop::Actions);
        match actions.iter().position(|&a| a == action) {
            Some(index) if index + 1 < actions.len() => Some(actions[index + 1]),
            _ => actions.first().copied(),
        }
    }

    fn path_samples(&mut self, scene: ObjectId, from: ObjectId) -> Vec<Vector> {
        let Some(to) = self.next_action(scene, from).filter(|&to| to != from) else { return Vec::new() };
        let (Some(start), Some(end)) = (self.object_of(from, Prop::IntPoint), self.object_of(to, Prop::IntPoint)) else { return Vec::new() };
        let (p0, p3) = (self.vec3_of(start, Prop::Location), self.vec3_of(end, Prop::Location));
        let bezier_style = self.is_a(to, Kind::ActionMoveCamera) && self.byte_of(to, Prop::PathStyle) != 0;
        let (p1, p2) = if bezier_style {
            ((p0 + self.vec3_of(from, Prop::StartControlPoint)), (p3 + self.vec3_of(to, Prop::EndControlPoint)))
        } else {
            let difference = p3 - p0;
            let length = difference.size();
            let direction = if length * length > 1e-8 { difference * (1.0 / length) } else { difference };
            let third = direction * (length / 3.0);
            ((p0 + third), (p3 - third))
        };
        let (samples, length) = bezier([p0, p1, p2, p3], SAMPLES);
        self.set(to, Prop::PathLength, Value::Float(length));
        samples
    }

    pub(crate) fn prepare_path(&mut self, scene: ObjectId) {
        let mut all = Vec::new();
        for action in self.objects_in(scene, Prop::Actions) {
            let samples = self.prev_action(scene, action).map(|prev| self.path_samples(scene, prev)).unwrap_or_default();
            all.extend(samples.iter().copied().map(Value::Vector));
            self.set(action, Prop::SampleLocations, Value::Array(samples.into_iter().map(Value::Vector).collect()));
            let velocity = self.float_of(action, Prop::PathVelocity);
            if self.flag_of(action, Prop::bConstantPathVelocity) && velocity != 0.0 {
                let duration = self.float_of(action, Prop::PathLength) / velocity;
                self.set(action, Prop::Duration, Value::Float(duration));
            }
        }
        self.set(scene, Prop::SampleLocations, Value::Array(all));
    }

    fn set_scene_start_time(&mut self, scene: ObjectId) {
        let total = self.total_scene_time(scene);
        self.set(scene, Prop::TotalSceneTime, Value::Float(total));
        self.set(scene, Prop::CurrentTime, Value::Float(0.0));
        let mut all = Vec::new();
        let mut start = 0.0;
        for action in self.objects_in(scene, Prop::Actions) {
            let duration = self.float_of(action, Prop::Duration);
            let (first, last) = (start / total, (duration + start) / total);
            self.set(action, Prop::PctStarting, Value::Float(first));
            self.set(action, Prop::PctEnding, Value::Float(last));
            self.set(action, Prop::PctDuration, Value::Float(last - first));
            for sub_action in self.objects_in(action, Prop::SubActions) {
                let delay = self.float_of(sub_action, Prop::Delay);
                let (first, last) = ((delay + start) / total, (delay + start + self.float_of(sub_action, Prop::Duration)) / total);
                self.set(sub_action, Prop::Status, Value::Byte(0));
                self.set(sub_action, Prop::PctStarting, Value::Float(first));
                self.set(sub_action, Prop::PctEnding, Value::Float(last));
                self.set(sub_action, Prop::PctDuration, Value::Float(last - first));
                if self.is_a(sub_action, Kind::SubActionOrientation) {
                    let ease_end = self.float_at(sub_action, &["CamOrientation", "EaseIntime"]) / total + first;
                    self.set_path(sub_action, &["CamOrientation", "PctInStart"], Value::Float(first));
                    self.set_path(sub_action, &["CamOrientation", "PctInEnd"], Value::Float(ease_end));
                    self.set_path(sub_action, &["CamOrientation", "PctInDuration"], Value::Float(ease_end - first));
                }
                all.push(Value::Object(Some(sub_action)));
            }
            start += duration;
        }
        self.set(scene, Prop::SubActions, Value::Array(all));
    }

    fn scene_started(&mut self, scene: ObjectId) -> Result<(), Error> {
        self.set(scene, Prop::bIsRunning, Value::Bool(true));
        let first = self.objects_in(scene, Prop::Actions).first().copied();
        if self.vectors_in(scene, Prop::SampleLocations).is_empty() && first.is_some_and(|first| self.vectors_in(first, Prop::SampleLocations).is_empty()) {
            self.prepare_path(scene);
        }
        self.set_scene_start_time(scene);
        self.event(scene, "SceneStarted", Vec::new())?;
        for orientation in ["CamOrientation", "PrevOrientation"] {
            if let Some(value) = self.get(scene, orientation).as_ref().map(zero_like) {
                self.set(scene, orientation, value);
            }
        }
        self.set(scene, Prop::SceneSpeed, Value::Float(1.0));
        self.set(scene, Prop::TimeDilation, Value::Float(1.0));
        self.set(scene, Prop::CurrentAction, Value::Object(None));
        Ok(())
    }

    fn scene_ended(&mut self, scene: ObjectId) -> Result<(), Error> {
        self.set(scene, Prop::bIsRunning, Value::Bool(false));
        self.set(scene, Prop::bIsSceneStarted, Value::Bool(false));
        self.set_path(scene, &["PrevOrientation", "MA"], Value::Int(0));
        self.event(scene, "SceneEnded", Vec::new())?;
        for action in self.objects_in(scene, Prop::Actions) {
            for sub_action in self.objects_in(action, Prop::SubActions) {
                let looks = self.get_path(sub_action, &["CamOrientation", "CamOrientation"]) == Some(Value::Byte(CAMORIENT_LOOK_AT_ACTOR));
                if self.is_a(sub_action, Kind::SubActionOrientation) && looks {
                    self.set_path(sub_action, &["CamOrientation", "LookAt"], Value::Object(None));
                }
            }
        }
        self.set(scene, Prop::SampleLocations, Value::Array(Vec::new()));
        if let Some(level) = self.level()
            && let Some(game) = self.object_of(level, Prop::Game)
        {
            let speed = self.get(game, Prop::GameSpeed).unwrap_or(Value::Float(1.0));
            self.set(level, Prop::TimeDilation, speed);
        }
        Ok(())
    }

    pub(crate) fn scene_tick(&mut self, scene: ObjectId, delta: f32) -> Result<(), Error> {
        if !self.flag_of(scene, Prop::bIsRunning) {
            return Ok(());
        }
        if !self.flag_of(scene, Prop::bIsSceneStarted) {
            self.set(scene, Prop::bIsSceneStarted, Value::Bool(true));
            self.scene_started(scene)?;
            let Some(viewer) = self.object_of(scene, Prop::Viewer) else {
                self.diagnostics.log.push(format!("{}: scene started without a Viewer", self.object(scene).path));
                return Ok(());
            };
            if let Some(&first) = self.objects_in(scene, Prop::Actions).first()
                && let Some(point) = self.object_of(first, Prop::IntPoint)
            {
                let rotation = self.rotator_of(point, Prop::Rotation);
                self.set(viewer, Prop::Rotation, Value::Rotator(rotation));
            }
        } else {
            self.tick_rotation_interpolator(scene, delta);
            let time = self.float_of(scene, Prop::SceneSpeed) * delta + self.float_of(scene, Prop::CurrentTime);
            self.set(scene, Prop::CurrentTime, Value::Float(time));
        }
        let Some(viewer) = self.object_of(scene, Prop::Viewer) else { return Ok(()) };
        let viewer_controller = self.is_a(viewer, Kind::PlayerController);
        let total = self.float_of(scene, Prop::TotalSceneTime);
        if self.flag_of(scene, Prop::bLetPlayerSkip) && viewer_controller && self.byte_of(viewer, Prop::bWantsToSkip) != 0 {
            self.set(viewer, Prop::bWantsToSkip, Value::Byte(0));
            self.set(scene, Prop::CurrentTime, Value::Float(total - SKIP_MARGIN));
        }
        let pct = self.float_of(scene, Prop::CurrentTime) / total;
        self.set(scene, Prop::PctSceneComplete, Value::Float(pct));
        if pct <= 1.0 {
            let before = self.vec3_of(viewer, Prop::Location);
            self.update_viewer(scene, viewer, pct)?;
            if delta > 0.0 {
                let after = self.vec3_of(viewer, Prop::Location);
                self.set_vector(viewer, Prop::Velocity, (after - before) * (1.0 / delta));
            }
            let Some(level) = self.level() else { return Ok(()) };
            let mut speed = 1.0;
            let game = self.object_of(level, Prop::Game);
            if !self.flag_of(scene, Prop::bLetPlayerSkip) && self.flag_of(scene, Prop::bLetPlayerFastForward) && viewer_controller
                && let Some(game) = game
                && self.byte_of(viewer, Prop::bWantsToSkip) != 0
            {
                speed = self.float_of(game, Prop::MatineeFastForwardSpeed);
            }
            let dilation = self.float_of(scene, Prop::TimeDilation) * speed;
            self.set(level, Prop::TimeDilation, Value::Float(dilation));
        } else {
            self.scene_ended(scene)?;
            let next = match self.get(scene, Prop::NextSceneTag) {
                Some(Value::Name(name)) if name != NameId::NONE => Some(name),
                _ if self.flag_of(scene, Prop::bLooping) => match self.get(scene, Prop::Tag) {
                    Some(Value::Name(name)) => Some(name),
                    _ => None,
                },
                _ => None,
            };
            if let Some(name) = next {
                let args = vec![Value::Name(name), Value::Object(Some(viewer)), Value::Object(self.object_of(scene, Prop::OldPawn))];
                self.event(scene, "TriggerEvent", args)?;
            }
        }
        Ok(())
    }

    fn tick_rotation_interpolator(&mut self, scene: ObjectId, delta: f32) {
        let at = |vm: &Self, member: &str| vm.get_path(scene, &["RotInterpolator", member]).unwrap_or(Value::Float(0.0));
        if int(&at(self, "bDone")) != 0 {
            return;
        }
        let remaining = float(&at(self, "_remainingTime")) - delta;
        let acceleration = float(&at(self, "_acceleration"));
        let speed = float(&at(self, "_speed"));
        let speed = if float(&at(self, "_totalTime")) * 0.5 <= remaining { acceleration * delta + speed } else { speed - acceleration * delta };
        let value = delta * speed + float(&at(self, "_value"));
        for (member, value) in [
            ("_remainingTime", Value::Float(remaining)),
            ("_speed", Value::Float(speed)),
            ("bDone", Value::Int(i32::from(remaining < 0.0))),
            ("_value", Value::Float(value)),
        ] {
            self.set_path(scene, &["RotInterpolator", member], value);
        }
    }

    fn start_rotation_interpolator(&mut self, scene: ObjectId, time: f32) {
        let set = |vm: &mut Self, member: &str, value: Value| vm.set_path(scene, &["RotInterpolator", member], value);
        if time <= 0.0 {
            set(self, "bDone", Value::Int(1));
            return;
        }
        set(self, "bDone", Value::Int(0));
        for (member, value) in [("_value", 0.0), ("_speed", 0.0), ("_totalTime", time), ("_remainingTime", time), ("_acceleration", 1.0 / (time * time * 0.25))] {
            set(self, member, Value::Float(value));
        }
    }

    fn change_orientation(&mut self, scene: ObjectId, mut orientation: Value) {
        let Value::Struct(members) = &mut orientation else { return };
        let looks = members.first() == Some(&Value::Byte(CAMORIENT_LOOK_AT_ACTOR));
        if looks
            && members.get(1) == Some(&Value::Object(None))
            && let Some(&Value::Name(tag)) = members.get(2)
            && tag != NameId::NONE
        {
            let found = self.actors().find(|&o| self.is_a(o, Kind::Actor) && !self.is_deleted(o) && self.get(o, Prop::Tag) == Some(Value::Name(tag)));
            if let Some(found) = found {
                members[1] = Value::Object(Some(found));
            }
        }
        let ease = members.get(3).map_or(0.0, float);
        self.set(scene, Prop::CamOrientation, orientation);
        if let Some(viewer) = self.object_of(scene, Prop::Viewer) {
            let rotation = self.rotator_of(viewer, Prop::Rotation);
            self.set_path(scene, &["CamOrientation", "StartingRotation"], Value::Rotator(rotation));
        }
        self.start_rotation_interpolator(scene, ease);
    }

    fn action_from_pct(&self, scene: ObjectId, pct: f32) -> Option<ObjectId> {
        self.objects_in(scene, Prop::Actions).into_iter().find(|&action| pct <= self.float_of(action, Prop::PctEnding))
    }

    fn action_pct(&mut self, scene: ObjectId, pct: f32) -> f32 {
        let action = match self.object_of(scene, Prop::CurrentAction) {
            Some(action) => Some(action),
            None => {
                let action = self.action_from_pct(scene, pct);
                self.set(scene, Prop::CurrentAction, Value::Object(action));
                action
            }
        };
        let Some(action) = action else { return MIN_PCT };
        let mut length = self.float_of(action, Prop::PctDuration);
        if length == 0.0 {
            length = 1.0;
        }
        let value = (pct - self.float_of(action, Prop::PctStarting)) / length;
        if value.is_nan() || value < MIN_PCT { MIN_PCT } else { value.min(MAX_PCT) }
    }

    fn scene_location(&mut self, scene: ObjectId, action: ObjectId, pct: f32) -> Vector {
        let along = self.action_pct(scene, pct);
        let samples = self.vectors_in(action, Prop::SampleLocations);
        if samples.is_empty() {
            let first = self.objects_in(scene, Prop::Actions).first().and_then(|&first| self.object_of(first, Prop::IntPoint));
            return first.map_or(Vector::ZERO, |point| self.vec3_of(point, Prop::Location));
        }
        let position = (samples.len() - 1) as f32 * along;
        let index = position as usize;
        let Some(&base) = samples.get(index) else { return samples[samples.len() - 1] };
        let fraction = position - index as f32;
        match samples.get(index + 1) {
            Some(&next) if fraction != 0.0 => base + ((next - base) * fraction),
            _ => base,
        }
    }

    fn path_sample(&self, scene: ObjectId, index: i32, action: ObjectId, samples: &[Vector]) -> Vector {
        let (mut index, mut owner) = (index, Some(action));
        if index > LAST_SAMPLE - 1 {
            index -= LAST_SAMPLE;
            owner = self.next_action(scene, action);
        }
        if owner.is_some() && owner == self.objects_in(scene, Prop::Actions).first().copied() {
            (index, owner) = (LAST_SAMPLE, Some(action));
        }
        let sample = |owner: ObjectId| match owner == action {
            true => samples.get(index as usize).copied(),
            false => self.vectors_in(owner, Prop::SampleLocations).get(index as usize).copied(),
        };
        owner.and_then(sample).unwrap_or(Vector::ZERO)
    }

    fn scene_rotation(&mut self, scene: ObjectId, location: Vector, current: Rotator, action: Option<ObjectId>, pct: f32) -> Rotator {
        let along = self.action_pct(scene, pct);
        let mode = self.get_path(scene, &["CamOrientation", "CamOrientation"]).map_or(0, |v| int(&v) as u8);
        let mut target = <[i32; 3]>::from(current).map(|c| c as f32);
        match (mode, action) {
            (CAMORIENT_FACE_PATH, Some(action)) => {
                let position = along * LAST_SAMPLE as f32;
                let index = position as i32;
                let samples = self.vectors_in(action, Prop::SampleLocations);
                let [p0, p1, p2] = [0, 1, 2].map(|k| self.path_sample(scene, index + k, action, &samples));
                let fraction = position - index as f32;
                target = rotation(((p2 - p1) * fraction) + ((p1 - p0) * (1.0 - fraction)));
            }
            (CAMORIENT_LOOK_AT_ACTOR, _) => {
                if let Some(Value::Object(Some(look_at))) = self.get_path(scene, &["CamOrientation", "LookAt"]) {
                    target = rotation(self.vec3_of(look_at, Prop::Location) - location);
                }
            }
            (CAMORIENT_INTERPOLATE, Some(action)) => {
                let prev = self.prev_action(scene, action);
                if let (Some(to), Some(from)) = (self.object_of(action, Prop::IntPoint), prev.and_then(|prev| self.object_of(prev, Prop::IntPoint))) {
                    let (to, from) = (self.rotator_of(to, Prop::Rotation), self.rotator_of(from, Prop::Rotation));
                    target = [0, 1, 2].map(|i| from[i] as f32 + to[i].wrapping_sub(from[i]) as f32 * along);
                }
            }
            _ => {}
        }
        let alpha = self.float_at(scene, &["RotInterpolator", "_value"]);
        let start = vec3(self.get_path(scene, &["CamOrientation", "StartingRotation"]));
        let shake = self.vec3_of(scene, Prop::CameraShake);
        let reverse = ["bReversePitch", "bReverseYaw", "bReverseRoll"].map(|flag| self.get_path(scene, &["CamOrientation", flag]).is_some_and(|v| int(&v) != 0));
        [0, 1, 2].map(|i| {
            let mut difference = target[i] - start[i];
            if reverse[i] {
                difference += if difference >= 0.0 { -REVERSE_TURN } else { REVERSE_TURN };
            }
            (shake[i] + start[i] + difference * alpha) as i32
        }).into()
    }

    fn update_viewer(&mut self, scene: ObjectId, viewer: ObjectId, pct: f32) -> Result<(), Error> {
        let pct = if pct < MIN_PCT { MIN_PCT } else { pct.min(MAX_PCT) };
        let action = self.action_from_pct(scene, pct);
        self.set(scene, Prop::CurrentAction, Value::Object(action));
        if let Some(action) = action {
            let location = self.scene_location(scene, action, pct);
            self.set_location(viewer, location)?;
        }
        self.set_vector(scene, Prop::CameraShake, Vector::ZERO);
        for sub_action in self.objects_in(scene, Prop::SubActions) {
            if self.byte_of(sub_action, Prop::Status) != SASTATUS_EXPIRED {
                self.update_sub_action(scene, sub_action, pct)?;
            }
        }
        let action = self.object_of(scene, Prop::CurrentAction);
        let (location, current) = (self.vec3_of(viewer, Prop::Location), self.rotator_of(viewer, Prop::Rotation));
        let rotation = self.scene_rotation(scene, location, current, action, pct);
        self.set(viewer, Prop::Rotation, Value::Rotator(rotation));
        Ok(())
    }

    fn sub_action_status(&mut self, sub_action: ObjectId, pct: f32) -> Option<bool> {
        if self.byte_of(sub_action, Prop::Status) == SASTATUS_ENDING {
            self.set(sub_action, Prop::Status, Value::Byte(SASTATUS_EXPIRED));
            return None;
        }
        let (start, end) = (self.float_of(sub_action, Prop::PctStarting), self.float_of(sub_action, Prop::PctEnding));
        if start < pct && pct <= end {
            self.set(sub_action, Prop::Status, Value::Byte(SASTATUS_RUNNING));
        } else if pct > end {
            self.set(sub_action, Prop::Status, Value::Byte(SASTATUS_ENDING));
        }
        Some(matches!(self.byte_of(sub_action, Prop::Status), SASTATUS_RUNNING | SASTATUS_ENDING))
    }

    fn sub_action_progress(&self, sub_action: ObjectId, pct: f32) -> f32 {
        if self.byte_of(sub_action, Prop::Status) == SASTATUS_ENDING {
            return 1.0;
        }
        progress((pct - self.float_of(sub_action, Prop::PctStarting)) / self.float_of(sub_action, Prop::PctDuration))
    }

    fn range_value(&mut self, sub_action: ObjectId, property: &str, current: f32, pct: f32) -> f32 {
        if self.float_at(sub_action, &[property, "Min"]) == 0.0 {
            self.set_path(sub_action, &[property, "Min"], Value::Float(current));
        }
        let (min, max) = (self.float_at(sub_action, &[property, "Min"]), self.float_at(sub_action, &[property, "Max"]));
        (max - min) * self.sub_action_progress(sub_action, pct) + min
    }

    fn update_sub_action(&mut self, scene: ObjectId, sub_action: ObjectId, pct: f32) -> Result<(), Error> {
        let Some(running) = self.sub_action_status(sub_action, pct) else { return Ok(()) };
        if !running {
            return Ok(());
        }
        let viewer = self.object_of(scene, Prop::Viewer);
        let controller = viewer.filter(|&viewer| self.is_a(viewer, Kind::PlayerController));
        if self.is_a(sub_action, Kind::SubActionOrientation) {
            if let Some(orientation) = self.get(sub_action, Prop::CamOrientation) {
                self.change_orientation(scene, orientation);
            }
            self.set(sub_action, Prop::Status, Value::Byte(SASTATUS_EXPIRED));
        } else if self.is_a(sub_action, Kind::SubActionFade) {
            if let Some(controller) = controller {
                let channel = |vm: &Self, name: &str| vm.get_path(sub_action, &["FadeColor", name]).map_or(0.0, |v| int(&v) as f32) / 255.0;
                let colour = [channel(self, "R"), channel(self, "G"), channel(self, "B")];
                self.set_vector(controller, Prop::FlashFog, colour.into());
                let alpha = self.sub_action_progress(sub_action, pct);
                let scene_scale = if self.flag_of(sub_action, Prop::bFadeOut) { 1.0 - alpha } else { alpha };
                self.set_path(controller, &["FlashScale", "X"], Value::Float(scene_scale));
            }
        } else if self.is_a(sub_action, Kind::SubActionFOV) {
            if let Some(controller) = controller {
                let current = self.float_of(controller, Prop::FovAngle);
                let fov = self.range_value(sub_action, "FOV", current, pct);
                self.set(controller, Prop::FovAngle, Value::Float(fov));
            }
        } else if self.is_a(sub_action, Kind::SubActionSceneSpeed) {
            let speed = self.range_value(sub_action, "SceneSpeed", self.float_of(scene, Prop::SceneSpeed), pct);
            self.set(scene, Prop::SceneSpeed, Value::Float(speed));
        } else if self.is_a(sub_action, Kind::SubActionGameSpeed) {
            let current = self.level().map_or(1.0, |level| self.float_of(level, Prop::TimeDilation));
            let speed = self.range_value(sub_action, "GameSpeed", current, pct);
            self.set(scene, Prop::TimeDilation, Value::Float(speed));
        } else if self.is_a(sub_action, Kind::SubActionCameraShake) {
            let shake = ["X", "Y", "Z"].map(|axis| (self.float_at(sub_action, &["Shake", axis, "Min"]), self.float_at(sub_action, &["Shake", axis, "Max"])));
            let offset = shake.map(|(min, max)| min + (max - min) * self.random());
            let total = self.vec3_of(scene, Prop::CameraShake) + offset.into();
            self.set_vector(scene, Prop::CameraShake, total);
        } else if self.is_a(sub_action, Kind::SubActionTrigger) {
            let event = self.get(sub_action, Prop::EventName).unwrap_or(Value::Name(NameId::NONE));
            let args = vec![event, Value::Object(viewer), Value::Object(self.object_of(scene, Prop::OldPawn))];
            self.event(scene, "TriggerEvent", args)?;
        } else if self.is_a(sub_action, Kind::SubActionCameraEffect) {
            if let (Some(controller), Some(effect)) = (controller, self.object_of(sub_action, Prop::CameraEffect)) {
                let (start, end) = (self.float_of(sub_action, Prop::StartAlpha), self.float_of(sub_action, Prop::EndAlpha));
                let length = self.float_of(sub_action, Prop::PctDuration);
                let alpha = if length <= 0.0 { end } else { (pct - self.float_of(sub_action, Prop::PctStarting)) / length * (end - start) + start };
                self.set(effect, Prop::Alpha, Value::Float(alpha));
                let ending = self.byte_of(sub_action, Prop::Status) == SASTATUS_ENDING;
                let keep = alpha > 0.0 && !(ending && self.flag_of(sub_action, Prop::DisableAfterDuration));
                self.show_camera_effect(controller, effect, keep)?;
            }
        } else if self.is_a(sub_action, Kind::SubActionTitle) {
            self.update_title(sub_action, controller, pct)?;
        } else if self.is_a(sub_action, Kind::SubActionSceneProps) {
            let property = match self.byte_of(sub_action, Prop::Prop) {
                0 => "bLetPlayerSkip",
                1 => "bLooping",
                _ => return Ok(()),
            };
            let value = self.flag_of(sub_action, Prop::bValue);
            self.set(scene, property, Value::Bool(value));
        }
        Ok(())
    }

    fn show_camera_effect(&mut self, controller: ObjectId, effect: ObjectId, show: bool) -> Result<(), Error> {
        if show {
            self.event(controller, "AddCameraEffect", [Value::Object(Some(effect)), Value::Bool(true)])
        } else {
            self.event(controller, "RemoveCameraEffect", [Value::Object(Some(effect))])
        }
    }

    fn update_title(&mut self, sub_action: ObjectId, controller: Option<ObjectId>, pct: f32) -> Result<(), Error> {
        let (Some(controller), Some(effect)) = (controller, self.object_of(sub_action, Prop::CameraEffect)) else { return Ok(()) };
        let (min, max) = (self.float_of(sub_action, Prop::MinAlpha), self.float_of(sub_action, Prop::MaxAlpha));
        let length = self.float_of(sub_action, Prop::PctDuration);
        let alpha = if length <= 0.0 {
            min
        } else {
            let t = ((pct - self.float_of(sub_action, Prop::PctStarting)) / length).clamp(0.0, 1.0);
            let fade_in = self.float_of(sub_action, Prop::FadeInTimePercent);
            let fade_in_end = if fade_in < 0.0 { 0.0 } else { fade_in.min(1.0) };
            let mut alpha = if fade_in_end <= 0.0 || fade_in_end < t { max } else { (max - min) * (t / fade_in_end) + min };
            let fade_out = self.float_of(sub_action, Prop::FadeOutTimePercent).clamp(0.0, 1.0);
            let fade_out_start = (1.0 - fade_out).max(fade_in_end);
            if fade_out_start > 0.0 && fade_out_start <= t {
                alpha = ((f64::from(t - fade_out_start) / (1.0 - f64::from(fade_out_start))) as f32) * (min - max) + max;
            }
            alpha
        };
        self.set(effect, Prop::Alpha, Value::Float(alpha));
        let ending = self.byte_of(sub_action, Prop::Status) == SASTATUS_ENDING;
        self.show_camera_effect(controller, effect, alpha > 0.0 && !ending)
    }

    pub fn screen_overlay(&self, controller: ObjectId) -> ScreenOverlay {
        let titles = self
            .objects_in(controller, Prop::CameraEffects)
            .into_iter()
            .filter(|&effect| self.is_a(effect, Kind::CameraTitle))
            .map(|effect| {
                let channel = |name: &str| self.get_path(effect, &["OverlayColor", name]).map_or(0, |v| int(&v) as u8);
                TitleOverlay {
                    material: self.object_of(effect, Prop::OverlayMaterial),
                    color: [channel("R"), channel("G"), channel("B"), channel("A")],
                    size: self.float_of(effect, Prop::OverlaySizeFactor),
                    x: self.float_of(effect, Prop::OverlayXPos),
                    y: self.float_of(effect, Prop::OverlayYPos),
                    style: self.byte_of(effect, Prop::OverlayStyle),
                    alpha: self.float_of(effect, Prop::Alpha),
                }
            })
            .collect();
        ScreenOverlay {
            flash_scale: self.float_at(controller, &["FlashScale", "X"]),
            flash_fog: self.vec3_of(controller, Prop::FlashFog).into(),
            titles,
        }
    }
}

fn zero_like(value: &Value) -> Value {
    match value {
        Value::Byte(_) => Value::Byte(0),
        Value::Int(_) => Value::Int(0),
        Value::Bool(_) => Value::Bool(false),
        Value::Float(_) => Value::Float(0.0),
        Value::Object(_) => Value::Object(None),
        Value::Name(_) => Value::Name(NameId::NONE),
        Value::Str(_) => Value::Str(String::new()),
        Value::Array(_) => Value::Array(Vec::new()),
        Value::Struct(members) => Value::Struct(members.iter().map(zero_like).collect()),
        Value::Vector(_) => Value::Vector(Vector::ZERO),
        Value::Rotator(_) => Value::Rotator(Rotator::ZERO),
        Value::Delegate { .. } => Value::Delegate { object: None, function: NameId::NONE },
        Value::Map => Value::Map,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_with_third_points_is_a_straight_even_line() {
        let (p0, p3) = ([0.0, 0.0, 0.0], [300.0, 0.0, 0.0]);
        let (samples, length) = bezier([p0.into(), [100.0, 0.0, 0.0].into(), [200.0, 0.0, 0.0].into(), p3.into()], SAMPLES);
        assert_eq!(samples.len(), SAMPLES);
        assert_eq!(samples[0], p0.into());
        assert!((samples[SAMPLES - 1][0] - 300.0).abs() < 1e-3);
        assert!((samples[33][0] - 100.0).abs() < 1e-3);
        assert!((length - 300.0).abs() < 1e-2);
    }

    #[test]
    fn rotation_truncates_without_wrapping() {
        assert_eq!(rotation([-1.0, 0.0, 0.0].into()), [0.0, 32767.0, 0.0]);
        let [pitch, yaw, _] = rotation([1.0, -1.0, -1.0].into());
        assert!(pitch < 0.0 && yaw == -8191.0);
    }

    #[test]
    fn progress_clamps_like_the_engine() {
        assert_eq!(progress(f32::NAN), MIN_PCT);
        assert_eq!(progress(-1.0), MIN_PCT);
        assert_eq!(progress(2.0), 1.0);
        assert_eq!(progress(0.5), 0.5);
    }
}
