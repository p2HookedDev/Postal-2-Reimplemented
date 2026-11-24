use std::collections::BTreeMap;
use bevy::asset::Handle;
use bevy::image::Image;
use bevy::material::AlphaMode;
use bevy::math::{UVec2, Vec3};
use bevy::transform::components::Transform;
use p2_assets::{Blending, Model, ModelPolygon, poly_flags};
use p2_bevy_assets::{model_polygons_to_mesh, texture_to_image, to_bevy};
use p2_bevy_assets::library::Game;
use p2_package::PackageId;
use rustc_hash::FxHashMap;
use super::{PendingAssets, SceneAssets};
use super::static_meshes::grid_cell;
use crate::level_collision::{MaskedCaster, masked_triangles};
use crate::scene::{MaterialImages, ModelView, SectionView, ZoneCulled, points_box};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct BspKey {
    pub(crate) in_sky: bool,
    material: i32,
    atlas: Option<usize>,
    flags: u32,
    zones: Option<Vec<u8>>,
    cell: [i32; 2],
}

/// Invisible surfaces and portals are left out; fake backdrop surfaces too where a sky zone shows
/// through them (unverified). Surfaces are split by zone for culling, a node by its front zone and a two-sided one by
/// both: a backdrop is a hole, and without culling the zones behind it showed where the sky is. Opaque and masked
/// surfaces of zone 0 stay unsplit and always drawn. Translucent ones are split by place too.
pub fn bsp_groups<'a>(
    model: &Model,
    polygons: &'a [ModelPolygon],
    sky_zone: Option<u8>,
) -> BTreeMap<BspKey, Vec<(&'a ModelPolygon, Option<Vec<[f32; 2]>>)>> {
    let backdrop = if sky_zone.is_some() { poly_flags::FAKE_BACKDROP } else { 0 };
    let hidden = poly_flags::INVISIBLE | poly_flags::PORTAL | backdrop;
    let drawing = poly_flags::MASKED | poly_flags::TRANSLUCENT | poly_flags::TWO_SIDED;
    let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for polygon in polygons.iter().filter(|p| p.poly_flags & hidden == 0) {
        let node = model.nodes.get(polygon.node);
        let in_sky = sky_zone.is_some_and(|zone| node.is_some_and(|node| node.zones[1] == zone));
        let translucent = polygon.poly_flags & poly_flags::TRANSLUCENT != 0;
        let zones = node.filter(|_| !in_sky).map(|node| {
            let mut zones = if polygon.poly_flags & poly_flags::TWO_SIDED == 0 { vec![node.zones[1]] } else { node.zones.to_vec() };
            zones.sort_unstable();
            zones.dedup();
            zones
        });
        let zones = zones.filter(|zones| translucent || !zones.contains(&0));
        let at = if translucent && zones.is_some() { polygon.positions.first().map_or([0, 0], |&p| grid_cell(Vec3::from(to_bevy(p)))) } else { [0, 0] };
        let (atlas, uvs) = model.light_map_uvs(polygon).unzip();
        groups.entry(BspKey { in_sky, material: polygon.material, atlas, flags: polygon.poly_flags & drawing, zones, cell: at }).or_default().push((polygon, uvs));
    }
    groups
}

pub struct BspParts {
    pub models: Vec<ModelView>,
    pub sky_models: Vec<ModelView>,
    pub textured: usize,
    pub lit: usize,
    pub min: Vec3,
    pub max: Vec3,
    pub atlases: Vec<Option<Handle<Image>>>,
    pub masked_casters: Vec<MaskedCaster>,
    pub material_albedos: FxHashMap<i32, [f32; 3]>,
}

pub fn bsp_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    id: PackageId,
    model: &Model,
    groups: &BTreeMap<BspKey, Vec<(&ModelPolygon, Option<Vec<[f32; 2]>>)>>,
) -> BspParts {
    let atlases: Vec<Option<Handle<Image>>> = model
        .light_map_textures
        .iter()
        .map(|atlas| atlas.texture().and_then(|texture| texture_to_image(&texture, None)))
        .map(|image| image.map(|image| t.add_image(image)))
        .collect();
    let points =
        groups.values().flatten().flat_map(|(p, _)| &p.positions).map(|&p| Vec3::from(to_bevy(p)));
    let (min, max) = points.fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let (mut textured, mut lit) = (0, 0);
    let mut drawn = Vec::new();
    let mut masked_casters = Vec::new();
    let mut material_albedos = FxHashMap::default();
    for (BspKey { in_sky, material, atlas, flags, zones, .. }, polygons) in groups {
        let (in_sky, material, atlas, flags) = (*in_sky, *material, *atlas, *flags);
        let mut found = texture(game, t, id, material);
        if let Some(MaterialImages { state, .. }) = &mut found {
            if flags & poly_flags::TRANSLUCENT != 0 {
                state.blending = Some(Blending::Additive);
            } else if flags & poly_flags::MASKED != 0 {
                state.blending = Some(Blending::Masked);
            }
            state.two_sided |= flags & poly_flags::TWO_SIDED != 0;
        }
        textured += polygons.len() * usize::from(found.is_some());
        if let Some(albedo) = found.as_ref().and_then(|found| t.albedos.get(&found.image.id())) {
            material_albedos.insert(material, *albedo);
        }
        let masked = found.as_ref().filter(|_| !in_sky).and_then(|found| match crate::scene::alpha_mode(&found.state) {
            AlphaMode::Mask(cutoff @ ..=1.0) => Some((found.image.clone(), cutoff)),
            _ => None,
        });
        let lightmap = atlas.and_then(|atlas| atlases.get(atlas).cloned().flatten());
        lit += polygons.len() * usize::from(lightmap.is_some());
        let size = found.as_ref().and_then(|found| t.image_size(&found.image)).unwrap_or(UVec2::splat(128));
        let scale = [1.0 / size.x.max(1) as f32, 1.0 / size.y.max(1) as f32];
        // The engine lists each node of a surface that needs sorting on its own, in BSP order.
        let sorted = found.as_ref().is_some_and(|found| !matches!(crate::scene::alpha_mode(&found.state), AlphaMode::Opaque | AlphaMode::Mask(_)));
        let mut parts: BTreeMap<Option<usize>, Vec<_>> = BTreeMap::new();
        for polygon in polygons {
            parts.entry(sorted.then_some(polygon.0.node)).or_default().push(polygon);
        }
        for (bsp_node, polygons) in parts {
            let mut mesh = model_polygons_to_mesh(polygons.iter().map(|(p, uvs)| (*p, uvs.as_deref())), scale);
            if let Some((image, cutoff)) = &masked {
                masked_casters.push(MaskedCaster { image: image.clone(), cutoff: *cutoff, triangles: masked_triangles(&mesh, &Transform::IDENTITY) });
            }
            let zone = zones.clone().zip(points_box(polygons.iter().flat_map(|(p, _)| p.positions.iter().copied())));
            let zone = zone.map(|(zones, (min, max))| ZoneCulled { zones, min, max, only_from: None });
            let chunks = if flags & poly_flags::TRANSLUCENT == 0 { crate::world_pass::chunks_of(&mut mesh) } else { None };
            let section = SectionView::textured(t.add_mesh(mesh), found.clone());
            let section = SectionView { no_scene_lights: true, lightmap: lightmap.clone().filter(|_| !section.fullbright), chunks, bsp_node, ..section };
            drawn.push((in_sky, ModelView { sections: vec![section], instances: vec![Transform::IDENTITY], zone, casts: !in_sky }));
        }
    }
    let (sky_bsp, bsp): (Vec<_>, Vec<_>) = drawn.into_iter().partition(|(in_sky, _)| *in_sky);
    let strip = |models: Vec<(bool, ModelView)>| models.into_iter().map(|(_, model)| model).collect::<Vec<_>>();
    BspParts { models: strip(bsp), sky_models: strip(sky_bsp), textured, lit, min, max, atlases, masked_casters, material_albedos }
}
