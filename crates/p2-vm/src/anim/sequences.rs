use std::sync::Arc;
use p2_assets::read_skeletal_mesh;
use crate::{LoaderRef, NameId, ObjectId, Vm, Prop};
use super::Sequence;

impl<L: LoaderRef> Vm<L> {
    pub(super) fn find_sequence_with<T>(&mut self, actor: ObjectId, name: NameId, read: impl Fn(&Sequence) -> T) -> Option<(usize, T)> {
        let instance = self.instance_mut(actor)?;
        let (mesh, unlinked) = (instance.mesh, instance.linked.is_empty());
        if unlinked && let Some(default) = self.default_anim(mesh) {
            let instance = self.instance_mut(actor)?;
            instance.linked.push(default);
            instance.rates.clear();
        }
        let count = self.media.anims.instances.get(&actor)?.linked.len();
        (0..count).find_map(|index| {
            let animation = *self.media.anims.instances.get(&actor)?.linked.get(index)?;
            let sequences = self.sequences(animation)?;
            sequences.iter().find(|s| s.name == name).map(|s| (index, read(s)))
        })
    }

    pub(super) fn sequences(&mut self, animation: ObjectId) -> Option<Arc<[Sequence]>> {
        if let Some(cached) = self.media.anims.sequences.get(&animation) {
            return cached.clone();
        }
        let phase = self.profile_start();
        let (loader, path) = (self.loader.clone(), self.store.object(animation).path.clone());
        let read = loader.loaded_export_by_path_of_class(&path, "MeshAnimation").and_then(|(id, export)| {
            let loaded = loader.package(id);
            let pkg = &loaded.package;
            let entry = pkg.exports.get(export)?;
            if !pkg.object_name(entry.class_index).is_some_and(|c| c.eq_ignore_ascii_case("MeshAnimation")) {
                return None;
            }
            let animation = self.mesh_animation_at(id, export)?;
            let sequences: Arc<[Sequence]> = animation
                .sequences
                .iter()
                .map(|s| Sequence {
                    name: self.intern(pkg.name(s.name)),
                    groups: s.groups.iter().map(|&g| self.intern(pkg.name(g))).collect(),
                    frames: s.num_frames as f32,
                    rate: s.rate,
                    has_notifies: !s.notifies.is_empty(),
                    notifies: s
                        .notifies
                        .iter()
                        .map(|n| (n.time, loader.resolve_loaded(id, n.object).and_then(|found| Some((found.package, found.export?)))))
                        .collect(),
                })
                .collect();
            Some(sequences)
        });
        self.media.anims.sequences.insert(animation, read.clone());
        self.profile_phase("load: mesh animation sequences", phase);
        read
    }

    pub(super) fn default_anim(&mut self, mesh: ObjectId) -> Option<ObjectId> {
        if let Some(&cached) = self.media.anims.default_anims.get(&mesh) {
            return cached;
        }
        let phase = self.profile_start();
        let (loader, path) = (self.loader.clone(), self.store.object(mesh).path.clone());
        let found = loader.loaded_export_by_path_of_class(&path, "SkeletalMesh").and_then(|(id, export)| {
            let loaded = loader.package(id);
            let entry = loaded.package.exports.get(export)?;
            let skeletal = read_skeletal_mesh(&loaded.package, &loaded.data, entry).ok()?;
            self.object_ref(&loaded.package, id, skeletal.default_animation)
        });
        self.media.anims.default_anims.insert(mesh, found);
        self.profile_phase("load: skeletal mesh default anim", phase);
        found
    }

    pub(crate) fn skeletal_mesh(&self, actor: ObjectId) -> Option<ObjectId> {
        let mesh = self.store.object_of(actor, Prop::Mesh)?;
        let skeletal = match self.media.anims.skeletal_meshes.get(&mesh) {
            Some(&known) => known,
            None => self.is_skeletal_mesh(mesh),
        };
        skeletal.then_some(mesh)
    }

    pub(super) fn skeletal_mesh_kept(&mut self, actor: ObjectId) -> Option<ObjectId> {
        let mesh = self.store.object_of(actor, Prop::Mesh)?;
        let skeletal = match self.media.anims.skeletal_meshes.get(&mesh) {
            Some(&known) => known,
            None => {
                let known = self.is_skeletal_mesh(mesh);
                self.media.anims.skeletal_meshes.insert(mesh, known);
                known
            }
        };
        skeletal.then_some(mesh)
    }

    /// Also true for a stub of class `Mesh` with a skeletal mesh export at its path.
    fn is_skeletal_mesh(&self, mesh: ObjectId) -> bool {
        let object = self.store.object(mesh);
        if self.store.name(object.class_name).eq_ignore_ascii_case("SkeletalMesh") {
            return true;
        }
        let general = ["Mesh", "LodMesh", "Primitive"].iter().any(|c| self.store.name(object.class_name).eq_ignore_ascii_case(c));
        general && self.loader.loaded_export_by_path_of_class(&object.path, "SkeletalMesh").is_some()
    }

    pub(crate) fn has_animation(&self, actor: ObjectId) -> bool {
        self.media.anims.instances.contains_key(&actor)
    }
}
