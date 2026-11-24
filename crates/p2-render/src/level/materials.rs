use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hash};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use bevy::asset::{AssetId, Handle};
use bevy::image::Image;
use bevy::math::Vec2;
use p2_assets::{FinalBlend, RenderState};
use p2_bevy_assets::library::{Game, SourceLayers, export_reference, SpecularMask, SpecularSource, TextureDecode, TextureSource, specular_params};
use p2_package::PackageId;
use rustc_hash::FxHashMap;

use super::{PendingAssets, SceneAssets};
use crate::level_collision::mean_albedo;
use crate::scene::{MaterialImages, ModelView};

/// Shared by the threads that build a scene: each entry is made once, by the first thread that asks; another
/// thread asking meanwhile waits for it. Entries only point down (material, source, detail or specular), so no
/// two waits cross.
#[derive(Default)]
pub struct MaterialCache {
    cache: Mutex<HashMap<(PackageId, i32), Arc<OnceLock<Option<MaterialImages>>>>>,
    images: Mutex<HashMap<TextureSource, Arc<OnceLock<Option<MaterialImages>>>>>,
    details: Mutex<FxHashMap<(PackageId, usize), Arc<OnceLock<Option<Handle<Image>>>>>>,
    speculars: Mutex<FxHashMap<SpecularSource, Arc<OnceLock<Option<Handle<Image>>>>>>,
}

fn entry_cell<K: Hash + Eq, V, S: BuildHasher>(map: &Mutex<HashMap<K, Arc<OnceLock<V>>, S>>, key: K) -> Arc<OnceLock<V>> {
    Arc::clone(map.lock().unwrap_or_else(PoisonError::into_inner).entry(key).or_default())
}

impl MaterialCache {
    pub fn images(&self, game: &mut Game, t: &mut PendingAssets, package: PackageId, material: i32) -> Option<MaterialImages> {
        entry_cell(&self.cache, (package, material))
            .get_or_init(|| {
                let (source, state) = game.material_source(package, material)?;
                let image = entry_cell(&self.images, source)
                    .get_or_init(|| {
                        let SourceLayers { image, glow, wet } = game.source_layers(source, TextureDecode::Keep)?;
                        let albedo = mean_albedo(&image);
                        let handle = t.add_image(image);
                        t.set_albedo(handle.id(), albedo);
                        let glow = glow.map(|glow| t.add_image(glow));
                        let detail = source.detail.and_then(|key| {
                            entry_cell(&self.details, key).get_or_init(|| game.detail_image(source).map(|detail| t.add_image(detail))).clone()
                        });
                        let specular = source.specular.and_then(|specular| {
                            let key = SpecularSource { mask: SpecularMask::None, env_map: None, ..specular };
                            let cube = entry_cell(&self.speculars, key).get_or_init(|| game.specular_image(specular).map(|cube| t.add_image(cube))).clone()?;
                            Some((cube, specular_params(source)?))
                        });
                        if let Some(wet) = wet {
                            t.wet.push((handle.id(), wet));
                        }
                        Some(MaterialImages { image: handle, glow, detail, specular, state: RenderState::default() })
                    })
                    .clone();
                Some(MaterialImages { state, ..image? })
            })
            .clone()
    }
}

pub fn material_images<S: SceneAssets>(game: &mut Game, t: &mut S, package: PackageId, material: i32) -> Option<MaterialImages> {
    let texture = game.material_texture(package, material)?;
    Some(MaterialImages { image: t.add_image(texture.image), glow: texture.glow.map(|glow| t.add_image(glow)), detail: texture.detail.map(|detail| t.add_image(detail)), specular: texture.specular.map(|(cube, params)| (t.add_image(cube), params)), state: texture.state })
}

pub fn projector_image(game: &mut Game, add: impl FnOnce(Image) -> Handle<Image>, texture: &str) -> Option<(Handle<Image>, Vec2, Option<FinalBlend>)> {
    let (package, export) = game.export_by_path(texture)?;
    let texture = game.material_texture(package, export_reference(export))?;
    let size = texture.image.size().as_vec2();
    Some((add(texture.image), size, texture.state.final_blend))
}

pub struct SwitchImage {
    pub image: AssetId<Image>,
    /// The switch ends at this image, so its surfaces take all of a new material. A material around the switch
    /// merged its own state into the walk, so those surfaces only get the new image.
    pub whole: bool,
    pub shown: RenderState,
}

pub fn switched_images(game: &mut Game, materials: &MaterialCache) -> Vec<((PackageId, usize), SwitchImage)> {
    let cache = materials.cache.lock().unwrap_or_else(PoisonError::into_inner);
    let images = materials.images.lock().unwrap_or_else(PoisonError::into_inner);
    let mut users_of: FxHashMap<AssetId<Image>, Vec<(PackageId, i32, &MaterialImages)>> = FxHashMap::default();
    for (&(package, material), used) in cache.iter() {
        if let Some(Some(used)) = used.get() {
            users_of.entry(used.image.id()).or_default().push((package, material, used));
        }
    }
    let mut switched = Vec::new();
    for (source, found) in images.iter().filter(|(source, _)| source.wet.is_none()) {
        let (Some(switch), Some(Some(found))) = (source.switch, found.get()) else { continue };
        let image = found.image.id();
        let users = users_of.get(&image).map_or(&[][..], Vec::as_slice);
        let whole = !users.is_empty() && users.iter().all(|&(package, material, _)| game.export_of(package, material) == Some(switch));
        if whole || (source.combine.is_none() && source.opacity.is_none() && source.glow.is_none()) {
            let shown = users.first().map_or_else(RenderState::default, |(_, _, used)| used.state.clone());
            switched.push((switch, SwitchImage { image, whole, shown }));
        }
    }
    switched
}

pub fn keep_restyled_out_of_world_pass<'a>(models: &mut [ModelView], switches: impl Iterator<Item = &'a SwitchImage>) {
    let restyled: HashSet<AssetId<Image>> = switches.filter(|switch| switch.whole).map(|switch| switch.image).collect();
    for section in models.iter_mut().flat_map(|model| &mut model.sections) {
        if section.texture.as_ref().is_some_and(|texture| restyled.contains(&texture.id())) {
            section.chunks = None;
        }
    }
}
