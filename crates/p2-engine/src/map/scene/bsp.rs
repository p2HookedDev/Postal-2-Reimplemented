use super::*;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct BspKey {
    pub(crate) in_sky: bool,
    material: i32,
    atlas: Option<usize>,
    flags: u32,
    zones: Option<Vec<u8>>,
    cell: [i32; 2],
}

/// Invisible surfaces and portals are left out; fake backdrop surfaces too where a sky zone shows
/// through them (unverified). Opaque and masked surfaces are not split by zone or place, to keep draws few; the depth test
/// hides what is behind. Translucent ones are, as they sort by their mesh's centre: a node by its front zone, two-sided
/// ones the eye's side.
pub(crate) fn bsp_groups<'a>(
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
        let zones = node.filter(|_| translucent && !in_sky).map(|node| {
            let mut zones = if polygon.poly_flags & poly_flags::TWO_SIDED == 0 { vec![node.zones[1]] } else { node.zones.to_vec() };
            zones.sort_unstable();
            zones.dedup();
            zones
        });
        let at = if zones.is_some() { polygon.positions.first().map_or([0, 0], |&p| cell(Vec3::from(to_bevy(p)))) } else { [0, 0] };
        let (atlas, uvs) = model.light_map_uvs(polygon).unzip();
        groups.entry(BspKey { in_sky, material: polygon.material, atlas, flags: polygon.poly_flags & drawing, zones, cell: at }).or_default().push((polygon, uvs));
    }
    groups
}

pub(super) struct BspParts {
    pub(super) models: Vec<ModelView>,
    pub(super) sky_models: Vec<ModelView>,
    pub(super) textured: usize,
    pub(super) lit: usize,
    pub(super) min: Vec3,
    pub(super) max: Vec3,
    pub(super) atlases: Vec<Option<Handle<Image>>>,
}

pub(super) fn bsp_models(
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
    let (sky_bsp, bsp): (Vec<_>, Vec<_>) = groups
        .iter()
        .map(|(BspKey { in_sky, material, atlas, flags, zones, .. }, polygons)| {
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
            let lightmap = atlas.and_then(|atlas| atlases.get(atlas).cloned().flatten());
            lit += polygons.len() * usize::from(lightmap.is_some());
            let size = found.as_ref().and_then(|found| t.image_size(&found.image)).unwrap_or(UVec2::splat(128));
            let scale = [1.0 / size.x.max(1) as f32, 1.0 / size.y.max(1) as f32];
            let mut mesh = model_polygons_to_mesh(polygons.iter().map(|(p, uvs)| (*p, uvs.as_deref())), scale);
            let zone = zones.clone().zip(bounds(polygons.iter().flat_map(|(p, _)| p.positions.iter().copied())));
            let zone = zone.map(|(zones, (min, max))| ZoneCulled { zones, min, max, only_from: None });
            let chunks = if zone.is_none() { crate::render::world_pass::chunks_of(&mut mesh) } else { None };
            let section = SectionView::textured(t.add_mesh(mesh), found);
            let section = SectionView { no_scene_lights: true, lightmap: lightmap.filter(|_| !section.fullbright), chunks, ..section };
            (in_sky, ModelView { sections: vec![section], instances: vec![Transform::IDENTITY], zone })
        })
        .partition(|(in_sky, _)| *in_sky);
    let strip = |models: Vec<(bool, ModelView)>| models.into_iter().map(|(_, model)| model).collect::<Vec<_>>();
    BspParts { models: strip(bsp), sky_models: strip(sky_bsp), textured, lit, min, max, atlases }
}
