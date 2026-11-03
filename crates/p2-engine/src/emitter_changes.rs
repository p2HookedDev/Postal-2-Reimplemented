//! Emitter properties scripts change while an emitter lives, passed on to the host's simulation: in the engine the
//! emitter objects are the simulation, but the host simulates from properties read when the emitter appeared.

use p2_particles::{Emitter, EmitterProperties};
use p2_vm::{LoaderRef, ObjectId, ParticleCommand, PropertyBlock, Vm};
use rustc_hash::FxHashMap;

use crate::actors::{emitter_objects, emitter_properties};

#[derive(Default)]
pub struct EmitterValues {
    values: FxHashMap<ObjectId, (u64, PropertyBlock)>,
    calls: u64,
}

impl EmitterValues {
    pub fn changes(&mut self, vm: &Vm<impl LoaderRef>, watched: &[ObjectId]) -> Vec<(ObjectId, EmitterProperties)> {
        self.calls += 1;
        let call = self.calls;
        let mut changed = Vec::new();
        for &actor in watched {
            if vm.is_deleted(actor) {
                continue;
            }
            for emitter in emitter_objects(vm, actor) {
                let values = vm.property_block(emitter);
                match self.values.get_mut(&emitter) {
                    Some((seen, before)) if before == values => *seen = call,
                    Some((seen, before)) => {
                        changed.push((emitter, emitter_properties(vm, emitter)));
                        before.clone_from(values);
                        *seen = call;
                    }
                    None => {
                        changed.push((emitter, emitter_properties(vm, emitter)));
                        self.values.insert(emitter, (call, values.clone()));
                    }
                }
            }
        }
        self.values.retain(|_, (seen, _)| *seen == call);
        changed
    }
}

/// A particle spawned on a line (`SpawnParticleLength`) uses the line as it was then; the frame's last line (from
/// [`EmitterValues::changes`], applied first) stays.
pub fn apply_command(simulation: &mut Emitter, index: usize, command: ParticleCommand<EmitterProperties>) {
    match command {
        ParticleCommand::Reset(_, properties) => simulation.reinitialize(index, properties),
        ParticleCommand::Spawn { offset, line, .. } => {
            let now = simulation.emitters().get(index).map(|e| (e.props().line_start, e.props().line_end));
            match (line, now) {
                (Some([start, end]), Some((now_start, now_end))) => {
                    simulation.set_line(index, start, end);
                    simulation.spawn_one(index, offset);
                    simulation.set_line(index, now_start, now_end);
                }
                _ => simulation.spawn_one(index, offset),
            }
        }
        ParticleCommand::CapTime { time, .. } => simulation.cap_time(index, time),
        ParticleCommand::KeepAlive(_) => simulation.keep_alive(index),
        ParticleCommand::Kill(_) => simulation.kill(),
    }
}
