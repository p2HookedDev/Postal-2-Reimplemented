use std::collections::VecDeque;

use p2_assets::{BspNode, Model, poly_flags};
use p2_math::Vector;

use crate::{point_solid, point_zone};

pub fn light_zones(model: &Model, start: Vector, radius: f32) -> Vec<u8> {
    if point_solid(model, start) {
        return Vec::new();
    }
    let own = point_zone(model, start);
    if own == 0 && !model.zones.is_empty() {
        return Vec::new();
    }
    let near_portals: Vec<[u8; 2]> = model
        .nodes
        .iter()
        .filter(|node| is_portal(model, node))
        .filter(|node| {
            let corners = model.verts.iter().skip(node.vert_pool).take(usize::from(node.num_vertices)).filter_map(|vert| model.points.get(vert.point));
            let (sum, count) = corners.clone().fold((Vector::default(), 0.0), |(sum, count), &corner| (sum + Vector::from(corner), count + 1.0));
            if count == 0.0 {
                return false;
            }
            let middle = sum * (1.0 / count);
            let size = corners.map(|&corner| (Vector::from(corner) - middle).size()).fold(0.0, f32::max);
            (middle - start).size() <= radius + size
        })
        .map(|node| node.zones)
        .collect();
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

fn is_portal(model: &Model, node: &BspNode) -> bool {
    node.zones[0] != node.zones[1] && model.surfaces.get(node.surface).is_some_and(|surface| surface.poly_flags & poly_flags::PORTAL != 0)
}

pub fn zone_hops(model: &Model) -> Vec<u8> {
    hops_between(model.zones.len().max(1), model.nodes.iter().filter(|node| is_portal(model, node)).map(|node| node.zones))
}

fn hops_between(count: usize, portals: impl Iterator<Item = [u8; 2]>) -> Vec<u8> {
    let mut joined = vec![Vec::new(); count];
    for [a, b] in portals.map(|zones| zones.map(usize::from)).filter(|&[a, b]| a < count && b < count) {
        joined[a].push(b);
        joined[b].push(a);
    }
    let mut hops = vec![u8::MAX; count * count];
    for (from, row) in hops.chunks_exact_mut(count).enumerate() {
        row[from] = 0;
        let mut queue = VecDeque::from([from]);
        while let Some(zone) = queue.pop_front() {
            let step = row[zone].saturating_add(1);
            for &other in &joined[zone] {
                if row[other] == u8::MAX {
                    row[other] = step;
                    queue.push_back(other);
                }
            }
        }
    }
    hops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_are_as_many_portal_steps_apart_as_the_shortest_way_takes() {
        let hops = hops_between(5, [[1, 2], [2, 3], [3, 1], [3, 1]].into_iter());
        assert_eq!(&hops[5..10], [255, 0, 1, 1, 255]);
        assert_eq!(&hops[10..15], [255, 1, 0, 1, 255]);
        assert_eq!(hops[4 * 5 + 4], 0);
        assert_eq!(hops[4 * 5 + 1], 255);
    }

    #[test]
    fn a_light_in_a_level_without_a_tree_names_no_zone() {
        assert!(light_zones(&Model::default(), Vector::default(), 1625.0).is_empty());
    }
}
