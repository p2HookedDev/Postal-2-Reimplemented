use std::path::PathBuf;
use std::sync::Arc;

use bevy::asset::{Assets, Handle};
use bevy::image::Image;
use bevy::math::Vec3;
use bevy::transform::components::Transform;
use p2_assets::{Model, ModelPolygon, RenderState, StaticMesh, poly_flags, read_actor_location, read_level, read_model};
use p2_bevy_assets::to_bevy;
use p2_package::{LoadedPackage, Loader};
use p2_script::{Objects, Source};

use super::static_meshes::placed_box;
use super::{SwitchImage, bsp_groups, keep_restyled_out_of_world_pass};
use crate::scene::{ModelView, SectionView, points_box};

#[test]
fn only_surfaces_a_switch_restyles_leave_the_world_pass() {
    let mut images = Assets::<Image>::default();
    let [restyled, image_only, plain] = [(); 3].map(|()| images.add(Image::default()));
    let section = |image: &Handle<Image>| SectionView { texture: Some(image.clone()), chunks: Some(Arc::from(Vec::new())), ..SectionView::textured(Handle::default(), None) };
    let mut models = [ModelView { sections: vec![section(&restyled), section(&image_only), section(&plain)], instances: Vec::new(), zone: None, casts: false }];
    let switch = |image: &Handle<Image>, whole| SwitchImage { image: image.id(), whole, shown: RenderState::default() };
    keep_restyled_out_of_world_pass(&mut models, [switch(&restyled, true), switch(&image_only, false)].iter());
    let in_table: Vec<bool> = models[0].sections.iter().map(|section| section.chunks.is_some()).collect();
    assert_eq!(in_table, [false, true, true]);
}

fn ray_hits<'a>(polygons: &[&'a ModelPolygon], origin: [f32; 3], dir: [f32; 3]) -> Vec<(f32, &'a ModelPolygon)> {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let mut hits = Vec::new();
    for &p in polygons {
        if dot(dir, p.normal) >= 0.0 && p.poly_flags & poly_flags::TWO_SIDED == 0 {
            continue;
        }
        for i in 1..p.positions.len().saturating_sub(1) {
            let (a, b, c) = (p.positions[0], p.positions[i], p.positions[i + 1]);
            let (e1, e2) = (sub(b, a), sub(c, a));
            let h = cross(dir, e2);
            let det = dot(e1, h);
            if det.abs() < 1e-6 {
                continue;
            }
            let s = sub(origin, a);
            let u = dot(s, h) / det;
            let q = cross(s, e1);
            let v = dot(dir, q) / det;
            let t = dot(e2, q) / det;
            if (0.0..=1.0).contains(&u) && v >= 0.0 && u + v <= 1.0 && t > 0.1 {
                hits.push((t, p));
                break;
            }
        }
    }
    hits.sort_by(|a, b| a.0.total_cmp(&b.0));
    hits
}

fn bsp_node(zones: [u8; 2]) -> p2_assets::BspNode {
    p2_assets::BspNode {
        plane: [0.0; 4],
        node_flags: 0,
        vert_pool: 0,
        surface: 0,
        back: None,
        front: None,
        coplanar: None,
        collision_bound: None,
        render_bound: None,
        zones,
        num_vertices: 0,
        leaves: [0; 2],
        section: None,
        first_vertex: 0,
        light_map: None,
    }
}

#[test]
fn opaque_bsp_splits_by_zone_but_for_zone_0() {
    let model = Model { nodes: vec![bsp_node([9, 1]), bsp_node([9, 2]), bsp_node([9, 0]), bsp_node([2, 0])], ..Model::default() };
    let polygon = |node, poly_flags| ModelPolygon { node, surface: 0, material: 1, poly_flags, normal: [0.0; 3], positions: vec![[0.0; 3]], uvs: Vec::new() };
    let polygons = [polygon(0, 0), polygon(1, 0), polygon(1, poly_flags::TWO_SIDED), polygon(2, 0), polygon(3, 0)];
    let groups = bsp_groups(&model, &polygons, None);
    let sizes: Vec<usize> = groups.values().map(Vec::len).collect();
    assert_eq!(groups.len(), 4, "zone 1, zone 2, zones 2 and 9 of the two-sided one, and zone 0's two together: {sizes:?}");
    assert!(sizes.contains(&2));
}

#[test]
fn only_translucent_bsp_splits_by_place() {
    let node = bsp_node([1, 1]);
    let model = Model { nodes: vec![node], ..Model::default() };
    let polygon = |poly_flags, x| ModelPolygon { node: 0, surface: 0, material: 1, poly_flags, normal: [0.0; 3], positions: vec![[x, 0.0, 0.0]], uvs: Vec::new() };
    let far = 1_000_000.0;
    let polygons = [polygon(0, 0.0), polygon(0, far), polygon(poly_flags::TRANSLUCENT, 0.0), polygon(poly_flags::TRANSLUCENT, far)];
    assert_eq!(bsp_groups(&model, &polygons, None).len(), 3);
}

#[test]
fn suburbs3_sky_zone_drawn_by_the_sky_camera_only() {
    let Some(dir) = std::env::var_os("P2_GAME_DIR").map(PathBuf::from) else { return };
    let mut loader = Loader::open(&dir.join("System")).unwrap();
    loader.load_scripts().unwrap();
    for map_name in ["Suburbs-3", "Church"] {
        let map = loader.load(map_name).unwrap();
        let LoadedPackage { package: pkg, data, .. } = loader.package(map);
        let level = pkg.exports.iter().find(|e| pkg.object_name(e.class_index) == Some("Level")).unwrap();
        let level = read_level(pkg, data, level).unwrap();
        let model = read_model(pkg, data, &pkg.exports[level.model as usize - 1]).unwrap();
        let sky = pkg.exports.iter().position(|e| pkg.object_name(e.class_index) == Some("SkyZoneInfo")).unwrap();
        let sky_zone = p2_collision::point_zone(&model, read_actor_location(&mut Objects::new(&loader), Source { package: pkg, data }, sky).unwrap());
        assert_eq!(pkg.object_name(pkg.exports[model.zones[usize::from(sky_zone)].actor as usize - 1].class_index), Some("SkyZoneInfo"));
        let polygons = model.polygons();
        let groups = bsp_groups(&model, &polygons, Some(sky_zone));
        let front_zone = |p: &ModelPolygon| model.nodes[p.node].zones[1];
        let (in_sky, in_view): (Vec<_>, Vec<_>) = groups.iter().flat_map(|(k, v)| v.iter().map(move |(p, _)| (k.in_sky, *p))).partition(|(sky, _)| *sky);
        assert!(!in_sky.is_empty() && in_sky.iter().all(|(_, p)| front_zone(p) == sky_zone));
        assert!(in_view.iter().all(|(_, p)| front_zone(p) != sky_zone && p.poly_flags & poly_flags::FAKE_BACKDROP == 0));
        let without = bsp_groups(&model, &polygons, None);
        let backdrop = without.values().flatten().filter(|(p, _)| p.poly_flags & poly_flags::FAKE_BACKDROP != 0).count();
        assert!(backdrop > 300, "{backdrop}");
        let drawn: Vec<&ModelPolygon> = polygons.iter().filter(|p| p.poly_flags & (poly_flags::INVISIBLE | poly_flags::PORTAL) == 0).collect();
        let mut objects = Objects::new(&loader);
        let starts: Vec<[f32; 3]> = (0..pkg.exports.len())
            .filter(|&i| pkg.object_name(pkg.exports[i].class_index) == Some("PlayerStart"))
            .filter_map(|i| read_actor_location(&mut objects, Source { package: pkg, data }, i).ok())
            .map(<[f32; 3]>::from)
            .take(4)
            .collect();
        let (mut sky_rays, mut sky_box_before, mut leaks) = (0, 0, 0);
        for start in &starts {
            let origin = [start[0], start[1], start[2] + 60.0];
            let n = 1000;
            for i in 0..n {
                let z = 1.0 - (i as f32 + 0.5) / n as f32 * 2.0;
                let r = (1.0 - z * z).sqrt();
                let phi = i as f32 * 2.399_963;
                let hits = ray_hits(&drawn, origin, [r * phi.cos(), r * phi.sin(), z]);
                if !hits.first().is_some_and(|(_, p)| p.poly_flags & poly_flags::FAKE_BACKDROP != 0) {
                    continue;
                }
                sky_rays += 1;
                let behind = hits.iter().find(|(_, p)| p.poly_flags & poly_flags::FAKE_BACKDROP == 0);
                sky_box_before += usize::from(behind.is_some_and(|(_, p)| front_zone(p) == sky_zone));
                leaks += usize::from(behind.is_some_and(|(_, p)| front_zone(p) != sky_zone));
            }
        }
        eprintln!("{map_name}: sky zone {sky_zone}, {} sky polygons; {sky_rays} rays see the sky, behind the backdrop {sky_box_before} the sky box (now not drawn), {leaks} other BSP", in_sky.len());
        assert!(sky_rays > 500);
        assert!(leaks * 100 <= sky_rays, "{leaks} of {sky_rays}");
    }
}

#[test]
fn a_mesh_is_in_the_zones_its_vertices_reach_not_its_pivots() {
    let model = crate::view::two_zone_model();
    let mesh = StaticMesh { positions: vec![[0.0, 0.0, 110.0], [10.0, 10.0, 120.0]], ..Default::default() };
    let local = points_box(mesh.positions.iter().map(|&p| to_bevy(p)));
    let pivot_below = Transform::from_translation(Vec3::from(to_bevy([0.0, 0.0, -100.0])));
    let (min, max) = placed_box(local, &pivot_below).unwrap();
    assert_eq!(p2_collision::box_zones(&model, min.into(), max.into()), [2]);
    assert!(placed_box(None, &pivot_below).is_none());
}
