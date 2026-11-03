use p2_collision::Hit;
use p2_math::Vector;

use crate::{LoaderRef, Vm};

pub(crate) trait WorldQuery {
    fn encroached(&self, location: Vector, extent: Vector) -> bool;
    fn trace(&self, start: Vector, end: Vector, extent: Vector) -> Option<Hit>;
}

impl<L: LoaderRef> WorldQuery for Vm<L> {
    fn encroached(&self, location: Vector, extent: Vector) -> bool {
        self.world_encroached(location, extent)
    }

    fn trace(&self, start: Vector, end: Vector, extent: Vector) -> Option<Hit> {
        self.trace_world(start, end, extent)
    }
}

/// `extent` is a half size; `location` moves to where the box fits.
pub(crate) fn find_spot(world: &impl WorldQuery, extent: Vector, location: &mut Vector) -> bool {
    if !world.encroached(*location, extent) {
        return true;
    }
    if extent == Vector::ZERO {
        return false;
    }
    let start = *location;
    let mut slice_blocked = false;
    if check_slice(world, extent, location, &mut slice_blocked) {
        return true;
    }
    if !slice_blocked {
        return false;
    }
    // Quarter boxes (0.5 Ex, 0.5 Ey, 1) at 0.55 of the extent from the start; each clear one adds 1.1x its offset.
    let quarter = Vector::new(0.5 * extent.x, 0.5 * extent.y, 1.0);
    let mut clear = 0;
    for i in [-1.0, 1.0] {
        for j in [-1.0, 1.0] {
            if clear >= 2 {
                break;
            }
            let offset = Vector::new(0.55 * extent.x * i, 0.55 * extent.y * j, 0.0);
            if !world.encroached(start + offset, quarter) {
                clear += 1;
                *location += offset * 1.1;
            }
        }
    }
    match clear {
        0 => return false,
        1 => *location = *location * 2.0 - start,
        _ => {}
    }
    if world.encroached(*location, extent) {
        return false;
    }
    let back = start + (start - *location) * 0.2;
    if let Some(hit) = world.trace(*location, back, extent) {
        *location = hit.location;
    }
    true
}

pub(crate) fn check_slice(world: &impl WorldQuery, extent: Vector, location: &mut Vector, slice_blocked: &mut bool) -> bool {
    let slice = Vector::new(extent.x, extent.y, 1.0);
    if world.encroached(*location, slice) {
        *slice_blocked = true;
        return false;
    }
    let down = *location - Vector::new(0.0, 0.0, 2.0 * extent.z);
    let hit = world.trace(*location, down, slice);
    let normal = hit.map_or(Vector::ZERO, |hit| Vector::new(hit.normal.x, hit.normal.y, 0.0));
    let lowered = match hit {
        None => {
            location.z -= extent.z;
            true
        }
        Some(hit) if hit.time <= 0.5 => {
            location.z += 2.0 * extent.z * (0.5 - hit.time) + 1.0;
            if !world.encroached(*location, extent) {
                return true;
            }
            false
        }
        Some(hit) => {
            location.z -= extent.z * (2.0 * hit.time - 1.0);
            location.z += 1.0;
            true
        }
    };
    if lowered && !world.encroached(*location, extent) {
        let up = *location + Vector::new(0.0, 0.0, extent.z);
        match world.trace(*location, up, extent) {
            Some(hit) => *location = hit.location,
            None => location.z += extent.z,
        }
        return true;
    }
    location.x += normal.x * extent.x;
    location.y += normal.y * extent.x;
    !world.encroached(*location, extent)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        floor: f32,
        wall: [f32; 2],
    }

    impl WorldQuery for Fake {
        fn encroached(&self, l: Vector, e: Vector) -> bool {
            l.z - e.z < self.floor || l.x + e.x > self.wall[0] || l.y + e.y > self.wall[1]
        }

        fn trace(&self, s: Vector, end: Vector, e: Vector) -> Option<Hit> {
            let (a, b) = (s.z - e.z - self.floor, end.z - e.z - self.floor);
            (a >= 0.0 && b < 0.0).then(|| {
                let time = a / (a - b);
                Hit { time, location: s.lerp(end, time), normal: Vector::new(0.0, 0.0, 1.0) }
            })
        }
    }

    const OPEN: [f32; 2] = [f32::INFINITY, f32::INFINITY];
    const EXTENT: Vector = Vector::new(10.0, 10.0, 25.0);

    fn near(a: Vector, b: Vector) -> bool {
        (a - b).size() < 1e-3
    }

    #[test]
    fn a_free_spot_is_kept() {
        let mut at = Vector::new(0.0, 0.0, 100.0);
        assert!(find_spot(&Fake { floor: 0.0, wall: OPEN }, EXTENT, &mut at));
        assert_eq!(at, Vector::new(0.0, 0.0, 100.0));
    }

    #[test]
    fn a_zero_extent_in_solid_fails() {
        let mut at = Vector::new(0.0, 0.0, -5.0);
        assert!(!find_spot(&Fake { floor: 0.0, wall: OPEN }, Vector::ZERO, &mut at));
    }

    #[test]
    fn a_box_sunk_in_the_floor_is_lifted_by_the_slice() {
        let mut at = Vector::new(0.0, 0.0, 20.0);
        assert!(find_spot(&Fake { floor: 0.0, wall: OPEN }, EXTENT, &mut at));
        assert!(near(at, Vector::new(0.0, 0.0, 27.0)), "{at:?}");
    }

    #[test]
    fn two_clear_corners_shift_away_from_a_wall() {
        let mut at = Vector::new(0.0, 0.0, 40.0);
        assert!(find_spot(&Fake { floor: 0.0, wall: [5.0, f32::INFINITY] }, EXTENT, &mut at));
        assert!(near(at, Vector::new(-12.1, 0.0, 40.0)), "{at:?}");
    }

    #[test]
    fn one_clear_corner_doubles_its_shift() {
        let mut at = Vector::new(0.0, 0.0, 40.0);
        assert!(find_spot(&Fake { floor: 0.0, wall: [5.0, 5.0] }, EXTENT, &mut at));
        assert!(near(at, Vector::new(-12.1, -12.1, 40.0)), "{at:?}");
    }

    #[test]
    fn no_clear_corner_fails() {
        let mut at = Vector::new(0.0, 0.0, 0.0);
        assert!(!find_spot(&Fake { floor: 100.0, wall: OPEN }, EXTENT, &mut at));
    }

    #[test]
    fn the_lowering_slice_paths_raise_by_the_trace_up() {
        let mut at = Vector::new(0.0, 0.0, 60.0);
        let mut blocked = false;
        assert!(check_slice(&Fake { floor: 0.0, wall: OPEN }, EXTENT, &mut at, &mut blocked));
        assert!(!blocked);
        assert!(near(at, Vector::new(0.0, 0.0, 60.0)), "{at:?}");
        let mut at = Vector::new(0.0, 0.0, 40.0);
        assert!(check_slice(&Fake { floor: 0.0, wall: OPEN }, EXTENT, &mut at, &mut blocked));
        assert!(near(at, Vector::new(0.0, 0.0, 52.0)), "{at:?}");
    }
}
