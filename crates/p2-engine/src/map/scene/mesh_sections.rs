use super::*;

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
            let chunks = if chunks == SectionChunks::Chunked { crate::render::world_pass::chunks_of(&mut section) } else { None };
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

#[derive(Clone, Default)]
pub(crate) struct VmAnimations(Option<(Arc<Loader>, p2_vm::MeshAnimations)>);

impl VmAnimations {
    pub(super) fn of(vm: &Vm<Arc<Loader>>, loader: &Arc<Loader>) -> Self {
        Self(Some((Arc::clone(loader), vm.mesh_animations())))
    }

    pub(crate) fn package(&self, name: &str, pkg: &Package, data: &[u8]) -> Arc<[Arc<MeshAnimation>]> {
        let vm = self.0.as_ref().and_then(|(loader, read)| Some((loader.loaded_id(name)?, read)));
        pkg.exports
            .iter()
            .enumerate()
            .filter(|(_, e)| pkg.object_name(e.class_index) == Some("MeshAnimation"))
            .filter_map(|(export, e)| match vm.and_then(|(id, read)| read.get(id, export)) {
                Some(read) => read,
                None => read_mesh_animation(pkg, data, e).ok().map(Arc::new),
            })
            .collect()
    }
}

/// Heads: their scripts play an animation of another package, so no sequence of its own, posed only by its actor's VM
/// channels.
pub(super) fn still_mesh(pkg: &Package, handles: Vec<Handle<Mesh>>, mesh: &Arc<SkeletalMesh>) -> AnimatedMesh {
    let none = Arc::new(MeshAnimation { version: 0, bones: Vec::new(), sequences: Vec::new() });
    AnimatedMesh::new(handles, Arc::clone(mesh), none, 0, |i| pkg.name(mesh.bones[i].name).to_owned(), |_| String::new())
        .with_aliases(|n| pkg.name(n).to_owned())
}

pub fn animate_mesh_with(
    pkg: &Package,
    animations: &[Arc<MeshAnimation>],
    handles: Vec<Handle<Mesh>>,
    mesh: &Arc<SkeletalMesh>,
    prefer: &[&str],
) -> Option<(AnimatedMesh, String)> {
    let bone_names: HashSet<String> = mesh.bones.iter().map(|b| pkg.name(b.name).to_ascii_lowercase()).collect();
    let animation = animations
        .iter()
        .max_by_key(|a| a.bones.iter().filter(|b| bone_names.contains(&pkg.name(b.name).to_ascii_lowercase())).count())?;
    let named = |part: &str| animation.sequences.iter().position(|s| pkg.name(s.name).to_ascii_lowercase().contains(part));
    let sequence = prefer.iter().find_map(|part| named(part)).unwrap_or(0);
    let name = animation.sequences.get(sequence).map(|s| pkg.name(s.name).to_owned())?;
    let item = AnimatedMesh::new(
        handles,
        Arc::clone(mesh),
        Arc::clone(animation),
        sequence,
        |i| pkg.name(mesh.bones[i].name).to_owned(),
        |i| pkg.name(animation.bones[i].name).to_owned(),
    )
    .with_aliases(|n| pkg.name(n).to_owned());
    Some((item, name))
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
    let sections = skeletal_mesh_to_meshes(mesh, gpu_skin.is_some())
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
