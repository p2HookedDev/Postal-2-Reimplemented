use std::collections::{HashMap, HashSet};

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

#[derive(Default)]
pub struct MaterialCache {
    cache: HashMap<(PackageId, i32), Option<MaterialImages>>,
    images: HashMap<TextureSource, Option<MaterialImages>>,
    details: FxHashMap<(PackageId, usize), Option<Handle<Image>>>,
    speculars: FxHashMap<SpecularSource, Option<Handle<Image>>>,
}

impl MaterialCache {
    pub fn images(&mut self, game: &mut Game, t: &mut PendingAssets, package: PackageId, material: i32) -> Option<MaterialImages> {
        let Self { cache, images, details, speculars } = self;
        cache
            .entry((package, material))
            .or_insert_with(|| {
                let (source, state) = game.material_source(package, material)?;
                let image = images.entry(source).or_insert_with(|| {
                    let SourceLayers { image, glow, wet } = game.source_layers(source, TextureDecode::Keep)?;
                    let albedo = mean_albedo(&image);
                    let handle = t.add_image(image);
                    t.albedos.insert(handle.id(), albedo);
                    let glow = glow.map(|glow| t.add_image(glow));
                    let detail = source.detail.and_then(|key| {
                        details.entry(key).or_insert_with(|| game.detail_image(source).map(|detail| t.add_image(detail))).clone()
                    });
                    let specular = source.specular.and_then(|specular| {
                        let key = SpecularSource { mask: SpecularMask::None, env_map: None, ..specular };
                        let cube = speculars.entry(key).or_insert_with(|| game.specular_image(specular).map(|cube| t.add_image(cube))).clone()?;
                        Some((cube, specular_params(source)?))
                    });
                    if let Some(wet) = wet {
                        t.wet.push((handle.id(), wet));
                    }
                    Some(MaterialImages { image: handle, glow, detail, specular, state: RenderState::default() })
                });
                Some(MaterialImages { state, ..image.clone()? })
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
    /// The switch itself ends at this image, so its surfaces take all of a new material (`restyle_switched`). A material
    /// around the switch merged its own state into the walk's: those surfaces only get the new image.
    pub whole: bool,
    pub shown: RenderState,
}

pub fn switched_images(game: &mut Game, materials: &MaterialCache) -> Vec<((PackageId, usize), SwitchImage)> {
    let MaterialCache { cache, images, .. } = materials;
    let mut users_of: FxHashMap<AssetId<Image>, Vec<(PackageId, i32, &MaterialImages)>> = FxHashMap::default();
    for (&(package, material), used) in cache {
        if let Some(used) = used {
            users_of.entry(used.image.id()).or_default().push((package, material, used));
        }
    }
    let mut switched = Vec::new();
    for (source, found) in images.iter().filter(|(source, _)| source.wet.is_none()) {
        let (Some(switch), Some(found)) = (source.switch, found) else { continue };
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
