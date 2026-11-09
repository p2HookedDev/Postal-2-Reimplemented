use std::sync::Mutex;
use bevy::image::{Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};

const LOD_SET_LABEL: &str = "lod set ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureQuality {
    pub detail: [u8; 5],
    pub min_lod: i32,
    pub max_lod: i32,
    pub anisotropy: u16,
    pub trilinear: bool,
    pub detail_textures: bool,
}

impl Default for TextureQuality {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TextureQuality {
    const DEFAULT: Self = Self { detail: [0; 5], min_lod: 0, max_lod: i32::MAX, anisotropy: 1, trilinear: true, detail_textures: true };

    pub fn detail_level(text: &str) -> u8 {
        let text = text.trim();
        let named = ["high", "medium", "low", "ultralow"].iter().position(|name| name.eq_ignore_ascii_case(text));
        named.map_or_else(|| text.parse().unwrap_or(0).min(3), |index| index as u8)
    }

    pub fn skipped_mips(&self, lod_set: u8, mips: u32) -> u32 {
        let Some(&detail) = usize::from(lod_set).checked_sub(1).and_then(|set| self.detail.get(set)) else { return 0 };
        let mips = mips as i32;
        let mut skip = i32::from(detail).min(mips - 1);
        if mips - skip < self.min_lod {
            skip = (mips - self.min_lod).max(0);
        }
        if self.max_lod < mips - skip {
            skip = skip.max(mips - self.max_lod);
        }
        skip.clamp(0, (mips - 1).max(0)) as u32
    }

    /// `None` for an image whose sampler [`Self::sampler`] did not make.
    pub fn refreshed_sampler(&self, image: &Image) -> Option<ImageSamplerDescriptor> {
        let ImageSampler::Descriptor(now) = &image.sampler else { return None };
        let lod_set = now.label.as_deref()?.strip_prefix(LOD_SET_LABEL)?.parse().ok()?;
        let wrap = [now.address_mode_u, now.address_mode_v];
        match self.sampler(lod_set, image.texture_descriptor.mip_level_count, wrap) {
            ImageSampler::Descriptor(descriptor) => Some(descriptor),
            _ => None,
        }
    }

    /// Skipped mips become the lowest level used. `UseTrilinear` picks the mip filter, `LevelOfAnisotropy`
    /// the min and mag filter; Bevy needs every filter linear then.
    pub fn sampler(&self, lod_set: u8, mips: u32, wrap: [ImageAddressMode; 2]) -> ImageSampler {
        let anisotropy = self.anisotropy.clamp(1, 16);
        let mipmap_filter = if self.trilinear || anisotropy > 1 { ImageFilterMode::Linear } else { ImageFilterMode::Nearest };
        ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: wrap[0],
            address_mode_v: wrap[1],
            mipmap_filter,
            lod_min_clamp: self.skipped_mips(lod_set, mips) as f32,
            anisotropy_clamp: anisotropy,
            label: Some(format!("{LOD_SET_LABEL}{lod_set}")),
            ..ImageSamplerDescriptor::linear()
        })
    }
}

/// The quality new images are made with; `render::sampler_refresh` brings the ones already made up to it.
static QUALITY: Mutex<TextureQuality> = Mutex::new(TextureQuality::DEFAULT);

pub fn set_texture_quality(quality: TextureQuality) {
    *QUALITY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = quality;
}

pub fn texture_quality() -> TextureQuality {
    *QUALITY.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_picks_skipped_mips_and_filters() {
        let quality = TextureQuality { detail: [1, 2, 3, 0, 0], ..TextureQuality::default() };
        assert_eq!([(1, 8), (2, 8), (3, 8), (3, 2)].map(|(set, mips)| quality.skipped_mips(set, mips)), [1, 2, 3, 1]);
        assert_eq!([(0, 8), (9, 8), (4, 8)].map(|(set, mips)| quality.skipped_mips(set, mips)), [0, 0, 0]);
        assert_eq!(TextureQuality { min_lod: 7, ..quality }.skipped_mips(3, 8), 1);
        assert_eq!(quality.skipped_mips(p2_assets::LOD_SET_LIGHT_MAP, 2), 1);
        assert_eq!(TextureQuality { max_lod: 5, ..quality }.skipped_mips(4, 8), 3);
        assert_eq!([" low", "UltraLow", "high", "2", "x"].map(TextureQuality::detail_level), [2, 3, 0, 2, 0]);
        let wrap = [ImageAddressMode::Repeat; 2];
        let ImageSampler::Descriptor(d) = (TextureQuality { anisotropy: 8, trilinear: false, ..quality }).sampler(1, 8, wrap) else { panic!() };
        assert_eq!((d.anisotropy_clamp, d.mipmap_filter, d.lod_min_clamp), (8, ImageFilterMode::Linear, 1.0));
        let ImageSampler::Descriptor(d) = (TextureQuality { trilinear: false, ..quality }).sampler(0, 8, wrap) else { panic!() };
        assert_eq!((d.anisotropy_clamp, d.mipmap_filter), (1, ImageFilterMode::Nearest));
    }
}
