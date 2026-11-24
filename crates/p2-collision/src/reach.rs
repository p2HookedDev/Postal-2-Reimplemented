//! The zones a light could light. Walls are not looked at: rays against them never find every gap, and a zone left out
//! is a room left dark.

use p2_assets::{Model, poly_flags};
use p2_math::Vector;

use crate::{point_solid, point_zone};

/// The zone the light stands in and every zone joined to one of them by a portal that comes within `radius` of the
/// light. Empty where its zone cannot be told: inside the level's solid, or in zone 0, which holds what no portal
/// closes off and so joins other zones without one.
pub fn light_zones(model: &Model, start: Vector, radius: f32) -> Vec<u8> {
    if point_solid(model, start) {
        return Vec::new();
    }
    let near_portals: Vec<[u8; 2]> = model
        .nodes
        .iter()
        .filter(|node| node.zones[0] != node.zones[1] && model.surfaces.get(node.surface).is_some_and(|surface| surface.poly_flags & poly_flags::PORTAL != 0))
        .filter(|node| {
            let corners = model.verts.iter().skip(node.vert_pool).take(usize::from(node.num_vertices)).filter_map(|vert| model.points.get(vert.point));
            let (sum, count) = corners.clone().fold((Vector::default(), 0.0), |(sum, count), &corner| (sum + Vector::from(corner), count + 1.0));
            if count == 0.0 {
                return false;
            }
            let middle = sum * (1.0 / count);
            let far = |from: Vector, to: Vector| (to - from).dot(to - from).sqrt();
            let size = corners.map(|&corner| far(middle, Vector::from(corner))).fold(0.0, f32::max);
            far(start, middle) <= radius + size
        })
        .map(|node| node.zones)
        .collect();
    let own = point_zone(model, start);
    if own == 0 && !model.zones.is_empty() {
        return Vec::new();
    }
    let mut zones = vec![own];
    let mut next = 0;
    while let Some(&zone) = zones.get(next) {
        next += 1;
        for &[a, b] in &near_portals {
            let other = if a == zone {
                b
            } else if b == zone {
                a
            } else {
                continue;
            };
            if !zones.contains(&other) {
                zones.push(other);
            }
        }
    }
    zones
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_light_in_a_level_without_a_tree_names_no_zone() {
        assert!(light_zones(&Model::default(), Vector::default(), 1625.0).is_empty());
    }
}
