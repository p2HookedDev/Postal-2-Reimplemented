//! Modern graphics, sharp HUD: where the canvas magnifies a texture it is drawn at a whole number of pixels per texel,
//! on whole pixels, sampling the nearest texel.

use bevy::asset::{Assets, Handle};
use bevy::image::{Image, ImageFilterMode};
use bevy_egui::{EguiContexts, EguiTextureHandle, egui};
use rustc_hash::FxHashMap;

/// Copies of the HUD's images that sample the nearest texel, made the first time one is drawn magnified; an image egui
/// has no source for (a movie) is drawn as it is.
#[derive(Default)]
pub(in crate::map) struct NearestImages(FxHashMap<egui::TextureId, (egui::TextureId, Option<Handle<Image>>)>, super::distance::DistancePages);

impl NearestImages {
    pub(super) fn texture(&mut self, id: egui::TextureId, sources: &[Handle<Image>], contexts: &mut EguiContexts, images: &mut Assets<Image>) -> egui::TextureId {
        if let Some((found, _)) = self.0.get(&id) {
            return *found;
        }
        let source = source_of(id, sources, contexts);
        let copy = source.and_then(|handle| images.get(handle)).filter(|image| image.data.is_some()).cloned();
        let (found, kept) = match copy {
            Some(mut image) => {
                let sampler = image.sampler.get_or_init_descriptor();
                (sampler.mag_filter, sampler.min_filter) = (ImageFilterMode::Nearest, ImageFilterMode::Nearest);
                let handle = images.add(image);
                (contexts.add_image(EguiTextureHandle::Weak(handle.id())), Some(handle))
            }
            None => (id, None),
        };
        self.0.insert(id, (found, kept));
        found
    }
}

impl NearestImages {
    /// A font page magnified `factor` times, drawn again from its distance field; where it cannot be, the page as it is.
    pub(super) fn text_page(&mut self, id: egui::TextureId, factor: f32, sources: &[Handle<Image>], contexts: &mut EguiContexts, images: &mut Assets<Image>) -> egui::TextureId {
        self.1.page(id, factor, sources, contexts, images).unwrap_or(id)
    }
}

pub(super) fn source_of<'a>(id: egui::TextureId, sources: &'a [Handle<Image>], contexts: &EguiContexts) -> Option<&'a Handle<Image>> {
    sources.iter().find(|handle| contexts.image_id(handle.id()) == Some(id))
}

/// Whether `pixels` over `texels` is a whole number of pixels per texel, one or more: only then does sampling the nearest
/// texel keep every texel the same size.
pub(super) fn whole_per_texel(pixels: f32, texels: f32) -> bool {
    let factor = pixels / texels;
    texels > 0.0 && factor >= 1.0 - WHOLE && (factor - factor.round()).abs() < WHOLE
}

const WHOLE: f32 = 1e-3;

pub(super) fn on_pixel(at: egui::Pos2, per_point: f32) -> egui::Pos2 {
    egui::pos2((at.x * per_point).round() / per_point, (at.y * per_point).round() / per_point)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_whole_pixels_per_texel_sample_the_nearest() {
        assert!(whole_per_texel(64.0, 32.0));
        assert!(whole_per_texel(32.0, 32.0));
        assert!(!whole_per_texel(480.0, 256.0));
        assert!(!whole_per_texel(16.0, 32.0));
        assert!(!whole_per_texel(10.0, 0.0));
        assert!(!whole_per_texel(-64.0, 32.0));
    }

    #[test]
    fn places_land_on_whole_pixels() {
        assert_eq!(on_pixel(egui::pos2(10.3, 4.6), 2.0), egui::pos2(10.5, 4.5));
    }
}
