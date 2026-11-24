use bevy::math::Vec3;
use p2_math::Vector;
use p2_particles::{CollisionHit, SpriteParticle, StripPoint};

pub(super) const PIECES: usize = 4;
const SAG: f32 = 0.06;
const FLOW_RATE: f32 = 1.5;
const GROW_SECONDS: f32 = 0.6;
const GROW_START: f32 = 0.25;
const LEAN_PER_SLOPE: f32 = 1.5;
const MAX_LEAN: f32 = 0.5;
const TRACE_UP: f32 = 16.0;
const TRACE_DOWN: f32 = 48.0;
/// Seeds stay small: the shader reads one as a float.
const SEEDS: u32 = 1023;

/// What modern liquids keep per emitter: its age in game seconds while the switch is on, and for a puddle its edge's
/// seed and the slope found under it.
#[derive(Default)]
pub(super) struct Liquid {
    pub(super) seconds: f32,
    puddle: Option<Puddle>,
}

struct Puddle {
    seed: u32,
    downhill: Option<(Vec3, f32)>,
}

impl Liquid {
    pub(super) fn new(puddle_seed: Option<u32>) -> Self {
        Self { seconds: 0.0, puddle: puddle_seed.map(|seed| Puddle { seed: seed % SEEDS + 1, downhill: None }) }
    }

    /// One frame with the switch on: the clock runs on game time and a puddle's sprites take their drawn shape.
    pub(super) fn advance(&mut self, flow: bool, game_step: f32, drawn: &mut [(Vec<StripPoint>, Vec<SpriteParticle>)], ground: impl Fn(Vector, Vector) -> Option<CollisionHit>) {
        if !flow {
            return;
        }
        self.seconds += game_step;
        for (_, sprites) in drawn {
            self.shape_puddle(sprites, &ground);
        }
    }

    pub(super) fn seed(&self) -> Option<u32> {
        self.puddle.as_ref().map(|puddle| puddle.seed)
    }

    /// Only the drawn copies change: the script's size, place and life stay as they are.
    pub(super) fn shape_puddle(&mut self, sprites: &mut [SpriteParticle], ground: impl FnOnce(Vector, Vector) -> Option<CollisionHit>) {
        let (Some(puddle), Some(first)) = (&mut self.puddle, sprites.first()) else { return };
        let at = first.location;
        let (direction, lean) = *puddle.downhill.get_or_insert_with(|| {
            ground(at + Vector::new(0.0, 0.0, TRACE_UP), at - Vector::new(0.0, 0.0, TRACE_DOWN)).map_or((Vec3::ZERO, 0.0), |hit| downhill(Vec3::from_array(hit.normal.into())))
        });
        let grow = growth(self.seconds);
        for sprite in sprites {
            *sprite = shaped(sprite, grow, direction, lean);
        }
    }
}

fn growth(seconds: f32) -> f32 {
    let t = (seconds / GROW_SECONDS).clamp(0.0, 1.0);
    GROW_START + (1.0 - GROW_START) * (1.0 - (1.0 - t).powi(3))
}

/// The level direction down the slope of a ground with this normal (Unreal Z up), and how far a puddle leans along it.
fn downhill(normal: Vec3) -> (Vec3, f32) {
    if normal.z <= 0.0 {
        return (Vec3::ZERO, 0.0);
    }
    let level = Vec3::new(normal.x, normal.y, 0.0);
    (level.normalize_or_zero(), (level.length() / normal.z * LEAN_PER_SLOPE).min(MAX_LEAN))
}

/// Grown about its centre, then stretched downhill by `lean` with its uphill edge kept.
fn shaped(sprite: &SpriteParticle, grow: f32, direction: Vec3, lean: f32) -> SpriteParticle {
    let vec = |v: Vector| Vec3::from_array(v.into());
    let (up, right) = (vec(sprite.up), vec(sprite.right));
    let stretch = |axis: Vec3| Vector::from(((axis + direction * axis.dot(direction) * lean) * grow).to_array());
    let reach = up.dot(direction).abs() + right.dot(direction).abs();
    let location = Vector::from((vec(sprite.location) + direction * reach * lean * grow).to_array());
    SpriteParticle { location, up: stretch(up), right: stretch(right), ..*sprite }
}

/// The texture's scroll along a stream, in segments.
pub(super) fn phase(seconds: f32) -> f32 {
    (seconds * FLOW_RATE).fract()
}

/// A Catmull-Rom curve through the script's points, `PIECES` points per segment, passing through every script point.
/// A sagging ribbon drops between its points by a share of the segment's level run (Unreal Z is up).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Hang {
    Sags,
    Straight,
}

pub(super) fn ribbon(points: &[StripPoint], hang: Hang) -> Vec<StripPoint> {
    let last = points.len().saturating_sub(1);
    let at = |i: usize| Vec3::from_array(points[i.min(last)].location.into());
    let mut out = Vec::with_capacity(last * PIECES + 1);
    for i in 0..last {
        let (p0, p1, p2, p3) = (at(i.saturating_sub(1)), at(i), at(i + 1), at(i + 2));
        let run = (p2 - p1).truncate().length();
        let (from, to) = (points[i], points[i + 1]);
        for k in 0..PIECES {
            let t = k as f32 / PIECES as f32;
            let curve = 0.5 * (2.0 * p1 + (p2 - p0) * t + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t + (3.0 * p1 - p0 - 3.0 * p2 + p3) * t * t * t);
            let drop = if hang == Hang::Sags { SAG * run * 4.0 * t * (1.0 - t) } else { 0.0 };
            out.push(StripPoint { location: Vector::from((curve - Vec3::Z * drop).to_array()), size: from.size + (to.size - from.size) * t, color: from.color });
        }
    }
    out.extend(points.get(last).copied());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, z: f32) -> StripPoint {
        StripPoint { location: [x, 0.0, z].into(), size: 2.0, color: [255; 4] }
    }

    fn at(p: &StripPoint) -> Vec3 {
        Vec3::from_array(p.location.into())
    }

    #[test]
    fn ribbon_passes_through_the_script_points() {
        let points = [point(0.0, 0.0), point(10.0, -5.0), point(20.0, -20.0), point(25.0, -40.0)];
        let smooth = ribbon(&points, Hang::Sags);
        assert_eq!(smooth.len(), 3 * PIECES + 1);
        for (i, p) in points.iter().enumerate() {
            assert!(at(&smooth[i * PIECES]).abs_diff_eq(at(p), 1e-4), "{i}");
        }
    }

    #[test]
    fn a_level_stream_sags_and_a_falling_one_does_not() {
        let level = ribbon(&[point(0.0, 0.0), point(10.0, 0.0), point(20.0, 0.0)], Hang::Sags);
        let middle = at(&level[PIECES / 2]);
        assert!(middle.z < -0.5 && middle.z > -1.0, "{middle}");
        assert!(ribbon(&[point(0.0, 0.0), point(10.0, 0.0), point(20.0, 0.0)], Hang::Straight).iter().all(|p| at(p).z == 0.0));
        let falling = ribbon(&[point(0.0, 0.0), point(0.0, -10.0), point(0.0, -20.0)], Hang::Sags);
        assert!(falling.iter().all(|p| at(p).x.abs() < 1e-4));
    }

    #[test]
    fn a_puddle_grows_to_its_script_size_and_stays() {
        assert_eq!(growth(0.0), GROW_START);
        assert!(growth(GROW_SECONDS * 0.5) > growth(GROW_SECONDS * 0.25));
        assert_eq!(growth(GROW_SECONDS), 1.0);
        assert_eq!(growth(100.0), 1.0);
    }

    #[test]
    fn the_lean_follows_the_slope_and_is_clamped() {
        assert_eq!(downhill(Vec3::Z), (Vec3::ZERO, 0.0));
        assert_eq!(downhill(-Vec3::Z), (Vec3::ZERO, 0.0));
        let (direction, gentle) = downhill(Vec3::new(0.1, 0.0, 1.0).normalize());
        assert!(direction.abs_diff_eq(Vec3::X, 1e-5) && (gentle - 0.15).abs() < 1e-4, "{direction} {gentle}");
        assert_eq!(downhill(Vec3::new(0.0, -1.0, 1.0).normalize()).1, MAX_LEAN);
    }

    #[test]
    fn a_leaning_puddle_keeps_its_uphill_edge() {
        let sprite = SpriteParticle {
            location: [100.0, 0.0, 0.0].into(),
            previous: [100.0, 0.0, 0.0].into(),
            size: [10.0; 2],
            up: [10.0, 0.0, 0.0].into(),
            right: [0.0, 10.0, 0.0].into(),
            color: [255; 4],
            uv: [0.0, 0.0, 1.0, 1.0],
            blend: None,
        };
        let leaned = shaped(&sprite, 1.0, Vec3::X, 0.5);
        let (at, up) = (Vec3::from_array(leaned.location.into()), Vec3::from_array(leaned.up.into()));
        assert!((at.x - up.x - 90.0).abs() < 1e-4 && (at.x + up.x - 120.0).abs() < 1e-4, "{at} {up}");
        assert_eq!(Vec3::from_array(leaned.right.into()), Vec3::new(0.0, 10.0, 0.0));
        let mut liquid = Liquid::new(Some(7));
        let mut sprites = [sprite];
        liquid.seconds = GROW_SECONDS;
        liquid.shape_puddle(&mut sprites, |_, _| None);
        assert_eq!(Vec3::from_array(sprites[0].up.into()), Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(liquid.seed(), Some(8));
        assert_eq!(Liquid::new(None).seed(), None);
    }

    #[test]
    fn phase_stays_within_one_segment() {
        for seconds in [0.0, 0.3, 1.0, 17.9, 1000.25] {
            assert!((0.0..1.0).contains(&phase(seconds)), "{seconds}");
        }
    }
}
