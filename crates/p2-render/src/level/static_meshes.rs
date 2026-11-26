use std::collections::BTreeMap;
use std::sync::Arc;
use bevy::asset::Handle;
use bevy::material::AlphaMode;
use bevy::math::Vec3;
use bevy::mesh::Mesh;
use bevy::transform::components::Transform;
use p2_assets::{Model, StaticMesh, read_convex_volume, read_static_mesh, read_static_mesh_instance, read_static_mesh_materials};
use p2_bevy_assets::{MeshBatch, lighting_colors, mirrors, static_mesh_to_meshes, to_bevy};
use p2_bevy_assets::library::{Game, export_reference};
use p2_collision::MeshCollision;
use p2_math::{Rotator, Vector};
use p2_package::{Package, PackageId};
use p2_render_cpu::{AntiportalActor, MeshPlacement, instance_fits};
use super::{PendingAssets, SceneAssets};
use super::mesh_sections::use_uv_stream;
use crate::actor_light::{ActorLit, LightEnvironment, MeshLighting, keep_for_relighting};
use crate::components::FollowActor;
use crate::level_collision::{GREY_ALBEDO, MaskedCaster, masked_triangles};
use crate::scene::{MaterialImages, ModelView, MovingModel, SectionView, ZoneCulled, merged_box, points_box};

const CELL: f32 = 2048.0;

pub(super) fn grid_cell(position: Vec3) -> [i32; 2] {
    [(position.x / CELL).floor() as i32, (position.z / CELL).floor() as i32]
}

#[derive(Clone, Copy)]
pub struct Placement {
    pub location: Vector,
    pub rotation: Rotator,
    pub scale: Vector,
    pub pre_pivot: Vector,
}

pub struct MeshPlacing<I> {
    pub transform: Transform,
    pub instance: Option<(I, MeshLighting)>,
    pub placement: Option<Placement>,
    pub follow: Option<FollowActor>,
    pub fixed: Option<Placement>,
    pub only_from: Option<Vec<u8>>,
}

/// The placed mesh's box in Unreal space. The engine files an actor under the zone of each BSP leaf this box
/// touches, wherever its pivot is.
pub(super) fn placed_box(local: Option<([f32; 3], [f32; 3])>, transform: &Transform) -> Option<([f32; 3], [f32; 3])> {
    let (min, max) = local?;
    let affine = transform.compute_affine();
    let corners = (0..8).map(|i: usize| {
        let pick = |axis: usize| if i >> axis & 1 == 0 { min[axis] } else { max[axis] };
        to_bevy(affine.transform_point3(Vec3::new(pick(0), pick(1), pick(2))).to_array())
    });
    points_box(corners)
}

pub struct StaticLevel<'a> {
    pub pkg: &'a Package,
    pub file: &'a [u8],
    pub model: &'a Model,
    pub environment: &'a LightEnvironment,
    pub sky_zone: Option<u8>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct BatchKey {
    material: String,
    has_uvs: bool,
    has_colors: bool,
    zones: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct StaticParts {
    pub models: Vec<ModelView>,
    pub sky_models: Vec<ModelView>,
    pub followers: Vec<MovingModel<FollowActor>>,
    pub collisions: Vec<MeshCollision>,
    pub lamp_casters: Vec<[[f32; 3]; 3]>,
    pub lamp_caster_albedos: Vec<[f32; 3]>,
    pub lamp_masked: Vec<MaskedCaster>,
    batches: BTreeMap<BatchKey, (SectionView, MeshBatch, ([f32; 3], [f32; 3]))>,
    pub lit_actors: usize,
}

#[expect(clippy::too_many_lines, reason = "one mesh's sections, its lamp casters and every way a placing of it is drawn")]
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
    let sphere = crate::actor_light::mesh_sphere(&mesh);
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
    let local_box = points_box(mesh.positions.iter().map(|&p| to_bevy(p)));
    let mut unlit: [Vec<(Transform, Option<ZoneCulled>)>; 2] = Default::default();
    let mut shared: Option<Arc<StaticMesh>> = None;
    for MeshPlacing { transform, instance, placement, follow, fixed, only_from } in placements {
        let collision = placement.map(|Placement { location, rotation, scale, pre_pivot }| MeshCollision::new(&mesh, location, rotation, scale, pre_pivot));
        let world_box = placed_box(local_box, &transform);
        let zones = world_box.map_or_else(Vec::new, |(min, max)| p2_collision::box_zones(level.model, min.into(), max.into()));
        let (in_sky, in_world) = crate::scene::sky_and_world(&zones, level.sky_zone);
        if let Some(Placement { location, rotation, scale, pre_pivot }) = fixed.filter(|_| in_world) {
            let built;
            let placed = if let Some(collision) = &collision {
                collision
            } else {
                built = MeshCollision::new(&mesh, location, rotation, scale, pre_pivot);
                &built
            };
            for (section, view) in base.iter().zip(&views) {
                if let (AlphaMode::Mask(cutoff @ ..=1.0), Some(image)) = (view.alpha_mode, &view.texture) {
                    let index = parts.lamp_masked.iter().position(|caster| caster.image.id() == image.id()).unwrap_or_else(|| {
                        parts.lamp_masked.push(MaskedCaster { image: image.clone(), cutoff, triangles: Vec::new() });
                        parts.lamp_masked.len() - 1
                    });
                    parts.lamp_masked[index].triangles.extend(masked_triangles(section, &transform));
                }
            }
            for (index, triangle) in placed.triangles().iter().enumerate() {
                let Some(view) = placed.slot(index).and_then(|slot| views.get(slot as usize)).filter(|view| view.alpha_mode == AlphaMode::Opaque) else { continue };
                parts.lamp_casters.push(*triangle);
                parts.lamp_caster_albedos.push(view.texture.as_ref().and_then(|image| t.albedo(image.id())).unwrap_or(GREY_ALBEDO));
            }
        }
        parts.collisions.extend(collision);
        let mirrored = mirrors(&transform);
        let lit = instance.map(|(reference, lighting)| {
            let export = reference.and_then(|reference| level.pkg.exports.get(usize::try_from(reference - 1).ok()?));
            let instance = export.and_then(|export| read_static_mesh_instance(level.pkg, level.file, export).ok());
            let fits = instance.as_ref().is_some_and(|instance| instance_fits(&mesh, instance));
            (level.environment.mesh_colors(level.model, &mesh, sphere, instance.as_ref(), &lighting), fits, lighting)
        });
        let (lighting, relit): (Option<_>, Option<_>) = lit.map(|(colors, fits, lighting)| (colors, (fits, lighting))).unzip();
        // A follower in both the sky and the level gets its sky copy from its zones as it is placed; drawn here too, it
        // would draw twice in the sky.
        if fixed.is_some() && in_sky && (follow.is_none() || !in_world) {
            parts.sky_models.push(ModelView { sections: sections(t, lighting.as_deref(), false, mirrored, &|_| true), instances: vec![transform], zone: None, casts: false });
            if !in_world {
                continue;
            }
        }
        if let Some(follow) = follow {
            // Without baked lighting the engine lights it by hardware lights where it is: relit as it moves
            // (movers with an instance keep it, as the engine does).
            let relight = relit.filter(|&(fits, _)| !fits).map(|(_, lighting)| {
                let mesh = shared.get_or_insert_with(|| Arc::new(mesh.clone())).clone();
                ActorLit::static_mesh(follow.actor, mesh, lighting.placement.scale, lighting.placement.pre_pivot)
                    .baked_at(lighting.placement.location, lighting.placement.rotation, lighting.glow)
            });
            parts.followers.push(MovingModel { sections: sections(t, lighting.as_deref(), relight.is_some(), mirrored, &|_| true), transform, follow, lit: relight });
            continue;
        }
        parts.lit_actors += usize::from(lighting.is_some());
        let zone = world_box.filter(|_| !level.model.leaves.is_empty()).map(|(min, max)| ZoneCulled { zones, min, max, only_from: only_from.clone() });
        if only_from.is_some() {
            parts.models.push(ModelView { sections: sections(t, lighting.as_deref(), false, mirrored, &|_| true), instances: vec![transform], zone, casts: true });
            continue;
        }
        let colors = lighting.as_deref().map(lighting_colors);
        let culled = zone.as_ref().filter(|zone| !zone.zones.is_empty());
        let reach = culled.map_or(([0.0; 3], [0.0; 3]), |zone| (zone.min, zone.max));
        for (i, section) in base.iter().enumerate().filter(|&(i, _)| !blended[i]) {
            let view = &views[i];
            let colors = colors.as_deref().filter(|_| !view.fullbright);
            let key = BatchKey {
                material: material_keys[i].clone(),
                has_uvs: section.attribute(Mesh::ATTRIBUTE_UV_0).is_some(),
                has_colors: colors.is_some(),
                zones: culled.map(|zone| zone.zones.clone()),
            };
            let (_, batch, held) = parts.batches.entry(key).or_insert_with(|| (view.clone(), MeshBatch::default(), reach));
            batch.push(section, &transform, colors);
            *held = merged_box(*held, reach);
        }
        if blended.contains(&true) {
            match lighting {
                Some(lighting) => parts.models.push(ModelView { sections: sections(t, Some(&lighting), false, mirrored, &|i| blended[i]), instances: vec![transform], zone, casts: true }),
                None => unlit[usize::from(mirrored)].push((transform, zone)),
            }
        }
    }
    for (mirrored, unlit) in unlit.into_iter().enumerate().filter(|(_, unlit)| !unlit.is_empty()) {
        let sections = sections(t, None, false, mirrored == 1, &|i| blended[i]);
        parts.models.extend(unlit.into_iter().map(|(transform, zone)| ModelView { sections: sections.clone(), instances: vec![transform], zone, casts: true }));
    }
    true
}


pub fn resolve_mesh_paths(
    game: &mut Game,
    id: PackageId,
    mesh_paths: BTreeMap<(String, Vec<Option<String>>), Vec<MeshPlacing<Option<String>>>>,
    placements: &mut BTreeMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>>,
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
            .map(|MeshPlacing { transform, instance, placement, follow, fixed, only_from }| {
                let instance = instance.map(|(path, lighting)| {
                    let reference = path.and_then(|path| export(&path)).filter(|&(p, _)| p == id).map(|(_, reference)| reference);
                    (reference, lighting)
                });
                MeshPlacing { transform, instance, placement, follow, fixed, only_from }
            })
            .collect::<Vec<_>>();
        placements.entry((mesh, skins)).or_default().extend(actors);
    }
    missing
}


pub fn antiportal_actors(game: &mut Game, model: &Model, paths: &[(String, MeshPlacement)]) -> Vec<AntiportalActor> {
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

pub fn static_models(
    game: &mut Game,
    t: &mut PendingAssets,
    texture: &mut impl FnMut(&mut Game, &mut PendingAssets, PackageId, i32) -> Option<MaterialImages>,
    level: &StaticLevel,
    placements: BTreeMap<((PackageId, i32), Vec<(PackageId, i32)>), Vec<MeshPlacing<Option<i32>>>>,
) -> (StaticParts, usize, usize) {
    let (mut parts, mut missing) = (StaticParts::default(), 0);
    for (mesh, placements) in placements {
        if !static_mesh_models(game, t, texture, level, mesh, placements, &mut parts) {
            missing += 1;
        }
    }
    let batches = std::mem::take(&mut parts.batches);
    let merged = batches.len();
    for (key, (view, batch, (min, max))) in batches {
        let mut mesh = batch.into_mesh();
        let chunks = crate::world_pass::chunks_of(&mut mesh);
        let zone = key.zones.map(|zones| ZoneCulled { zones, min, max, only_from: None });
        parts.models.push(ModelView { sections: vec![SectionView { mesh: t.add_mesh(mesh), chunks, ..view }], instances: vec![Transform::IDENTITY], zone, casts: true });
    }
    (parts, missing, merged)
}
