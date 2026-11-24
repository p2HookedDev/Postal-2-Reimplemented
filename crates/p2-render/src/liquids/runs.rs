//! Liquid on walls: a run is a trail that trickles down a wall from where a stream or a drop hit it, or from a lip a
//! pool spills over. It leaves its liquid behind as it goes, thins, and stops as a bead; one that reaches a floor pours
//! what it has left there. Unreal units and axes; the head is the last point.

use p2_math::Vector;
use p2_vm::{FluidType, ObjectId};

use crate::level_collision::StaticCollision;

pub const RUN_POINTS: usize = 16;
/// Unreal units a second a full run falls, and how far apart it lays its points (twice as far each time it fills).
const RUN_SPEED: f32 = 40.0;
const RUN_SPACING: f32 = 3.0;
/// A full run's trail radius, and what of its liquid it leaves behind per Unreal unit at that width.
const RUN_RADIUS: f32 = 0.9;
const RUN_LOSS: f32 = 0.5;
/// Below this share of the most it held, a run is too thin to move: it stops as a bead. It slows as it thins and
/// stops once it is down to the least pace.
const RUN_STOP: f32 = 0.12;
const LEAST_PACE: f32 = 0.05;
/// Seconds a run nothing feeds takes to dry after it stops.
const RUN_DRYING: f32 = 10.0;
/// A wall is a surface whose normal leans this far from straight up or down at most.
pub const STEEP: f32 = 0.5;
/// The volume a second a stream's hit on a wall feeds its run with, and how near its last start a hit feeds it.
const STREAM_FEED: f32 = 12.0;
const STREAM_REACH: f32 = 16.0;
/// A splat's head against the radius of a sphere of its volume.
const SPLAT_SPREAD: f32 = 2.5;

/// The level's surfaces as a run meets them: the first hit between two points, its place and normal.
pub trait Surfaces {
    fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)>;
}

impl Surfaces for StaticCollision {
    fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)> {
        self.first_hit(from, to).filter(|hit| hit.time > 0.0).map(|hit| (hit.location, hit.normal))
    }
}

/// What a run comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSource {
    /// A feeder's stream.
    Stream(ObjectId),
    Splat,
    /// A pool spilling over a lip.
    Lip,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub kind: FluidType,
    pub source: RunSource,
    /// Place and radius of each point laid; the last is the head.
    pub points: Vec<(Vector, f32)>,
    /// The wall's normal where the head is.
    pub normal: Vector,
    pub volume: f32,
    pub most: f32,
    pub travelled: f32,
    /// The splat's radius at the run's start.
    pub splat: f32,
    pub running: bool,
    /// 1 while fresh, to 0 as it dries.
    pub wet: f32,
    pub last_fed: f32,
    seed: u32,
    since_point: f32,
}

/// Where liquid meets a wall: the place, the wall's normal there and the kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallHit {
    pub at: Vector,
    pub normal: Vector,
    pub kind: FluidType,
}

/// A floor a run reached, and what it pours there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pour {
    pub at: Vector,
    pub kind: FluidType,
    pub volume: f32,
}

fn share(run: &Run) -> f32 {
    run.volume / run.most.max(1e-6)
}

/// Radians off straight down after `travelled` units: a lean of the run's own plus value noise, a knot every 8 units.
fn meander(seed: u32, travelled: f32) -> f32 {
    let hash = |k: i32| {
        let mut h = seed ^ (k as u32).wrapping_mul(0x9E37_79B1);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2C1B_3C6D);
        h ^= h >> 12;
        h = h.wrapping_mul(0x297A_2D39);
        h ^= h >> 15;
        (h & 0xFFFF) as f32 / 32767.5 - 1.0
    };
    let x = travelled / 8.0;
    let (k, t) = (x.floor() as i32, x - x.floor());
    let s = t * t * (3.0 - 2.0 * t);
    hash(-1) * 0.12 + (hash(k) + (hash(k + 1) - hash(k)) * s) * 0.42
}

/// The way down a wall facing `normal`, turned by `angle` radians toward its side.
fn downhill(normal: Vector, angle: f32) -> Vector {
    let down = Vector::new(0.0, 0.0, -1.0);
    let down = (down - normal * down.dot(normal)).safe_normal();
    let side = normal.cross(down);
    down * angle.cos() + side * angle.sin()
}

fn width(share: f32) -> f32 {
    RUN_RADIUS * (0.35 + 0.65 * share.clamp(0.0, 1.0)).sqrt()
}

#[derive(Default)]
pub struct Runs {
    pub list: Vec<Run>,
    seed: u32,
}

impl Runs {
    fn start(&mut self, WallHit { at, normal, kind }: WallHit, source: RunSource, volume: f32, now: f32, cap: usize) {
        if self.list.len() >= cap {
            let Some(oldest) = self.list.iter().enumerate().filter(|(_, run)| !run.running).min_by(|a, b| a.1.wet.total_cmp(&b.1.wet)).map(|(i, _)| i) else { return };
            self.list.swap_remove(oldest);
        }
        self.seed = self.seed.wrapping_mul(0x9E37_79B9).wrapping_add(0x7F4A_7C15);
        let splat = (volume * 3.0 / (4.0 * std::f32::consts::PI)).cbrt() * SPLAT_SPREAD;
        let at = at + normal * 0.05;
        self.list.push(Run {
            kind,
            source,
            points: vec![(at, splat.max(width(1.0)))],
            normal,
            volume,
            most: volume,
            travelled: 0.0,
            splat,
            running: true,
            wet: 1.0,
            last_fed: now,
            seed: self.seed,
            since_point: 0.0,
        });
    }

    /// A stream hit a wall: feeds the run its feeder started near there, or starts one.
    /// Feeds the run `source` started near `hit` with `volume`, or starts one.
    fn feed(&mut self, hit: WallHit, source: RunSource, volume: f32, now: f32, cap: usize) {
        let fed = self.list.iter_mut().find(|run| run.source == source && (run.points[0].0 - hit.at).size() < STREAM_REACH);
        match fed {
            Some(run) => {
                run.volume += volume;
                run.most = run.most.max(run.volume);
                (run.last_fed, run.wet) = (now, 1.0);
                run.running |= share(run) > RUN_STOP;
            }
            None => self.start(hit, source, volume, now, cap),
        }
    }

    /// A feeder's stream hit a wall for `seconds`.
    pub fn stream(&mut self, feeder: ObjectId, hit: WallHit, seconds: f32, now: f32, cap: usize) {
        self.feed(hit, RunSource::Stream(feeder), STREAM_FEED * seconds, now, cap);
    }

    /// A drop or a splash landed on a wall: a splat that may run.
    pub fn splat(&mut self, hit: WallHit, volume: f32, now: f32, cap: usize) {
        self.start(hit, RunSource::Splat, volume, now, cap);
    }

    /// A pool spills `volume` over a lip, down the face under it.
    pub fn spill(&mut self, hit: WallHit, volume: f32, now: f32, cap: usize) {
        self.feed(hit, RunSource::Lip, volume, now, cap);
    }

    /// Moves every running head down its wall by `seconds`, and dries the stopped ones; what reaches a floor pours.
    pub fn step(&mut self, seconds: f32, surfaces: &impl Surfaces, now: f32, pours: &mut Vec<Pour>) {
        for run in &mut self.list {
            if !run.running {
                if now - run.last_fed > 0.5 {
                    run.wet -= seconds / RUN_DRYING;
                }
                continue;
            }
            let share = share(run);
            let pace = ((share - RUN_STOP) / (1.0 - RUN_STOP)).clamp(0.0, 1.0);
            let length = RUN_SPEED * pace * seconds;
            if pace <= LEAST_PACE || length <= 0.0 {
                run.running = false;
                continue;
            }
            let (head, _) = run.points[run.points.len() - 1];
            let next = head + downhill(run.normal, meander(run.seed, run.travelled)) * length;
            let (mut at, mut stopped) = (next, false);
            if let Some((place, normal)) = surfaces.hit(head, next) {
                if normal.z > STEEP {
                    // A lip's liquid already lies on the floor below in the pools.
                    if run.source != RunSource::Lip {
                        pours.push(Pour { at: place, kind: run.kind, volume: run.volume });
                    }
                    (at, stopped, run.volume) = (place + normal * 0.05, true, 0.0);
                } else {
                    at = place + normal * 0.05;
                    run.normal = normal;
                }
            } else {
                match surfaces.hit(next + run.normal * 1.5, next - run.normal * 4.0) {
                    Some((place, normal)) if normal.z.abs() < STEEP => {
                        at = place + normal * 0.05;
                        run.normal = normal;
                    }
                    _ => stopped = true,
                }
            }
            let moved = (at - head).size();
            run.travelled += moved;
            run.since_point += moved;
            run.volume = (run.volume - RUN_LOSS * moved * (width(share) / RUN_RADIUS).powi(2)).max(0.0);
            let radius = width(share);
            if run.since_point >= RUN_SPACING * (1 + run.points.len() / RUN_POINTS) as f32 || run.points.len() == 1 {
                if run.points.len() == RUN_POINTS {
                    let kept: Vec<(Vector, f32)> = run.points.iter().step_by(2).copied().collect();
                    run.points = kept;
                }
                run.points.push((at, radius));
                run.since_point = 0.0;
            } else if run.points.len() > 1
                && let Some(last) = run.points.last_mut()
            {
                *last = (at, radius);
            }
            if stopped || share <= RUN_STOP {
                run.running = false;
            }
        }
        self.list.retain(|run| run.wet > 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wall at y = 0 facing -y, over a floor at z = 0.
    struct Corner;

    impl Surfaces for Corner {
        fn hit(&self, from: Vector, to: Vector) -> Option<(Vector, Vector)> {
            if from.z > 0.0 && to.z <= 0.0 {
                let t = from.z / (from.z - to.z);
                return Some((from + (to - from) * t, Vector::new(0.0, 0.0, 1.0)));
            }
            (from.y < 0.0 && to.y >= 0.0).then(|| {
                let t = -from.y / (to.y - from.y);
                (from + (to - from) * t, Vector::new(0.0, -1.0, 0.0))
            })
        }
    }

    fn running(runs: &mut Runs, seconds: f32, steps: usize) -> Vec<Pour> {
        let mut pours = Vec::new();
        for _ in 0..steps {
            runs.step(seconds, &Corner, 0.0, &mut pours);
        }
        pours
    }

    #[test]
    fn a_splat_runs_down_its_wall_thins_and_stops_as_a_bead() {
        let mut runs = Runs::default();
        runs.splat(WallHit { at: Vector::new(0.0, 0.0, 100.0), normal: Vector::new(0.0, -1.0, 0.0), kind: FluidType::Blood }, 10.0, 0.0, 8);
        running(&mut runs, 1.0 / 30.0, 300);
        let run = &runs.list[0];
        assert!(!run.running, "it stopped");
        let head = run.points[run.points.len() - 1].0;
        assert!(head.z < 95.0 && head.z > 0.0, "it ran down and stopped above the floor: {head:?}");
        assert!(head.y.abs() < 0.2, "on the wall: {head:?}");
        assert!(run.points.len() > 2 && run.points.len() <= RUN_POINTS);
        assert!(run.points.windows(2).all(|pair| pair[1].0.z <= pair[0].0.z + 0.01), "downhill all the way");
    }

    #[test]
    fn a_fed_stream_runs_to_the_floor_and_pours_there() {
        let mut runs = Runs::default();
        let mut pours = Vec::new();
        for _ in 0..600 {
            runs.stream(ObjectId::from_index(7), WallHit { at: Vector::new(0.0, 0.0, 40.0), normal: Vector::new(0.0, -1.0, 0.0), kind: FluidType::Urine }, 1.0 / 30.0, 0.0, 8);
            runs.step(1.0 / 30.0, &Corner, 0.0, &mut pours);
        }
        assert_eq!(runs.list.len(), 1, "one feeder, one run");
        assert!(pours.first().is_some_and(|pour| pour.at.z.abs() < 0.01 && pour.volume > 0.0), "{pours:?}");
    }

    #[test]
    fn a_dry_run_is_gone_and_the_cap_holds() {
        let mut runs = Runs::default();
        for k in 0..4 {
            runs.splat(WallHit { at: Vector::new(k as f32 * 10.0, 0.0, 50.0), normal: Vector::new(0.0, -1.0, 0.0), kind: FluidType::Blood }, 0.5, 0.0, 3);
        }
        assert_eq!(runs.list.len(), 3);
        let mut pours = Vec::new();
        for _ in 0..400 {
            runs.step(0.05, &Corner, 10.0, &mut pours);
        }
        assert!(runs.list.is_empty());
        assert!((meander(5, 3.0) - meander(5, 3.0)).abs() < 1e-6 && meander(5, 3.0).abs() < 0.6);
    }
}
