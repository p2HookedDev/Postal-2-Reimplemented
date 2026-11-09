pub(crate) use p2_math::{Rotator, Vector, turn};

use crate::Rng;

pub(crate) fn mirror(v: Vector, normal: Vector) -> Vector {
    v - (normal * (2.0 * v.dot(normal)))
}

pub(crate) fn non_parallel(v: Vector) -> Vector {
    if v.x.abs() > 0.57 {
        [0.0, 1.0, 0.0].into()
    } else if v.y.abs() > 0.57 {
        [0.0, 0.0, 1.0].into()
    } else {
        [1.0, 0.0, 0.0].into()
    }
}

pub(crate) fn rotate_about(vs: [Vector; 2], units: i32, axis: Vector) -> [Vector; 2] {
    let (s, c) = turn(units).sin_cos();
    vs.map(|v| ((v * c) + (axis.cross(v) * s)) + (axis * (axis.dot(v) * (1.0 - c))))
}

pub(crate) fn rotate_by(axes: &[Vector; 3], v: Vector) -> Vector {
    axes[0] * v.x + axes[1] * v.y + axes[2] * v.z
}

pub(crate) fn vrand(rng: &mut Rng) -> Vector {
    loop {
        let v = Vector::from([0; 3].map(|_| 1.0 - rng.frand() * 2.0));
        let size = v.size();
        if (0.1..=1.0).contains(&size) {
            return v * (1.0 / size);
        }
    }
}

pub(crate) fn fractional(x: f32) -> f32 {
    x - x.floor()
}
