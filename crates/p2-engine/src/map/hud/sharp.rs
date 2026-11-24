//! Modern graphics, sharp HUD: where the canvas magnifies a texture it is drawn at a whole number of pixels per texel,
//! on whole pixels, sampling the nearest texel.

use bevy::asset::{Assets, Handle};
use bevy::image::{Image, ImageSampler};
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
                image.sampler = ImageSampler::nearest();
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
    /// A font page magnified `factor` times, drawn again from its distance field; where it cannot be, its nearest copy.
    pub(super) fn text_page(&mut self, id: egui::TextureId, factor: f32, sources: &[Handle<Image>], contexts: &mut EguiContexts, images: &mut Assets<Image>) -> egui::TextureId {
        match self.1.page(id, factor, sources, contexts, images) {
            Some(found) => found,
            None => self.texture(id, sources, contexts, images),
        }
    }
}

pub(super) fn source_of<'a>(id: egui::TextureId, sources: &'a [Handle<Image>], contexts: &EguiContexts) -> Option<&'a Handle<Image>> {
    sources.iter().find(|handle| contexts.image_id(handle.id()) == Some(id))
}

/// The length in points `texels` are drawn over: `drawn`, made a whole number of pixels per texel where it magnifies.
pub(super) fn whole_pixels(drawn: f32, texels: f32, per_point: f32) -> f32 {
    let factor = drawn * per_point / texels;
    if texels <= 0.0 || factor < 1.0 {
        return drawn;
    }
    factor.round() * texels / per_point
}

pub(super) fn on_pixel(at: egui::Pos2, per_point: f32) -> egui::Pos2 {
    egui::pos2((at.x * per_point).round() / per_point, (at.y * per_point).round() / per_point)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnified_textures_take_whole_pixels_per_texel() {
        assert_eq!(whole_pixels(1.5, 1.0, 1.0), 2.0);
        assert_eq!(whole_pixels(1.4, 1.0, 1.0), 1.0);
        assert_eq!(whole_pixels(0.5, 1.0, 1.0), 0.5);
        assert_eq!(whole_pixels(64.0, 32.0, 1.5), 32.0 * 3.0 / 1.5);
        assert_eq!(whole_pixels(10.0, 0.0, 1.0), 10.0);
    }

    #[test]
    fn places_land_on_whole_pixels() {
        assert_eq!(on_pixel(egui::pos2(10.3, 4.6), 2.0), egui::pos2(10.5, 4.5));
    }
}
