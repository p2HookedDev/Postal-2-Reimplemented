use std::time::{Duration, Instant};

use bevy::ecs::prelude::{ResMut, Resource};
use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VmProfile {
    #[default]
    Off,
    TickByClass,
    Work,
}

#[derive(Resource, Default)]
pub struct FrameTimes {
    pub totals: std::collections::BTreeMap<&'static str, Duration>,
    pub vm_totals: std::collections::BTreeMap<&'static str, Duration>,
    pub vm_profile: VmProfile,
    pub vm_parts: FxHashMap<String, Duration>,
    /// Game ticks presented: fewer than the frames drawn while a tick takes longer than a frame.
    pub vm_frames: u32,
    pub(super) current: FrameParts,
    pub(super) slowest: Option<FrameParts>,
}

#[derive(Default, Clone, Debug)]
pub struct FrameParts {
    pub(super) started: Option<Instant>,
    pub length: Duration,
    pub main: Vec<(&'static str, Duration)>,
    pub vm: Vec<(&'static str, Duration)>,
    pub profile: Vec<(String, Duration)>,
}

pub(super) fn add_part(parts: &mut Vec<(&'static str, Duration)>, name: &'static str, time: Duration) {
    match parts.iter_mut().find(|(n, _)| *n == name) {
        Some((_, total)) => *total += time,
        None => parts.push((name, time)),
    }
}

impl FrameTimes {
    pub fn add(&mut self, name: &'static str, start: Instant) {
        let time = start.elapsed();
        *self.totals.entry(name).or_default() += time;
        add_part(&mut self.current.main, name, time);
    }

    pub fn add_vm(&mut self, parts: &mut Vec<(&'static str, Duration)>, profile: &mut FxHashMap<String, Duration>) {
        self.vm_frames += 1;
        for (name, time) in parts.drain(..) {
            *self.vm_totals.entry(name).or_default() += time;
            add_part(&mut self.current.vm, name, time);
        }
        let mut sorted: Vec<(String, Duration)> = profile.drain().collect();
        for (part, time) in &sorted {
            *self.vm_parts.entry(part.clone()).or_default() += *time;
        }
        sorted.sort_by(|(a, a_time), (b, b_time)| b_time.cmp(a_time).then_with(|| a.cmp(b)));
        self.current.profile = sorted;
    }

    pub fn end_frame(&mut self) {
        let now = Instant::now();
        let started = self.current.started.replace(now);
        let length = started.map(|started| now - started);
        if let Some(length) = length
            && self.slowest.as_ref().is_none_or(|slowest| length > slowest.length)
        {
            let done = std::mem::replace(&mut self.current, FrameParts { started: Some(now), ..Default::default() });
            self.slowest = Some(FrameParts { started, length, ..done });
        } else {
            self.current.main.clear();
            self.current.vm.clear();
            self.current.profile.clear();
        }
    }

    pub fn take_slowest(&mut self) -> Option<FrameParts> {
        self.slowest.take()
    }
}

pub fn end_frame(mut times: ResMut<FrameTimes>) {
    times.end_frame();
}

/// A changed asset is a queued write, which `finish encoders and submit` pays for.
/// What rewrote a mesh, for the profile log: each rewrite is uploaded again.
#[derive(Clone, Copy)]
pub enum MeshWrite {
    Strip,
    Beam,
    SpriteGrid,
    Fluid,
    Corona,
    ActorLight,
    TerrainLight,
}

pub const MESH_WRITERS: [&str; 7] = ["strips", "beams", "sprite grids", "fluids", "coronas", "actor vertex light", "terrain light"];
pub static MESH_WRITES: [std::sync::atomic::AtomicU32; 7] = [const { std::sync::atomic::AtomicU32::new(0) }; 7];

pub fn mesh_written(by: MeshWrite) {
    MESH_WRITES[by as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
