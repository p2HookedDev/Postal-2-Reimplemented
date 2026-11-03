//! As Karma, a shape behind a level triangle is pushed to its front; Rapier alone would push it further in.

use rapier3d::prelude::*;

pub(crate) const ONE_SIDED: u128 = 1;

/// Not the origin for other shapes: an actor's origin can lie outside its shape.
fn axis(collider: &Collider) -> ([Vector; 2], Option<f32>) {
    let pose = collider.position();
    if let Some(capsule) = collider.shape().as_capsule() {
        return ([pose.transform_point(capsule.segment.a), pose.transform_point(capsule.segment.b)], Some(capsule.radius));
    }
    let centre = collider.compute_aabb().center();
    ([centre; 2], collider.shape().as_ball().map(|ball| ball.radius))
}

fn over_triangle(point: Vector, [a, b, c]: [Vector; 3], front: Vector) -> bool {
    [(a, b), (b, c), (c, a)].into_iter().all(|(from, to)| (to - from).cross(point - from).dot(front) >= 0.0)
}

pub(crate) fn push_to_front(context: &mut ContactModificationContext) {
    let ModifiableContacts::Rigid(contacts) = &mut context.contacts else { return };
    let (Some(first), Some(second)) = (context.colliders.get(context.collider1), context.colliders.get(context.collider2)) else { return };
    let (level, part, triangle, outward) = if first.user_data == ONE_SIDED {
        (first, second, contacts.manifold.subshape1, 1.0)
    } else if second.user_data == ONE_SIDED {
        (second, first, contacts.manifold.subshape2, -1.0)
    } else {
        return;
    };
    let Some(mesh) = level.shape().as_trimesh() else { return };
    let triangle = mesh.triangle(triangle);
    let corners = [triangle.a, triangle.b, triangle.c].map(|corner| level.position().transform_point(corner));
    let Some(front) = (corners[1] - corners[0]).cross(corners[2] - corners[0]).try_normalize() else { return };
    let (ends, radius) = axis(part);
    let heights = ends.map(|end| front.dot(end - corners[0]));
    let (low, high) = (heights[0].min(heights[1]), heights[0].max(heights[1]));
    if low >= 0.0 {
        return;
    }
    let Some(radius) = radius else {
        contacts.solver_contacts.clear();
        return;
    };
    if high < -radius {
        contacts.solver_contacts.clear();
        return;
    }
    let nearest = if heights[0] * heights[1] < 0.0 {
        ends[0] + (ends[1] - ends[0]) * (heights[0] / (heights[0] - heights[1]))
    } else if heights[0].abs() < heights[1].abs() {
        ends[0]
    } else {
        ends[1]
    };
    let over = |point: Vector| over_triangle(point - front * front.dot(point - corners[0]), corners, front);
    if over(nearest) || over(ends[0]) || over(ends[1]) {
        *contacts.normal = front * outward;
        for contact in contacts.solver_contacts.iter_mut() {
            contact.dist = low - radius;
        }
    } else if contacts.normal.dot(front) * outward < 0.0 {
        contacts.solver_contacts.clear();
    }
}
