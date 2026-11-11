use p2_assets::Model;
use p2_math::Vector;

use crate::lighting::{LE_SUNLIGHT, LT_NONE, MAX_HARDWARE_LIGHTS, MeshLight, SUN_DISTANCE};

/// Whether a light cut off from the actor's centre by the level is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walls {
    Checked,
    Ignored,
}

/// The lights in the baked permeating lists of `leaves`, each once, in leaf order.
pub fn leaf_lights(model: &Model, leaves: &[usize]) -> Vec<i32> {
    let mut listed = Vec::new();
    for leaf in leaves.iter().filter_map(|&leaf| model.leaves.get(leaf)) {
        let start = usize::try_from(leaf.permeating).ok();
        let lights = start.and_then(|start| model.lights.get(start..)).unwrap_or_default();
        for &light in lights.iter().take_while(|&&light| light != 0) {
            if !listed.contains(&light) {
                listed.push(light);
            }
        }
    }
    listed
}

/// Per channel the brightest ambient of the first four distinct zones of `leaves`; black without leaves.
pub fn leaves_ambient(model: &Model, leaves: &[usize], zone_ambients: &[[f32; 3]]) -> [f32; 3] {
    let mut zones: Vec<usize> = Vec::new();
    for zone in leaves.iter().filter_map(|&leaf| model.leaves.get(leaf)).filter_map(|leaf| usize::try_from(leaf.zone).ok()) {
        if zones.len() < 4 && !zones.contains(&zone) {
            zones.push(zone);
        }
    }
    zones.iter().map(|&zone| zone_ambients.get(zone).copied().unwrap_or_default()).fold([0.0; 3], |a, b| [0, 1, 2].map(|i| a[i].max(b[i])))
}

/// The lights the renderer gives an actor: those reaching its sphere and, with `Walls::Checked`, not cut off
/// from `center` by the level, sun lights first, the rest by (1 - d² / R²) · brightness with R the light's own
/// radius, at most `max`. A light with a score of zero or less still counts, last. A spot takes a slot whatever
/// its cone; the shading cuts it.
pub fn relevant_lights<'a>(model: &Model, candidates: impl IntoIterator<Item = &'a MeshLight>, center: Vector, radius: f32, max: usize, walls: Walls) -> Vec<MeshLight> {
    // A prop set into a wall has its centre in solid, where every trace starts blocked: it keeps its lights as before
    // this check came in. What retail's trace answers from inside solid is (unverified).
    let traced = walls == Walls::Checked && p2_collision::point_leaf(model, center).is_some();
    let mut keyed: Vec<(f32, MeshLight)> = candidates
        .into_iter()
        .filter(|light| light.light_type != LT_NONE)
        .filter_map(|light| {
            let to_center = center - light.location;
            let (distance_squared, range) = (to_center.dot(to_center), light.world_radius());
            let (key, end) = match light.effect {
                LE_SUNLIGHT => (f32::MAX, center - light.rotation.vector() * SUN_DISTANCE),
                _ if distance_squared > (range + radius) * (range + radius) => return None,
                _ => ((1.0 - distance_squared / (range * range)) * light.brightness, light.location),
            };
            (!traced || light_clear(model, center, end)).then_some((key, *light))
        })
        .collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    keyed.into_iter().take(max.min(MAX_HARDWARE_LIGHTS)).map(|(_, light)| light).collect()
}

/// Level geometry only; the sky's backdrop surfaces let light through (unverified).
fn light_clear(model: &Model, start: Vector, end: Vector) -> bool {
    match p2_collision::line_check_node(model, start, end) {
        None => true,
        Some((_, Some(node))) => p2_collision::is_sky(model, node),
        Some((_, None)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::LE_SPOTLIGHT;
    use p2_assets::{BspNode, BspSurface, Leaf, poly_flags};

    fn wall() -> Model {
        let node = BspNode {
            plane: [1.0, 0.0, 0.0, 0.0],
            node_flags: 0,
            vert_pool: 0,
            surface: 0,
            back: None,
            front: None,
            coplanar: None,
            collision_bound: None,
            render_bound: None,
            zones: [1, 2],
            num_vertices: 4,
            leaves: [0, 1],
            section: None,
            first_vertex: 0,
            light_map: None,
        };
        let surface =
            BspSurface { material: 0, poly_flags: 0, base: 0, normal: 0, texture_u: 0, texture_v: 0, brush_poly: None, actor: 0, plane: node.plane, light_map_scale: 16.0 };
        let leaf = |zone, permeating| Leaf { zone, permeating, volumetric: 0, visible_zones: 0 };
        Model { nodes: vec![node], surfaces: vec![surface], leaves: vec![leaf(1, 0), leaf(2, 2)], lights: vec![10, 0, 20, 30, 0], ..Model::default() }
    }

    fn at(x: f32, brightness: f32) -> MeshLight {
        MeshLight { location: [x, 0.0, 0.0].into(), radius: 7.0, brightness, ..MeshLight::default() }
    }

    #[test]
    fn with_walls_checked_a_light_behind_a_wall_is_dropped() {
        let (center, behind, in_front) = (Vector::new(10.0, 0.0, 0.0), at(-40.0, 255.0), at(60.0, 255.0));
        assert!(relevant_lights(&wall(), &[behind], center, 20.0, 8, Walls::Checked).is_empty());
        assert_eq!(relevant_lights(&Model::default(), &[behind], center, 20.0, 8, Walls::Checked), [behind]);
        assert_eq!(relevant_lights(&wall(), &[in_front], center, 20.0, 8, Walls::Checked), [in_front]);
        let mut sky = wall();
        sky.surfaces[0].poly_flags = poly_flags::FAKE_BACKDROP;
        assert_eq!(relevant_lights(&sky, &[behind], center, 20.0, 8, Walls::Checked), [behind]);
        let sun = |yaw: i32| MeshLight { effect: LE_SUNLIGHT, rotation: [0, yaw, 0].into(), ..MeshLight::default() };
        assert!(relevant_lights(&wall(), &[sun(0)], center, 20.0, 8, Walls::Checked).is_empty());
        assert_eq!(relevant_lights(&wall(), &[sun(32768)], center, 20.0, 8, Walls::Checked), [sun(32768)]);
    }

    #[test]
    fn with_walls_ignored_a_light_behind_a_wall_is_kept() {
        let (center, behind) = (Vector::new(10.0, 0.0, 0.0), at(-40.0, 255.0));
        assert_eq!(relevant_lights(&wall(), &[behind], center, 20.0, 8, Walls::Ignored), [behind]);
        let sun = MeshLight { effect: LE_SUNLIGHT, ..MeshLight::default() };
        assert_eq!(relevant_lights(&wall(), &[sun], center, 20.0, 8, Walls::Ignored), [sun]);
    }

    #[test]
    fn a_spot_facing_away_keeps_its_place_in_the_order() {
        let away = MeshLight { effect: LE_SPOTLIGHT, ..at(100.0, 40.0) };
        let lights = relevant_lights(&Model::default(), &[at(190.0, 255.0), away, at(10.0, 255.0)], Vector::ZERO, 50.0, 8, Walls::Checked);
        assert_eq!(lights, [at(10.0, 255.0), away, at(190.0, 255.0)]);
    }

    #[test]
    fn a_box_takes_the_lights_of_every_leaf_it_touches() {
        let model = wall();
        let (center, reach) = (Vector::new(5.0, 0.0, 0.0), Vector::new(10.0, 10.0, 10.0));
        assert_eq!(leaf_lights(&model, &p2_collision::box_leaves(&model, center - reach, center + reach)), [10, 20, 30]);
        let point = Vector::new(1.0, 1.0, 1.0);
        assert_eq!(leaf_lights(&model, &p2_collision::box_leaves(&model, center - point, center + point)), [20, 30]);
    }

    #[test]
    fn suns_come_first_and_a_light_past_its_radius_is_kept_last() {
        let sun = MeshLight { effect: LE_SUNLIGHT, rotation: [-16384, 0, 0].into(), brightness: 10.0, ..MeshLight::default() };
        let (near, edge, past, gone) = (at(100.0, 64.0), at(200.0, 255.0), at(230.0, 255.0), at(260.0, 255.0));
        let lights = relevant_lights(&Model::default(), &[edge, past, gone, near, sun], Vector::ZERO, 50.0, 8, Walls::Checked);
        assert_eq!(lights, [sun, near, edge, past]);
    }

    #[test]
    fn the_highest_scored_lights_by_their_own_radius_are_kept() {
        let candidates = [at(190.0, 255.0), at(100.0, 40.0), at(10.0, 255.0), at(300.0, 255.0)];
        let lights = relevant_lights(&Model::default(), &candidates, Vector::ZERO, 50.0, 2, Walls::Checked);
        assert_eq!(lights, [at(10.0, 255.0), at(100.0, 40.0)]);
        assert_eq!(relevant_lights(&Model::default(), &candidates, Vector::ZERO, 50.0, 20, Walls::Checked).len(), 3);
    }

    #[test]
    fn the_ambient_is_the_brightest_of_four_zones_per_channel() {
        let ambients = [[0.0; 3], [0.1, 0.5, 0.0], [0.3, 0.2, 0.0], [0.0; 3], [0.0; 3], [1.0; 3]];
        assert_eq!(leaves_ambient(&wall(), &[0, 1], &ambients), [0.3, 0.5, 0.0]);
        assert_eq!(leaves_ambient(&wall(), &[], &ambients), [0.0; 3]);
        let leaf = |zone| Leaf { zone, permeating: -1, volumetric: 0, visible_zones: 0 };
        let many = Model { leaves: [1, 1, 2, 3, 4, 5].map(leaf).to_vec(), ..Model::default() };
        assert_eq!(leaves_ambient(&many, &[0, 1, 2, 3, 4, 5], &ambients), [0.3, 0.5, 0.0]);
    }
}
