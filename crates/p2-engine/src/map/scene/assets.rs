use super::*;

pub trait SceneAssets {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh>;
    fn add_image(&mut self, image: Image) -> Handle<Image>;
    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes>;
    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2>;
}

impl SceneAssets for AssetStores<'_> {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh> {
        self.meshes.add(mesh)
    }

    fn add_image(&mut self, image: Image) -> Handle<Image> {
        self.images.add(image)
    }

    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes> {
        self.bindposes.add(bindposes)
    }

    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2> {
        self.images.get(image).map(Image::size)
    }
}

pub struct PendingAssets {
    mesh_ids: AssetHandleProvider,
    image_ids: AssetHandleProvider,
    bindpose_ids: AssetHandleProvider,
    pub(super) meshes: Vec<(Handle<Mesh>, Mesh)>,
    pub(super) images: Vec<(Handle<Image>, Image)>,
    image_sizes: HashMap<AssetId<Image>, UVec2>,
    bindposes: Vec<(Handle<SkinnedMeshInverseBindposes>, SkinnedMeshInverseBindposes)>,
    pub wet: Vec<(AssetId<Image>, crate::library::WetSource)>,
}

impl PendingAssets {
    pub fn new(t: &SceneTargets) -> Self {
        Self {
            mesh_ids: t.assets.meshes.get_handle_provider(),
            image_ids: t.assets.images.get_handle_provider(),
            bindpose_ids: t.assets.bindposes.get_handle_provider(),
            meshes: Vec::new(),
            images: Vec::new(),
            image_sizes: HashMap::new(),
            bindposes: Vec::new(),
            wet: Vec::new(),
        }
    }

    pub(crate) fn add_image_as(&mut self, handle: Handle<Image>, image: Image) -> Handle<Image> {
        self.image_sizes.insert(handle.id(), image.size());
        self.images.push((handle.clone(), image));
        handle
    }

    pub fn insert(self, t: &mut SceneTargets) {
        for (image, source) in self.wet {
            t.assets.wet.add(image, source);
        }
        let failed = self.meshes.into_iter().map(|(handle, mesh)| t.assets.meshes.insert(handle.id(), mesh).is_err())
            .chain(self.images.into_iter().map(|(handle, image)| t.assets.images.insert(handle.id(), image).is_err()))
            .chain(self.bindposes.into_iter().map(|(handle, bindposes)| t.assets.bindposes.insert(handle.id(), bindposes).is_err()))
            .filter(|&failed| failed)
            .count();
        if failed > 0 {
            bevy::log::warn!("{failed} scene assets lost their handles");
        }
    }
}

impl SceneAssets for PendingAssets {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh> {
        let handle = self.mesh_ids.reserve_handle().typed::<Mesh>();
        self.meshes.push((handle.clone(), mesh));
        handle
    }

    fn add_image(&mut self, image: Image) -> Handle<Image> {
        let handle = self.image_ids.reserve_handle().typed::<Image>();
        self.add_image_as(handle, image)
    }

    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes> {
        let handle = self.bindpose_ids.reserve_handle().typed::<SkinnedMeshInverseBindposes>();
        self.bindposes.push((handle.clone(), bindposes));
        handle
    }

    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2> {
        self.image_sizes.get(&image.id()).copied()
    }
}

#[derive(Clone)]
pub(crate) struct ActorTexture {
    pub(crate) image: Handle<Image>,
    pub(crate) texels: f32,
    pub(crate) blending: Option<Blending>,
}

impl ActorTexture {
    pub(crate) fn read(game: &mut Game, path: &str, add: impl FnOnce(Image) -> Handle<Image>) -> Option<Self> {
        let (package, export) = game.export_by_path(path)?;
        let texture = game.material_texture(package, export_reference(export))?;
        let (texels, blending) = (texture.image.size().x as f32, texture.state.blending);
        Some(Self { image: add(texture.image), texels, blending })
    }
}
