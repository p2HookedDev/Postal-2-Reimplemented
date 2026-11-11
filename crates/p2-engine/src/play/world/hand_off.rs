use super::*;

impl PlayWorld {
    pub fn take_spawned(&mut self) -> Vec<Spawned> {
        std::mem::take(&mut self.frame.spawned)
    }

    pub fn is_gone(&self, actor: ObjectId) -> bool {
        self.actor(actor).is_some_and(|a| a.gone)
    }

    pub fn watch(&mut self, actor: ObjectId) {
        self.watched.insert(actor);
    }

    pub fn unwatch(&mut self, actor: ObjectId) {
        self.watched.remove(&actor);
    }

    pub fn watched(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.watched.iter().copied()
    }

    pub fn dynamic_lights(&self) -> &[p2_render_cpu::MeshLight] {
        &self.frame.dynamic_lights
    }

    pub fn actor_forces(&self) -> &[ActorForce] {
        &self.frame.forces
    }

    pub fn take_particle_commands(&mut self) -> Vec<ParticleCommand<EmitterProperties>> {
        std::mem::take(&mut self.frame.particles)
    }

    pub fn step(&self) -> f32 {
        self.frame.step
    }

    pub fn take_plings(&mut self) -> Vec<p2_vm::FluidPling> {
        std::mem::take(&mut self.frame.plings)
    }

    pub fn fluid_touchers(&self, surface: ObjectId) -> &[p2_particles::FluidToucher] {
        self.frame.fluid_touchers.iter().find(|(s, _)| *s == surface).map_or(&[][..], |(_, touchers)| touchers.as_slice())
    }

    pub fn corona_blocked(&self, light: ObjectId) -> bool {
        self.frame.corona_blocked.binary_search(&light).is_ok()
    }

    pub fn take_material_switches(&mut self) -> Vec<(String, Option<String>)> {
        std::mem::take(&mut self.frame.material_switches)
    }

    pub fn take_emitter_changes(&mut self) -> Vec<(ObjectId, EmitterProperties)> {
        std::mem::take(&mut self.frame.emitter_changes)
    }

    pub fn screen_overlay(&self) -> Option<Overlay> {
        self.frame.overlay.clone()
    }

    pub fn take_set_mouse(&mut self) -> Option<[f32; 2]> {
        self.frame.set_mouse.take()
    }

    pub fn wants_quit(&self) -> bool {
        self.frame.quit
    }

    pub fn take_zone_fogs(&mut self) -> Option<Vec<Option<crate::actors::ZoneFog>>> {
        self.frame.zone_fogs.take()
    }

    pub fn take_display(&mut self) -> (Option<p2_vm::WindowRequest>, Option<crate::display::Settings>) {
        (self.frame.window.take(), self.frame.settings.take())
    }

    pub fn shows_mouse(&self) -> bool {
        self.frame.shows_mouse
    }

    /// In seconds, once: later calls return 0 until the next frame (egui may run a frame's UI pass twice, which must not
    /// advance what follows the game twice).
    pub fn take_step(&mut self) -> f32 {
        std::mem::take(&mut self.frame.step)
    }

    pub fn take_projections(&mut self) -> Vec<(ProjectorEvent, Option<ProjectorActor>)> {
        std::mem::take(&mut self.frame.projections)
    }

    pub fn take_projector_actors(&mut self) -> Option<FxHashSet<ObjectId>> {
        self.frame.projector_actors.take()
    }

    pub fn take_travel(&mut self) -> Option<String> {
        self.frame.travel.take()
    }

    pub fn presented(&self) -> bool {
        self.presented
    }

    pub fn hud_items(&self) -> Arc<[CanvasItem]> {
        Arc::clone(&self.frame.hud)
    }

    pub fn movies(&self) -> &[(String, ObjectId, p2_vm::MovieState)] {
        &self.frame.movies
    }

    pub fn game_paused(&self) -> bool {
        self.frame.paused
    }

    pub fn hud(&mut self, width: f32, height: f32) -> Vec<CanvasItem> {
        self.sim_mut().map(|sim| sim.hud([width, height])).unwrap_or_default()
    }

    pub fn summary(&self) -> String {
        format!("Playing: {:.0} s", self.seconds)
    }
}
