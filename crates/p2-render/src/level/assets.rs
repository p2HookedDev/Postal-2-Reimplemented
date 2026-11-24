use std::collections::HashMap;

use bevy::asset::{AssetHandleProvider, AssetId, Assets, Handle};
use bevy::image::Image;
use bevy::math::UVec2;
use bevy::mesh::Mesh;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use p2_assets::Blending;
use p2_bevy_assets::library::{Game, export_reference};

use crate::wet::WetTextures;

pub trait SceneAssets {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh>;
    fn add_image(&mut self, image: Image) -> Handle<Image>;
    fn add_bindposes(&mut self, bindposes: SkinnedMeshInverseBindposes) -> Handle<SkinnedMeshInverseBindposes>;
    fn image_size(&self, image: &Handle<Image>) -> Option<UVec2>;
}

pub struct PendingAssets {
    mesh_ids: AssetHandleProvider,
    image_ids: AssetHandleProvider,
    bindpose_ids: AssetHandleProvider,
    pub(super) meshes: Vec<(Handle<Mesh>, Mesh)>,
    pub(super) images: Vec<(Handle<Image>, Image)>,
    image_sizes: HashMap<AssetId<Image>, UVec2>,
    bindposes: Vec<(Handle<SkinnedMeshInverseBindposes>, SkinnedMeshInverseBindposes)>,
    pub wet: Vec<(AssetId<Image>, p2_bevy_assets::library::WetSource)>,
    pub(super) albedos: HashMap<AssetId<Image>, [f32; 3]>,
}

impl PendingAssets {
    pub fn new(meshes: &Assets<Mesh>, images: &Assets<Image>, bindposes: &Assets<SkinnedMeshInverseBindposes>) -> Self {
        Self {
            mesh_ids: meshes.get_handle_provider(),
            image_ids: images.get_handle_provider(),
            bindpose_ids: bindposes.get_handle_provider(),
            meshes: Vec::new(),
            images: Vec::new(),
            image_sizes: HashMap::new(),
            bindposes: Vec::new(),
            wet: Vec::new(),
            albedos: HashMap::new(),
        }
    }

    pub fn add_image_as(&mut self, handle: Handle<Image>, image: Image) -> Handle<Image> {
        self.image_sizes.insert(handle.id(), image.size());
        self.images.push((handle.clone(), image));
        handle
    }

    pub fn insert(
        self,
        meshes: &mut Assets<Mesh>,
        images: &mut Assets<Image>,
        bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
        wet: &mut WetTextures,
    ) {
        for (image, source) in self.wet {
            wet.add(image, source);
        }
        let failed = self.meshes.into_iter().map(|(handle, mesh)| meshes.insert(handle.id(), mesh).is_err())
            .chain(self.images.into_iter().map(|(handle, image)| images.insert(handle.id(), image).is_err()))
            .chain(self.bindposes.into_iter().map(|(handle, pose)| bindposes.insert(handle.id(), pose).is_err()))
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
pub struct ActorTexture {
    pub image: Handle<Image>,
    pub texels: f32,
    pub blending: Option<Blending>,
}

impl ActorTexture {
    pub fn read(game: &mut Game, path: &str, add: impl FnOnce(Image) -> Handle<Image>) -> Option<Self> {
        let (package, export) = game.export_by_path(path)?;
        let texture = game.material_texture(package, export_reference(export))?;
        let (texels, blending) = (texture.image.size().x as f32, texture.state.blending);
        Some(Self { image: add(texture.image), texels, blending })
    }
}
