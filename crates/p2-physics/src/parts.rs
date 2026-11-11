use rapier3d::prelude::*;

use crate::{BodyId, World, from_rapier, to_rapier};

impl World {
    pub fn parts(&self, bodies: &[BodyId]) -> Parts {
        let mut parts = Parts::default();
        self.parts_into(bodies, &mut parts);
        parts
    }

    pub fn parts_into(&self, bodies: &[BodyId], parts: &mut Parts) {
        parts.colliders.clear();
        parts.centres.clear();
        parts.bounds = None;
        for &id in bodies {
            let Some(body) = self.bodies.get(id.0) else { continue };
            parts.centres.push((id, from_rapier(body.center_of_mass())));
            for part in body.colliders().iter().filter_map(|&c| self.colliders.get(c)) {
                let aabb = part.compute_aabb();
                parts.bounds = Some(parts.bounds.map_or(aabb, |bounds| bounds.merged(&aabb)));
                parts.colliders.push(Part { body: id, shape: part.shared_shape().clone(), pose: *part.position(), aabb });
            }
        }
    }
}

#[derive(Clone)]
struct Part {
    body: BodyId,
    shape: SharedShape,
    pose: Pose,
    aabb: Aabb,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentHit {
    pub body: BodyId,
    pub time: f32,
    pub normal: p2_math::Vector,
}

#[derive(Clone, Default)]
pub struct Parts {
    colliders: Vec<Part>,
    bounds: Option<Aabb>,
    centres: Vec<(BodyId, p2_math::Vector)>,
}

impl Parts {
    pub fn bounds(&self) -> Option<(p2_math::Vector, p2_math::Vector)> {
        self.bounds.map(|b| (b.mins.to_array().into(), b.maxs.to_array().into()))
    }

    pub fn segment_hit(&self, start: p2_math::Vector, end: p2_math::Vector) -> Option<SegmentHit> {
        use rapier3d::parry::query::{Ray, RayCast};
        let origin = to_rapier(start);
        let ray = Ray::new(origin, to_rapier(end) - origin);
        if !self.bounds.is_some_and(|b| b.intersects_local_ray(&ray, 1.0)) {
            return None;
        }
        self.colliders
            .iter()
            .filter(|part| part.aabb.intersects_local_ray(&ray, 1.0))
            .filter_map(|part| {
                let hit = part.shape.cast_ray_and_get_normal(&part.pose, &ray, 1.0, true)?;
                Some(SegmentHit { body: part.body, time: hit.time_of_impact, normal: hit.normal.to_array().into() })
            })
            .min_by(|a, b| a.time.total_cmp(&b.time))
    }

    pub fn nearest_body(&self, point: p2_math::Vector) -> Option<BodyId> {
        let distance = |c: &p2_math::Vector| (*c - point).size_squared();
        self.centres.iter().min_by(|a, b| distance(&a.1).total_cmp(&distance(&b.1))).map(|&(id, _)| id)
    }
}
