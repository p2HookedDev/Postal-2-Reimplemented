use bevy::asset::Handle;
use bevy::mesh::Mesh;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use p2_assets::{SkeletalMesh, StaticMesh, read_skeletal_mesh};
use p2_bevy_assets::library::{Game, export_reference};
use p2_bevy_assets::{character_smoothing, skeletal_mesh_to_meshes, skin_bind, smoothed_skeletal_mesh, static_mesh_to_meshes};
use p2_package::PackageId;

use super::SceneAssets;
use crate::world::scene::{MaterialImages, SectionView, Skin};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SectionChunks {
    Whole,
    Chunked,
}

pub fn sections_of_static_mesh<S: SceneAssets>(mesh: &StaticMesh, t: &mut S, chunks: SectionChunks, mut texture: impl FnMut(&mut S, usize) -> Option<MaterialImages>) -> Vec<SectionView> {
    static_mesh_to_meshes(mesh)
        .into_iter()
        .enumerate()
        .map(|(i, mut section)| {
            let found = texture(t, i);
            use_uv_stream(&mut section, mesh, found.as_ref());
            let chunks = if chunks == SectionChunks::Chunked { crate::world::world_pass::chunks_of(&mut section) } else { None };
            SectionView { chunks, ..SectionView::textured(t.add_mesh(section), found) }
        })
        .collect()
}

pub(super) fn use_uv_stream(section: &mut Mesh, mesh: &StaticMesh, found: Option<&MaterialImages>) {
    let stream = found.and_then(|found| found.state.uv_stream).filter(|&stream| stream > 0);
    if let Some(uvs) = stream.and_then(|stream| mesh.uvs.get(usize::from(stream))) {
        section.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone());
    }
}

pub fn skeletal_sections<S: SceneAssets>(
    game: &mut Game,
    t: &mut S,
    texture: &mut impl FnMut(&mut Game, &mut S, PackageId, i32) -> Option<MaterialImages>,
    path: &str,
    skins: &[Option<String>],
) -> Option<(SkeletalMesh, PackageId, Vec<SectionView>, Vec<Handle<Mesh>>)> {
    let (mesh_id, mesh_export) = game.export_by_path_of_class(path, "SkeletalMesh")?;
    let mesh = {
        let (pkg, file) = game.data(mesh_id);
        let export = &pkg.exports[mesh_export];
        (pkg.object_name(export.class_index) == Some("SkeletalMesh")).then(|| read_skeletal_mesh(pkg, file, export).ok()).flatten()?
    };
    let (sections, handles) = skeletal_mesh_sections(game, t, texture, &mesh, mesh_id, skins, true);
    Some((mesh, mesh_id, sections, handles))
}

const MAX_JOINTS: usize = 256;

pub fn skeletal_draw_meshes(mesh: &SkeletalMesh, gpu_skin: bool, smoothing: u8) -> Vec<(Mesh, u16)> {
    let smoothed = (gpu_skin && smoothing > 0).then(|| smoothed_skeletal_mesh(mesh)).flatten();
    skeletal_mesh_to_meshes(smoothed.as_ref().unwrap_or(mesh), gpu_skin)
}

pub fn skeletal_mesh_sections<S: SceneAssets>(
    game: &mut Game,
    t: &mut S,
    texture: &mut impl FnMut(&mut Game, &mut S, PackageId, i32) -> Option<MaterialImages>,
    mesh: &SkeletalMesh,
    mesh_id: PackageId,
    skins: &[Option<String>],
    gpu_skin: bool,
) -> (Vec<SectionView>, Vec<Handle<Mesh>>) {
    let mut handles = Vec::new();
    let gpu_skin = (gpu_skin && mesh.bones.len() <= MAX_JOINTS).then(|| {
        let (inverse, bind) = skin_bind(mesh);
        Skin { inverse_bindposes: t.add_bindposes(SkinnedMeshInverseBindposes::from(inverse)), bind: bind.into() }
    });
    let sections = skeletal_draw_meshes(mesh, gpu_skin.is_some(), character_smoothing())
        .into_iter()
        .map(|(section, material)| {
            let handle = t.add_mesh(section);
            handles.push(handle.clone());
            let skin = skins.get(usize::from(material)).cloned().flatten();
            let found = match skin.and_then(|skin| game.export_by_path(&skin)) {
                Some((package, export)) => texture(game, t, package, export_reference(export)),
                None => mesh
                    .materials
                    .get(usize::from(material))
                    .or(mesh.materials.first())
                    .and_then(|&material| texture(game, t, mesh_id, material)),
            };
            SectionView { no_scene_lights: true, skin: gpu_skin.clone(), ..SectionView::textured(handle, found) }
        })
        .collect();
    (sections, handles)
}
