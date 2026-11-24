use super::*;

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
