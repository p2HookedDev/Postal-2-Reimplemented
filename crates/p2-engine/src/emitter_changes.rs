//! Emitter properties scripts change while an emitter lives, passed on to the host's simulation: in the engine the
//! emitter objects are the simulation, but the host simulates from properties read when the emitter appeared.

use p2_particles::{Emitter, EmitterProperties};
use p2_vm::{LoaderRef, ObjectId, ParticleCommand, PropertyBlock, Vm};
use rustc_hash::FxHashMap;

use crate::actors::{emitter_objects, emitter_properties};

#[derive(Default)]
pub struct EmitterValues {
    values: FxHashMap<ObjectId, Seen>,
    calls: u64,
}

struct Seen {
    call: u64,
    /// The reads' mark when the values were compared: an emitter not written since is not compared again.
    read_at: u64,
    values: PropertyBlock,
}

impl EmitterValues {
    /// `mark`: the store's stamp as this pass of reads began, the stamp itself raised since.
    pub fn changes(&mut self, vm: &Vm<impl LoaderRef>, watched: &[ObjectId], mark: u64) -> Vec<(ObjectId, EmitterProperties)> {
        self.calls += 1;
        let call = self.calls;
        let mut changed = Vec::new();
        for &actor in watched {
            if vm.store.is_deleted(actor) {
                continue;
            }
            for emitter in emitter_objects(vm, actor) {
                let values = vm.property_block(emitter);
                match self.values.get_mut(&emitter) {
                    Some(seen) => {
                        seen.call = call;
                        if vm.store.object(emitter).written() <= seen.read_at {
                            continue;
                        }
                        seen.read_at = mark;
                        if seen.values != *values {
                            changed.push((emitter, emitter_properties(vm, emitter)));
                            seen.values.clone_from(values);
                        }
                    }
                    None => {
                        changed.push((emitter, emitter_properties(vm, emitter)));
                        self.values.insert(emitter, Seen { call, read_at: mark, values: values.clone() });
                    }
                }
            }
        }
        self.values.retain(|_, seen| seen.call == call);
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
