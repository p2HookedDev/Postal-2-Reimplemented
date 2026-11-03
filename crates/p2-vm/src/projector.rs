use p2_math::{Rotator, Vector};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub normal: Vector,
    pub w: f32,
}

impl Plane {
    fn through(point: Vector, normal: Vector) -> Self {
        Self { normal, w: point.dot(normal) }
    }

    fn of_points(a: Vector, b: Vector, c: Vector) -> Self {
        Self::through(a, (b - a).cross(c - a).safe_normal())
    }

    pub fn dot(self, point: Vector) -> f32 {
        self.normal.dot(point) - self.w
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frustum {
    pub planes: [Plane; 6],
    pub vertices: [Vector; 8],
}

#[derive(Debug, Clone, Copy)]
pub struct ProjectorShape {
    pub location: Vector,
    pub rotation: Rotator,
    pub texels: [f32; 2],
    pub draw_scale: f32,
    pub fov: i32,
    pub max_trace_distance: i32,
    pub rotating: bool,
}

/// Axes X (forward), Y, Z; `r` = ½·√(w² + h²) for the texture's size times `DrawScale`.
pub fn frustum(shape: &ProjectorShape) -> Frustum {
    let [x, y, z] = shape.rotation.axes();
    let side = y * -1.0;
    let (w, h) = (shape.texels[0] * shape.draw_scale, shape.texels[1] * shape.draw_scale);
    let radius = (0.25 * w * w + 0.25 * h * h).sqrt();
    let diagonals = [z + side, z - side, z * -1.0 - side, z * -1.0 + side];
    let near = diagonals.map(|d| {
        let d = if shape.rotating { d } else { d.safe_normal() };
        shape.location + d * radius
    });
    let trace = shape.max_trace_distance as f32;
    let (sides, far) = if shape.fov == 0 {
        let sides = [0, 1, 2, 3].map(|i| Plane::through(near[i], diagonals[i].safe_normal() * -1.0));
        (sides, near.map(|v| v + x * trace))
    } else {
        let half = (shape.fov as f32 * 0.5).to_radians();
        let apex = shape.location - x * (0.5 * w / half.tan());
        let sides = [(1, 0), (2, 1), (3, 2), (0, 3)].map(|(b, c)| Plane::of_points(apex, near[b], near[c]));
        (sides, near.map(|v| v + (v - apex).safe_normal() * (trace / half.cos())))
    };
    let planes = [
        Plane::through(shape.location, x),
        sides[0],
        sides[1],
        sides[2],
        sides[3],
        Plane::through(far[3], x * -1.0),
    ];
    Frustum { planes, vertices: [near[0], near[1], near[2], near[3], far[0], far[1], far[2], far[3]] }
}

pub fn frustum_meets_box(planes: &[Plane; 6], center: Vector, extent: Vector) -> bool {
    planes.iter().all(|plane| {
        let n = plane.normal;
        let reach = (n.x * extent.x).abs() + (n.y * extent.y).abs() + (n.z * extent.z).abs();
        plane.dot(center) >= -reach
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(fov: i32) -> ProjectorShape {
        ProjectorShape {
            location: Vector::new(100.0, 0.0, 0.0),
            rotation: Rotator::new(0, 0, 0),
            texels: [64.0, 64.0],
            draw_scale: 1.0,
            fov,
            max_trace_distance: 500,
            rotating: false,
        }
    }

    fn inside(frustum: &Frustum, point: Vector) -> bool {
        frustum_meets_box(&frustum.planes, point, Vector::ZERO)
    }

    #[test]
    fn orthographic_frustum_spans_the_texture_and_the_trace() {
        let f = frustum(&shape(0));
        assert!((f.vertices[0] - Vector::new(100.0, -32.0, 32.0)).size() < 1e-3, "{:?}", f.vertices[0]);
        assert!((f.vertices[4] - Vector::new(600.0, -32.0, 32.0)).size() < 1e-3);
        assert!(inside(&f, Vector::new(300.0, 0.0, 0.0)));
        assert!(inside(&f, Vector::new(300.0, 30.0, -30.0)));
        assert!(!inside(&f, Vector::new(99.0, 0.0, 0.0)), "behind the projector");
        assert!(!inside(&f, Vector::new(601.0, 0.0, 0.0)), "past MaxTraceDistance");
        assert!(!inside(&f, Vector::new(300.0, 0.0, 70.0)));
    }

    #[test]
    fn perspective_frustum_widens() {
        let f = frustum(&shape(90));
        assert!(inside(&f, Vector::new(400.0, 0.0, 100.0)));
        assert!(!inside(&f, Vector::new(150.0, 0.0, 100.0)));
        assert!(inside(&f, Vector::new(101.0, 0.0, 0.0)));
        assert!(!inside(&f, Vector::new(99.0, 0.0, 0.0)));
    }

    #[test]
    fn box_reach_counts_against_each_plane() {
        let f = frustum(&shape(0));
        let behind = Vector::new(90.0, 0.0, 0.0);
        assert!(!frustum_meets_box(&f.planes, behind, Vector::new(5.0, 5.0, 5.0)));
        assert!(frustum_meets_box(&f.planes, behind, Vector::new(15.0, 5.0, 5.0)));
    }

    #[test]
    fn zero_planes_meet_everything() {
        let zero = Plane { normal: Vector::ZERO, w: 0.0 };
        assert!(frustum_meets_box(&[zero; 6], Vector::new(1e5, 0.0, 0.0), Vector::new(1.0, 1.0, 1.0)));
    }
}
