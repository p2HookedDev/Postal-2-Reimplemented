use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use p2_package::PackageId;

use rustc_hash::FxHashMap;

use crate::{LoaderRef, ObjectStore, Vm};

macro_rules! work {
    ($($(#[$doc:meta])* $name:ident = $text:literal,)*) => {
        #[derive(Debug, Clone, Copy)]
        pub(crate) enum Work { $($(#[$doc])* $name),* }

        const NAMES: &[&str] = &[$($text),*];
    };
}

work! {
    PropertyByName = "property by name",
    PropertyScan = "property scan",
    ClassTestByName = "class test by name",
    ClassTestStep = "class test step",
    Sweep = "sweep",
    SweepCandidate = "sweep candidate",
    SweepBlockingCandidate = "sweep blocking candidate",
    SweepBoundsReject = "sweep bounds reject",
    SweepTested = "sweep tested",
    Touch = "touch",
    TouchCandidate = "touch candidate",
    Statement = "statement",
    ScriptCall = "script call",
    NativeCall = "native call",
    FunctionSearch = "function search",
    Event = "event",
    EventNotProbed = "event not probed",
    EventNoFunction = "event without function",
    HashActor = "hash actor",
    HashEntry = "hash entry",
    VolumeTest = "volume test",
    VolumeBoxReject = "volume box reject",
    FrameAlloc = "frame allocated",
    ArgVector = "argument vector",
    Trace = "trace",
    TraceWorldMicros = "trace world us",
    TraceActorsMicros = "trace actors us",
    TraceCandidate = "trace candidate",
    TraceRagdoll = "trace ragdoll",
    KarmaWait = "karma waits",
    KarmaWaitMicros = "karma wait us",
    LimbFlying = "limbs flying",
    LimbMade = "limb bodies made",
    LimbNoBody = "limbs without body",
    LimbThrown = "limb throws taken",
    LimbMoved = "limbs moved",
    LimbImpact = "limb impacts",
    LimbSettled = "limbs settled",
}

const COUNT: usize = NAMES.len();

#[derive(Debug)]
pub(crate) struct Counters {
    counts: [AtomicU64; COUNT],
    property_names: std::sync::Mutex<FxHashMap<String, u64>>,
}

impl Default for Counters {
    fn default() -> Self {
        Self { counts: std::array::from_fn(|_| AtomicU64::new(0)), property_names: std::sync::Mutex::default() }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FunctionTime {
    pub calls: u64,
    pub total: Duration,
    pub own: Duration,
    pub native: bool,
}

#[derive(Debug, Default)]
pub(crate) struct FunctionProfile {
    times: FxHashMap<(PackageId, usize), FunctionTime>,
    children: Vec<Duration>,
}

impl ObjectStore {
    #[inline]
    pub(crate) fn count(&self, work: Work) {
        if self.count_work {
            self.work.counts[work as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_property(&self, name: &str) {
        if self.count_work {
            self.count(Work::PropertyByName);
            if let Ok(mut names) = self.work.property_names.lock() {
                match names.get_mut(name) {
                    Some(count) => *count += 1,
                    None => {
                        names.insert(name.to_owned(), 1);
                    }
                }
            }
        }
    }
}

impl<L: LoaderRef> Vm<L> {
    pub(crate) fn count(&self, work: Work) {
        if self.diagnostics.count_work {
            self.store.work.counts[work as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_n(&self, work: Work, n: usize) {
        if self.diagnostics.count_work {
            self.store.work.counts[work as usize].fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    pub fn take_work_counts(&mut self) -> Vec<(&'static str, u64)> {
        NAMES.iter().zip(&self.store.work.counts).map(|(&name, count)| (name, count.swap(0, Ordering::Relaxed))).collect()
    }

    pub fn take_property_names(&mut self) -> Vec<(String, u64)> {
        let mut names: Vec<(String, u64)> = self.store.work.property_names.get_mut().map(std::mem::take).unwrap_or_default().into_iter().collect();
        names.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        names
    }

    pub(crate) fn function_enter(&mut self) -> Option<Instant> {
        let profile = self.diagnostics.function_profile.as_mut()?;
        profile.children.push(Duration::ZERO);
        Some(Instant::now())
    }

    pub(crate) fn function_exit(&mut self, function: &crate::interp::Function, started: Option<Instant>) {
        let (Some(started), Some(profile)) = (started, self.diagnostics.function_profile.as_mut()) else { return };
        let total = started.elapsed();
        let children = profile.children.pop().unwrap_or_default();
        if let Some(parent) = profile.children.last_mut() {
            *parent += total;
        }
        let entry = profile.times.entry((function.package, function.export)).or_default();
        entry.native = function.is_native();
        entry.calls += 1;
        entry.total += total;
        entry.own += total.saturating_sub(children);
    }

    pub fn profile_functions(&mut self, on: bool) {
        self.diagnostics.function_profile = on.then(FunctionProfile::default);
    }

    pub fn take_function_profile(&mut self) -> Vec<(String, FunctionTime)> {
        let Some(profile) = self.diagnostics.function_profile.as_mut() else { return Vec::new() };
        let times = std::mem::take(&mut profile.times);
        let mut list: Vec<(String, FunctionTime)> = times
            .into_iter()
            .map(|((package, export), time)| {
                let pkg = &self.loader.package(package).package;
                let entry = &pkg.exports[export];
                let outer = pkg.object_name(entry.package_index).unwrap_or_default();
                (format!("{outer}.{}", pkg.name(entry.object_name)), time)
            })
            .collect();
        list.sort_by_key(|(_, time)| std::cmp::Reverse(time.own));
        list
    }
}
