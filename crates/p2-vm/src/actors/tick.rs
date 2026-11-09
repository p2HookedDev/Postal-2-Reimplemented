use crate::enums::{EPhysics, EProbe};
use crate::{Error, Kind, ObjectId, LoaderRef, Prop, Value, Vm};
use super::add_time;

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn profile_start(&self) -> Option<std::time::Instant> {
        self.diagnostics.tick_profile.is_some().then(std::time::Instant::now)
    }

    pub(crate) fn profile_phase(&mut self, name: &str, start: Option<std::time::Instant>) {
        if let (Some(start), Some(profile)) = (start, &mut self.diagnostics.tick_profile) {
            add_time(profile, name, start.elapsed());
        }
    }

    /// In retail's order: animation, `Tick` event, state code, timer and `LifeSpan`, physics.
    pub fn tick_actor(&mut self, actor: ObjectId, delta: f32) -> Result<(), Error> {
        if self.store.is_deleted(actor) {
            return Ok(());
        }
        if self.rests(actor)? {
            self.age(actor, delta)?;
            return Ok(());
        }
        let Some(delta) = self.fps_pawn_tick(actor, delta)? else { return Ok(()) };
        let phase = self.profile_start();
        self.update_animation(actor, delta)?;
        self.profile_phase("phase: animation", phase);
        if self.store.is_deleted(actor) {
            return Ok(());
        }
        // A player controller gets no `Tick` event; the local player's reads input and gets `PlayerTick`.
        let phase = self.profile_start();
        if !self.store.is_a(actor, Kind::PlayerController) {
            self.event(actor, "Tick", [Value::Float(delta)])?;
        } else if self.player() == Some(actor) {
            self.player_tick(actor, delta)?;
        }
        self.profile_phase("phase: timer and Tick", phase);
        let phase = self.profile_start();
        self.tick(actor, delta)?;
        self.profile_phase("phase: state code", phase);
        // An actor whose `LifeSpan` runs out is destroyed without its physics this frame.
        if self.store.is_deleted(actor) {
            return Ok(());
        }
        if let Some(Value::Float(rate)) = self.store.get(actor, Prop::TimerRate)
            && rate > 0.0
        {
            let counter = match self.store.get(actor, Prop::TimerCounter) {
                Some(Value::Float(counter)) => counter + delta,
                _ => delta,
            };
            if counter >= rate {
                if self.store.flag_of(actor, Prop::bTimerLoop) {
                    self.store.set(actor, Prop::TimerCounter, Value::Float(counter - rate));
                } else {
                    self.store.set(actor, Prop::TimerRate, Value::Float(0.0));
                    self.store.set(actor, Prop::TimerCounter, Value::Float(0.0));
                }
                self.event(actor, "Timer", Vec::new())?;
            } else {
                self.store.set(actor, Prop::TimerCounter, Value::Float(counter));
            }
        }
        if self.age(actor, delta)? || self.store.is_deleted(actor) {
            return Ok(());
        }
        if self.store.is_a(actor, Kind::Pawn) {
            let phase = self.profile_start();
            let result = self.pawn_physics(actor, delta);
            self.profile_phase("phase: pawn physics", phase);
            result?;
            if self.store.physics_of(actor) != Some(EPhysics::None) {
                let acceleration = self.store.vector(actor, Prop::Acceleration);
                self.set_vector(actor, Prop::OldAcceleration, acceleration);
            }
            if self.store.is_deleted(actor) {
                return Ok(());
            }
            let phase = self.profile_start();
            let result = self.pawn_tick_special(actor, delta);
            self.profile_phase("phase: movement animation", phase);
            result
        } else if self.store.is_a(actor, Kind::Mover) {
            // Retail's mover physics turns movers too, after their move.
            self.mover_physics(actor, delta)?;
            self.actor_rotation(actor, delta)
        } else {
            let phase = self.profile_start();
            self.actor_physics(actor, delta)?;
            if self.store.is_deleted(actor) {
                return Ok(());
            }
            self.actor_rotation(actor, delta)?;
            self.profile_phase("phase: actor physics", phase);
            if self.store.is_a(actor, Kind::Controller) {
                let phase = self.profile_start();
                if self.store.is_a(actor, Kind::PlayerController) {
                    self.player_sight(actor, delta)?;
                } else {
                    self.controller_tick(actor, delta)?;
                }
                self.profile_phase("phase: controller tick", phase);
            }
            if self.store.is_a(actor, Kind::SceneManager) {
                self.scene_tick(actor, delta)?;
            }
            Ok(())
        }
    }

    fn age(&mut self, actor: ObjectId, delta: f32) -> Result<bool, Error> {
        let life = self.store.float_of(actor, Prop::LifeSpan);
        if life == 0.0 {
            return Ok(false);
        }
        let left = life - delta;
        self.store.set(actor, Prop::LifeSpan, Value::Float(left));
        if left > 1e-4 {
            return Ok(false);
        }
        self.destroy(actor)?;
        Ok(true)
    }

    fn rests(&mut self, actor: ObjectId) -> Result<bool, Error> {
        if [Kind::Pawn, Kind::Controller, Kind::Mover, Kind::SceneManager].into_iter().any(|kind| self.store.is_a(actor, kind))
            || self.has_animation(actor)
            || self.store.physics_of(actor) != Some(EPhysics::None)
            || self.store.float_of(actor, Prop::TimerRate) > 0.0
        {
            return Ok(false);
        }
        if let Some(state) = self.script.states.objects.get(&actor)
            && (state.code.is_some()
                || state.sleep > 0.0
                || state.finish_anim.is_some()
                || state.finish_interpolation.is_some()
                || state.latent_move.is_some())
        {
            return Ok(false);
        }
        if self.store.rotator_of(actor, Prop::RotationRate) != [0; 3].into()
            && !self.store.flag_of(actor, Prop::bInterpolating)
            && (self.store.flag_of(actor, Prop::bRotateToDesired) || self.store.flag_of(actor, Prop::bFixedRotationDir))
        {
            return Ok(false);
        }
        Ok(!self.is_probing(actor, EProbe::Tick as u32)?)
    }

    /// Runs before the stock tick. Pawns owned by a player's controller, or with `bIgnoreTimeDilation`,
    /// tick in real time (`delta / TimeDilation`). `None`: the controller stays in stasis, no tick.
    fn fps_pawn_tick(&mut self, pawn: ObjectId, delta: f32) -> Result<Option<f32>, Error> {
        if !self.store.is_a(pawn, Kind::FPSPawn) {
            return Ok(Some(delta));
        }
        let now = self.level.map_or(0.0, |level| self.store.float_of(level, Prop::TimeSeconds));
        let rendered = self.store.float_of(pawn, Prop::LastRenderTime);
        if let Some(controller) = self.store.object_of(pawn, Prop::Controller) {
            if self.store.flag_of(controller, Prop::bStasis) {
                if !self.store.flag_of(pawn, Prop::bSliderStasis) && rendered + delta >= now {
                    self.event(controller, "ComeOutOfStasis", [Value::Bool(false)])?;
                }
            } else {
                let wait = self.store.float_of(pawn, Prop::TimeTillStasis);
                if self.store.flag_of(pawn, Prop::bAllowStasis) && wait > 0.0 && now - rendered > wait && !self.store.flag_of(controller, Prop::bPendingStasis) {
                    self.store.set(controller, Prop::bPendingStasis, Value::Bool(true));
                }
            }
            if self.store.flag_of(controller, Prop::bStasis) {
                return Ok(None);
            }
        }
        let owner = self.store.object_of(pawn, Prop::Owner);
        let real_time = self.store.flag_of(pawn, Prop::bIgnoreTimeDilation) || owner.is_some_and(|o| self.store.is_a(o, Kind::PlayerController) || self.is_player(o));
        let dilation = self.level.map_or(1.0, |level| self.store.float_of(level, Prop::TimeDilation));
        Ok(Some(if real_time && dilation > 0.0 { delta / dilation } else { delta }))
    }

    pub fn tick_actors(&mut self, delta: f32) -> Result<(), Error> {
        self.world.frame += 1;
        let actors: Vec<ObjectId> = self.actors().filter(|&o| self.store.is_a(o, Kind::Actor)).collect();
        for actor in actors {
            self.tick_actor(actor, delta)?;
        }
        Ok(())
    }
}
