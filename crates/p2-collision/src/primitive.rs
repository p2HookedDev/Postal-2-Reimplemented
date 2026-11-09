use p2_math::Vector;

use crate::Hit;

pub fn pull_back(start: Vector, end: Vector, hit: Hit, units: f32) -> Hit {
    let length = (end - start).size();
    let time = if length > 1e-6 { (hit.time - units / length).clamp(0.0, 1.0) } else { hit.time };
    Hit { time, location: start.lerp(end, time), ..hit }
}

/// How far (units) a primitive pulls back the hit of a box check of `length`: 0.1 of it,
/// at least 0.1 and at most 4 for a model or 1 for a static mesh.
pub fn pull_back_units(primitive: Primitive, length: f32) -> f32 {
    (0.1 * length).clamp(0.1, if primitive == Primitive::Brush { 4.0 } else { 1.0 })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    Brush,
    StaticMesh,
}

pub fn segment_cylinder(start: Vector, end: Vector, center: Vector, radius: f32, height: f32) -> Option<Hit> {
    let (d, s) = (end - start, start - center);
    let a = d[0] * d[0] + d[1] * d[1];
    let b = 2.0 * (s[0] * d[0] + s[1] * d[1]);
    let c = s[0] * s[0] + s[1] * s[1] - radius * radius;
    let (circle_in, circle_out) = if a < 1e-6 {
        if c > 0.0 {
            return None;
        }
        (f32::NEG_INFINITY, f32::INFINITY)
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            return None;
        }
        let root = disc.sqrt();
        ((-b - root) / (2.0 * a), (-b + root) / (2.0 * a))
    };
    let (slab_in, slab_out) = if d[2].abs() < 1e-6 {
        if s[2].abs() > height {
            return None;
        }
        (f32::NEG_INFINITY, f32::INFINITY)
    } else {
        let (t0, t1) = ((-height - s[2]) / d[2], (height - s[2]) / d[2]);
        (t0.min(t1), t0.max(t1))
    };
    let enter = circle_in.max(slab_in);
    let exit = circle_out.min(slab_out);
    if enter > exit || exit < 0.0 || enter > 1.0 {
        return None;
    }
    let time = enter.max(0.0);
    let location = start + d * time;
    let normal = if slab_in >= circle_in {
        Vector::new(0.0, 0.0, if d[2] > 0.0 { -1.0 } else { 1.0 })
    } else {
        let (x, y) = (location[0] - center[0], location[1] - center[1]);
        let length = x.hypot(y).max(1e-6);
        Vector::new(x / length, y / length, 0.0)
    };
    Some(Hit { time, location, normal })
}

pub fn world_to_local(point: Vector, location: Vector, axes: &[Vector; 3], scale: Vector, pre_pivot: Vector) -> Option<Vector> {
    if <[f32; 3]>::from(scale).contains(&0.0) {
        return None;
    }
    let d = point - location;
    Some([0, 1, 2].map(|k| d.dot(axes[k]) / scale[k] + pre_pivot[k]).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_enters_cylinder_wall_and_cap() {
        let hit = segment_cylinder(Vector::new(-100.0, 0.0, 0.0), Vector::new(100.0, 0.0, 0.0), Vector::ZERO, 10.0, 20.0).unwrap();
        assert!((hit.location.x + 10.0).abs() < 1e-4 && hit.normal == Vector::new(-1.0, 0.0, 0.0));
        let hit = segment_cylinder(Vector::new(0.0, 0.0, 100.0), Vector::new(0.0, 0.0, -100.0), Vector::ZERO, 10.0, 20.0).unwrap();
        assert!((hit.location.z - 20.0).abs() < 1e-4 && hit.normal == Vector::new(0.0, 0.0, 1.0));
        assert!(segment_cylinder(Vector::new(-100.0, 50.0, 0.0), Vector::new(100.0, 50.0, 0.0), Vector::ZERO, 10.0, 20.0).is_none());
    }

    #[test]
    fn world_to_local_inverts_the_placement() {
        let (location, scale, pre_pivot) = (Vector::new(100.0, -50.0, 20.0), Vector::new(2.0, 1.0, 0.5), Vector::new(1.0, 0.0, 4.0));
        let local = Vector::new(3.0, 4.0, 5.0);
        let axes = p2_math::Rotator::new(0, 16384, 0).axes();
        let s = (local - pre_pivot) * scale;
        let world = location + axes[0] * s.x + axes[1] * s.y + axes[2] * s.z;
        let back = world_to_local(world, location, &axes, scale, pre_pivot).unwrap_or(Vector::new(f32::NAN, f32::NAN, f32::NAN));
        for i in 0..3 {
            assert!((back[i] - local[i]).abs() < 1e-3, "{back:?} != {local:?}");
        }
        assert_eq!(world_to_local(world, location, &axes, Vector::new(0.0, 1.0, 1.0), pre_pivot), None);
    }
}
