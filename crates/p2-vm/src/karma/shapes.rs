use std::f32::consts::PI;

use p2_assets::{ElemTm, KMeshProps};

use p2_physics::Shape;

/// Unreal units per Karma length unit; `KMeshProps` shapes are stored in Karma units.
const ME_TO_UNREAL: f32 = 50.0;
const CYLINDER_SIDES: usize = 12;

/// Boxes and cylinders become hulls so any scale stays exact; spheres take the largest scale.
/// Cylinders are 12-sided along the element's Z (unverified).
pub(crate) fn mesh_shapes(props: Option<&KMeshProps>, bounds: (p2_math::Vector, p2_math::Vector), scale: p2_math::Vector, pre_pivot: p2_math::Vector) -> Vec<Shape> {
    let place = |p: p2_math::Vector| [0, 1, 2].map(|i| (p[i] - pre_pivot[i]) * scale[i]);
    let (min, max) = bounds;
    let Some(props) = props else {
        return vec![Shape::Hull(box_corners(min, max).map(place).to_vec())];
    };
    let element = |tm: &ElemTm, p: p2_math::Vector| [0, 1, 2].map(|i| (p[0] * tm[0][i] + p[1] * tm[1][i] + p[2] * tm[2][i] + tm[3][i]) * ME_TO_UNREAL);
    let geom = &props.geom;
    let mut shapes = Vec::new();
    for sphere in &geom.spheres {
        let largest = <[f32; 3]>::from(scale).iter().fold(0.0_f32, |a, s| a.max(s.abs()));
        shapes.push(Shape::Sphere((place(element(&sphere.tm, [0.0; 3].into()).into())).into(), sphere.radius * ME_TO_UNREAL * largest));
    }
    for b in &geom.boxes {
        let half = b.size.map(|s| s / 2.0);
        let corners = box_corners(half.map(|h| -h).into(), half.into());
        shapes.push(Shape::Hull(corners.iter().map(|&c| place(element(&b.tm, c).into())).collect()));
    }
    for c in &geom.cylinders {
        let points = (0..CYLINDER_SIDES * 2).map(|i| {
            let angle = (i % CYLINDER_SIDES) as f32 * 2.0 * PI / CYLINDER_SIDES as f32;
            let z = if i < CYLINDER_SIDES { -c.height / 2.0 } else { c.height / 2.0 };
            place(element(&c.tm, [c.radius * angle.cos(), c.radius * angle.sin(), z].into()).into())
        });
        shapes.push(Shape::Hull(points.collect()));
    }
    for hull in &geom.convexes {
        shapes.push(Shape::Hull(hull.vertices.iter().map(|&v| place(element(&hull.tm, v.into()).into())).collect()));
    }
    if shapes.is_empty() {
        shapes.push(Shape::Hull(box_corners(min, max).map(place).to_vec()));
    }
    shapes
}

fn box_corners(min: p2_math::Vector, max: p2_math::Vector) -> [p2_math::Vector; 8] {
    std::array::from_fn(|i| [0, 1, 2].map(|axis| if i >> axis & 1 == 0 { min[axis] } else { max[axis] }).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_corners_span_the_box() {
        let corners = box_corners([-1.0, -2.0, -3.0].into(), [1.0, 2.0, 3.0].into());
        assert!(corners.contains(&[-1.0, -2.0, -3.0].into()) && corners.contains(&[1.0, 2.0, 3.0].into()) && corners.contains(&[1.0, -2.0, 3.0].into()));
    }

    #[test]
    fn a_mesh_without_props_is_its_box() {
        let shapes = mesh_shapes(None, ([-1.0; 3].into(), [1.0; 3].into()), [2.0, 2.0, 2.0].into(), [1.0, 0.0, 0.0].into());
        let [Shape::Hull(points)] = shapes.as_slice() else { panic!("one hull") };
        assert!(points.contains(&[-4.0, -2.0, -2.0]) && points.contains(&[0.0, 2.0, 2.0]));
    }
}
