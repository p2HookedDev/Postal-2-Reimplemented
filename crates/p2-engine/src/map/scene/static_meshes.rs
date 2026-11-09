use super::*;

const CELL: f32 = 2048.0;

pub(super) fn cell(position: Vec3) -> [i32; 2] {
    [(position.x / CELL).floor() as i32, (position.z / CELL).floor() as i32]
}

pub(super) struct MeshPlacing<I> {
    pub(super) transform: Transform,
    pub(super) instance: Option<(I, MeshLighting)>,
    pub(super) placement: Option<Placement>,
    pub(super) follow: Option<FollowActor>,
    pub(super) in_sky: bool,
    pub(super) only_from: Option<Vec<u8>>,
}

pub(super) struct StaticLevel<'a> {
    pub(super) pkg: &'a Package,
    pub(super) file: &'a [u8],
    pub(super) model: &'a Model,
    pub(super) environment: &'a LightEnvironment,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    material: String,
    has_uvs: bool,
    has_colors: bool,
}

#[derive(Default)]
pub(super) struct StaticParts {
    pub(super) models: Vec<ModelView>,
    pub(super) sky_models: Vec<ModelView>,
    pub(super) followers: Vec<MovingModel<FollowActor>>,
    pub(super) collisions: Vec<MeshCollision>,
    batches: BTreeMap<BatchKey, (SectionView, MeshBatch)>,
    pub(super) lit_actors: usize,
}

fn static_mesh_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    level: &StaticLevel,
    ((mesh_package, reference), skins): ((PackageId, i32), Vec<(PackageId, i32)>),
    placements: Vec<MeshPlacing<Option<i32>>>,
    parts: &mut StaticParts,
) -> bool {
    let mesh = game.export_of(mesh_package, reference).and_then(|(mesh_id, mesh_export)| {
        let (pkg, file) = game.data(mesh_id);
        let export = &pkg.exports[mesh_export];
        let mesh = read_static_mesh(pkg, file, export).ok()?;
        Some((mesh_id, mesh, read_static_mesh_materials(pkg, file, export).unwrap_or_default()))
    });
    let Some((mesh_id, mesh, materials)) = mesh else { return false };
    let mut base = static_mesh_to_meshes(&mesh);
    let textures: Vec<_> = (0..base.len())
        .map(|i| match skins.get(i).filter(|(_, skin)| *skin != 0) {
            Some(&(skin_package, skin)) => texture(game, t, skin_package, skin),
            None => materials.get(i).and_then(|&material| texture(game, t, mesh_id, material)),
        })
        .collect();
    for (section, found) in base.iter_mut().zip(&textures) {
        use_uv_stream(section, &mesh, found.as_ref());
    }
    let views: Vec<SectionView> =
        textures.iter().map(|found| SectionView { no_scene_lights: true, ..SectionView::textured(Handle::default(), found.clone()) }).collect();
    let blended: Vec<bool> = views.iter().map(|view| !matches!(view.alpha_mode, AlphaMode::Opaque | AlphaMode::Mask(_))).collect();
    let material_keys: Vec<String> = views.iter().map(SectionView::material_key).collect();
    let sections = |t: &mut PendingAssets, lighting: Option<&[[u8; 4]]>, relit: bool, mirrored: bool, keep: &dyn Fn(usize) -> bool| -> Vec<SectionView> {
        base.iter()
            .zip(&views)
            .enumerate()
            .filter(|&(i, _)| keep(i))
            .map(|(_, (section, view))| {
                let mut section = section.clone();
                if let Some(lighting) = lighting.filter(|_| !view.fullbright) {
                    section.insert_attribute(Mesh::ATTRIBUTE_COLOR, lighting_colors(lighting));
                }
                if relit {
                    keep_for_relighting(&mut section);
                }
                if mirrored {
                    section.invert_winding().ok();
                }
                SectionView { mesh: t.add_mesh(section), ..view.clone() }
            })
            .collect()
    };
    let mut unlit: [Vec<(Transform, Option<ZoneCulled>)>; 2] = Default::default();
    let mut shared: Option<Arc<StaticMesh>> = None;
    for MeshPlacing { transform, instance, placement, follow, in_sky, only_from } in placements {
        let collision = placement.map(|Placement { location, rotation, scale, pre_pivot }| MeshCollision::new(&mesh, location, rotation, scale, pre_pivot));
        let bounds = collision.as_ref().map(MeshCollision::bounds);
        parts.collisions.extend(collision);
        let mirrored = mirrors(&transform);
        let lit = instance.map(|(reference, lighting)| {
            let export = reference.and_then(|reference| level.pkg.exports.get(usize::try_from(reference - 1).ok()?));
            let instance = export.and_then(|export| read_static_mesh_instance(level.pkg, level.file, export).ok());
            let fits = instance.as_ref().is_some_and(|instance| instance_fits(&mesh, instance));
            (level.environment.mesh_colors(level.model, &mesh, instance.as_ref(), &lighting), fits, lighting)
        });
        let (lighting, relit): (Option<_>, Option<_>) = lit.map(|(colors, fits, lighting)| (colors, (fits, lighting))).unzip();
        if let Some(follow) = follow {
            // Without baked lighting the engine lights it by hardware lights where it is: relit as it moves
            // (movers with an instance keep it, as the engine does).
            let relight = relit.filter(|&(fits, _)| !fits).map(|(_, lighting)| {
                let mesh = shared.get_or_insert_with(|| Arc::new(mesh.clone())).clone();
                let shading = Shading::Vertices { mesh, scale: lighting.placement.scale, pre_pivot: lighting.placement.pre_pivot };
                ActorLit::new(follow.actor, lighting.glow, MAX_HARDWARE_LIGHTS, 0.0, shading)
                    .lit_at(lighting.placement.location, lighting.placement.rotation)
            });
            parts.followers.push(MovingModel { sections: sections(t, lighting.as_deref(), relight.is_some(), mirrored, &|_| true), transform, follow, lit: relight });
            continue;
        }
        if in_sky {
            parts.sky_models.push(ModelView { sections: sections(t, lighting.as_deref(), false, mirrored, &|_| true), instances: vec![transform], zone: None });
            continue;
        }
        parts.lit_actors += usize::from(lighting.is_some());
        let zone = bounds
            .filter(|_| !level.model.leaves.is_empty())
            .map(|(min, max)| ZoneCulled { zones: p2_collision::box_zones(level.model, min, max), min: min.into(), max: max.into(), only_from: only_from.clone() });
        if only_from.is_some() {
            parts.models.push(ModelView { sections: sections(t, lighting.as_deref(), false, mirrored, &|_| true), instances: vec![transform], zone });
            continue;
        }
        let colors = lighting.as_deref().map(lighting_colors);
        for (i, section) in base.iter().enumerate().filter(|&(i, _)| !blended[i]) {
            let view = &views[i];
            let colors = colors.as_deref().filter(|_| !view.fullbright);
            let key = BatchKey { material: material_keys[i].clone(), has_uvs: section.attribute(Mesh::ATTRIBUTE_UV_0).is_some(), has_colors: colors.is_some() };
            let (_, batch) = parts.batches.entry(key).or_insert_with(|| (view.clone(), MeshBatch::default()));
            batch.push(section, &transform, colors);
        }
        if blended.contains(&true) {
            match lighting {
                Some(lighting) => parts.models.push(ModelView { sections: sections(t, Some(&lighting), false, mirrored, &|i| blended[i]), instances: vec![transform], zone }),
                None => unlit[usize::from(mirrored)].push((transform, zone)),
            }
        }
    }
    for (mirrored, unlit) in unlit.into_iter().enumerate().filter(|(_, unlit)| !unlit.is_empty()) {
        let sections = sections(t, None, false, mirrored == 1, &|i| blended[i]);
        parts.models.extend(unlit.into_iter().map(|(transform, zone)| ModelView { sections: sections.clone(), instances: vec![transform], zone }));
    }
    true
}


pub(super) fn resolve_mesh_paths(
    game: &mut Game,
    id: PackageId,
    mesh_paths: HashMap<(String, Vec<Option<String>>), Vec<MeshPlacing<Option<String>>>>,
    placements: &mut HashMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>>,
) -> usize {
    let mut missing = 0;
    for ((mesh, skins), actors) in mesh_paths {
        let mesh = game.export_by_path_of_class(&mesh, "StaticMesh").map(|(package, e)| (package, export_reference(e)));
        let mut export = |path: &str| game.export_by_path(path).map(|(package, e)| (package, export_reference(e)));
        let Some(mesh) = mesh else {
            missing += 1;
            continue;
        };
        let skins = skins.iter().map(|skin| skin.as_deref().and_then(&mut export).unwrap_or((id, 0))).collect();
        let actors = actors
            .into_iter()
            .map(|MeshPlacing { transform, instance, placement, follow, in_sky, only_from }| {
                let instance = instance.map(|(path, lighting)| {
                    let reference = path.and_then(|path| export(&path)).filter(|&(p, _)| p == id).map(|(_, reference)| reference);
                    (reference, lighting)
                });
                MeshPlacing { transform, instance, placement, follow, in_sky, only_from }
            })
            .collect::<Vec<_>>();
        placements.entry((mesh, skins)).or_default().extend(actors);
    }
    missing
}


pub(super) fn antiportal_actors(game: &mut Game, model: &Model, paths: &[(String, MeshPlacement)]) -> Vec<AntiportalActor> {
    paths
        .iter()
        .filter_map(|(path, placement)| {
            let (package, export) = game.export_by_path_of_class(path, "ConvexVolume")?;
            let (pkg, file) = game.data(package);
            let volume = read_convex_volume(pkg, file, &pkg.exports[export]).ok()?;
            AntiportalActor::new(&volume, placement, |min, max| p2_collision::box_zones(model, min, max))
        })
        .collect()
}

pub(super) fn static_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    level: &StaticLevel,
    placements: HashMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>>,
) -> (StaticParts, usize, usize) {
    let (mut parts, mut missing) = (StaticParts::default(), 0);
    for (mesh, placements) in placements {
        if !static_mesh_models(game, t, texture, level, mesh, placements, &mut parts) {
            missing += 1;
        }
    }
    let batches = std::mem::take(&mut parts.batches);
    let merged = batches.len();
    for (view, batch) in batches.into_values() {
        let mut mesh = batch.into_mesh();
        let chunks = crate::render::world_pass::chunks_of(&mut mesh);
        parts.models.push(ModelView { sections: vec![SectionView { mesh: t.add_mesh(mesh), chunks, ..view }], instances: vec![Transform::IDENTITY], zone: None });
    }
    (parts, missing, merged)
}
