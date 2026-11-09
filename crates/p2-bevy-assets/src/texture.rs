use std::sync::atomic::{AtomicBool, Ordering};
use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use p2_assets::{Texture, TextureFormat};
use wgpu_types::{Extent3d, TextureDimension, TextureFormat as GpuFormat};
use crate::quality::texture_quality;

static BLOCK_COMPRESSION: AtomicBool = AtomicBool::new(true);

pub fn set_block_compression(supported: bool) {
    BLOCK_COMPRESSION.store(supported, Ordering::Relaxed);
}

pub fn texture_to_image(texture: &Texture, palette: Option<&[[u8; 4]]>) -> Option<Image> {
    let compression = if BLOCK_COMPRESSION.load(Ordering::Relaxed) { Compression::Block } else { Compression::Off };
    texture_image(texture, palette, compression)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Off,
    Block,
}

pub fn texture_image(texture: &Texture, palette: Option<&[[u8; 4]]>, compression: Compression) -> Option<Image> {
    let compressed = compression == Compression::Block;
    let base = texture.mips.first()?;
    let (format, block) = match texture.format {
        TextureFormat::Dxt1 if compressed => (GpuFormat::Bc1RgbaUnormSrgb, 4),
        TextureFormat::Dxt3 if compressed => (GpuFormat::Bc2RgbaUnormSrgb, 4),
        TextureFormat::Dxt5 if compressed => (GpuFormat::Bc3RgbaUnormSrgb, 4),
        TextureFormat::Dxt1 | TextureFormat::Dxt3 | TextureFormat::Dxt5 => (GpuFormat::Rgba8UnormSrgb, 4),
        TextureFormat::Rgba8 => (GpuFormat::Bgra8UnormSrgb, 1),
        TextureFormat::P8 | TextureFormat::G16 => (GpuFormat::Rgba8UnormSrgb, 1),
        _ => return None,
    };
    if base.width % block != 0 || base.height % block != 0 {
        return None;
    }
    let palette = match texture.format {
        TextureFormat::P8 => palette?,
        _ => &[],
    };

    let passthrough = match texture.format {
        TextureFormat::Rgba8 => true,
        TextureFormat::Dxt1 | TextureFormat::Dxt3 | TextureFormat::Dxt5 => compressed,
        _ => false,
    };
    let mips = texture.mips.iter().take(32);
    let mut data = Vec::with_capacity(mips.clone().map(|mip| if passthrough { mip.data.len() } else { mip.width as usize * mip.height as usize * 4 }).sum());
    let mut levels = 0;
    for (level, mip) in mips.enumerate() {
        let expected = ((base.width >> level).max(1), (base.height >> level).max(1));
        if mip.data.is_empty() || (mip.width, mip.height) != expected || mip.data.len() < texture.format.mip_bytes(mip.width, mip.height) {
            break;
        }
        if passthrough {
            data.extend_from_slice(&mip.data);
        } else {
            let Some((_, _, texels)) = p2_assets::mip_rgba(texture, level, Some(palette)) else { break };
            data.extend(texels.iter().flatten());
        }
        levels += 1;
    }
    if levels == 0 {
        return None;
    }
    let sampler = texture_quality().sampler(texture.lod_set, levels, [ImageAddressMode::Repeat; 2]);
    Some(image_2d([base.width, base.height, 1], format, (data, levels), sampler))
}

pub(crate) fn image_2d([width, height, layers]: [u32; 3], format: GpuFormat, (data, levels): (Vec<u8>, u32), sampler: ImageSampler) -> Image {
    let size = Extent3d { width, height, depth_or_array_layers: layers };
    let mut image = Image::new_uninit(size, TextureDimension::D2, format, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.mip_level_count = levels;
    image.data = Some(data);
    image.sampler = sampler;
    image
}

pub fn texture_with_opacity(
    texture: &Texture,
    palette: Option<&[[u8; 4]]>,
    opacity: &Texture,
    opacity_palette: Option<&[[u8; 4]]>,
) -> Option<Image> {
    let (width, height, mut texels) = p2_assets::texture_rgba(texture, palette)?;
    let (mask_width, mask_height, mask) = p2_assets::texture_rgba(opacity, opacity_palette)?;
    for (index, texel) in texels.iter_mut().enumerate() {
        let (x, y) = (index as u32 % width, index as u32 / width);
        let (mx, my) = (x * mask_width / width, y * mask_height / height);
        texel[3] = mask[(my * mask_width + mx) as usize][3];
    }
    Some(rgba_image(width, height, texels))
}

pub fn texture_combined(
    ops: &p2_assets::CombineOps,
    material1: (u32, u32, &[[u8; 4]]),
    material2: (u32, u32, &[[u8; 4]]),
    mask: Option<(u32, u32, &[[u8; 4]])>,
) -> Image {
    let (width, height) = if material2.0 * material2.1 > material1.0 * material1.1 { (material2.0, material2.1) } else { (material1.0, material1.1) };
    let at = |(w, h, texels): (u32, u32, &[[u8; 4]]), x: u32, y: u32| texels[((y * h / height) * w + x * w / width) as usize];
    let texels = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            ops.texel(at(material1, x, y), at(material2, x, y), mask.map(|m| at(m, x, y)))
        })
        .collect();
    rgba_image(width, height, texels)
}

pub fn texture_with_constant_alpha(texture: &Texture, palette: Option<&[[u8; 4]]>, alpha: u8) -> Option<Image> {
    let (width, height, mut texels) = p2_assets::texture_rgba(texture, palette)?;
    for texel in &mut texels {
        texel[3] = alpha;
    }
    Some(rgba_image(width, height, texels))
}

pub fn alpha_on_black_image(texture: &Texture, palette: Option<&[[u8; 4]]>) -> Option<Image> {
    let (width, height, mut texels) = p2_assets::texture_rgba(texture, palette)?;
    for texel in &mut texels {
        *texel = [0, 0, 0, texel[3]];
    }
    Some(rgba_image(width, height, texels))
}

/// Bakes masked self-illumination: base = `diffuse` × (1 − m), glow = `glow` × m, m = `mask` alpha.
/// Lit base plus glow equals the driver's lerp(lit diffuse, glow, m).
pub fn texture_glow(diffuse: (u32, u32, &[[u8; 4]]), glow: (u32, u32, &[[u8; 4]]), mask: (u32, u32, &[[u8; 4]])) -> (Image, Image) {
    let (width, height) = [diffuse, glow, mask].into_iter().map(|(w, h, _)| (w, h)).max_by_key(|&(w, h)| w * h).unwrap_or((1, 1));
    let at = |(w, h, texels): (u32, u32, &[[u8; 4]]), x: u32, y: u32| texels[((y * h / height) * w + x * w / width) as usize];
    let (base, lit): (Vec<_>, Vec<_>) = (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            glow_texel(at(diffuse, x, y), at(glow, x, y), at(mask, x, y)[3])
        })
        .unzip();
    (rgba_image(width, height, base), rgba_image(width, height, lit))
}

fn glow_texel(diffuse: [u8; 4], glow: [u8; 4], m: u8) -> ([u8; 4], [u8; 4]) {
    let scale = p2_math::mul_byte_round;
    let [r, g, b, _] = diffuse;
    let [gr, gg, gb, _] = glow;
    ([scale(r, 255 - m), scale(g, 255 - m), scale(b, 255 - m), m], [scale(gr, m), scale(gg, m), scale(gb, m), m])
}

pub fn cube_image(faces: [(u32, u32, &[[u8; 4]]); 6]) -> Image {
    let (width, height) = (faces[0].0.max(1), faces[0].1.max(1));
    let mut data = Vec::with_capacity((width * height * 24) as usize);
    for (w, h, texels) in faces {
        for i in 0..width * height {
            let (x, y) = (i % width, i / width);
            data.extend(texels.get(((y * h / height) * w + x * w / width) as usize).copied().unwrap_or([0; 4]));
        }
    }
    let mut image = image_2d([width, height, 6], GpuFormat::Rgba8UnormSrgb, (data, 1), ImageSampler::Descriptor(ImageSamplerDescriptor::linear()));
    image.texture_view_descriptor =
        Some(wgpu_types::TextureViewDescriptor { dimension: Some(wgpu_types::TextureViewDimension::Cube), ..Default::default() });
    image
}

fn rgba_image(width: u32, height: u32, texels: Vec<[u8; 4]>) -> Image {
    let (data, levels) = with_mips(width, height, texels);
    let sampler = texture_quality().sampler(0, levels, [ImageAddressMode::Repeat; 2]);
    image_2d([width, height, 1], GpuFormat::Rgba8UnormSrgb, (data, levels), sampler)
}

pub(crate) fn with_mips<const N: usize>(width: u32, height: u32, mut texels: Vec<[u8; N]>) -> (Vec<u8>, u32) {
    let (mut w, mut h) = (width, height);
    let mut data = Vec::with_capacity(texels.len() * N * 4 / 3 + N);
    data.extend(texels.iter().flatten());
    let mut levels = 1;
    while w > 1 && h > 1 && w % 2 == 0 && h % 2 == 0 {
        let previous = texels;
        let (nw, nh) = (w / 2, h / 2);
        texels = (0..nw * nh)
            .map(|i| {
                let (x, y) = (i % nw * 2, i / nw * 2);
                let at = |dx: u32, dy: u32, c: usize| previous.get(((y + dy) * w + x + dx) as usize).map_or(0, |texel| u32::from(texel[c]));
                std::array::from_fn(|c| ((at(0, 0, c) + at(1, 0, c) + at(0, 1, c) + at(1, 1, c) + 2) / 4) as u8)
            })
            .collect();
        data.extend(texels.iter().flatten());
        (w, h) = (nw, nh);
        levels += 1;
    }
    (data, levels)
}

/// Colour premultiplied by alpha (in gamma space), as egui blends (`One`, `OneMinusSrcAlpha`); straight alpha would add
/// the colour of transparent texels. `additive`: the colour as is with zero alpha, which egui adds. Box-filtered mips and
/// a trilinear sampler: a 512-pixel weapon icon drawn at ~40 pixels does not sparkle.
pub fn texture_to_ui_image(texture: &Texture, palette: Option<&[[u8; 4]]>, additive: bool) -> Option<Image> {
    let (width, height, texels) = p2_assets::texture_rgba(texture, palette)?;
    let texels = texels
        .iter()
        .map(|&[r, g, b, a]| {
            if additive {
                return [r, g, b, 0];
            }
            let premultiply = |c: u8| p2_math::mul_byte_round(c, a);
            [premultiply(r), premultiply(g), premultiply(b), a]
        })
        .collect();
    let mut image = rgba_image(width, height, texels);
    let mode = |clamp: bool| if clamp { ImageAddressMode::ClampToEdge } else { ImageAddressMode::Repeat };
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: mode(texture.clamp[0]),
        address_mode_v: mode(texture.clamp[1]),
        mipmap_filter: ImageFilterMode::Linear,
        ..ImageSamplerDescriptor::linear()
    });
    Some(image)
}

#[cfg(test)]
mod tests {
    use p2_assets::Mip;

    use super::*;

    fn mip(width: u32, height: u32, data: Vec<u8>) -> Mip {
        Mip { width, height, data }
    }

    #[test]
    fn opacity_replaces_alpha() {
        let rgba8 = |width, height, data| Texture { format: TextureFormat::Rgba8, palette: None, mips: vec![mip(width, height, data)], masked: false, clamp: [false; 2], lod_set: 0 };
        let texture = rgba8(2, 2, [[0, 0, 200, 255], [0, 0, 100, 255], [0, 0, 200, 255], [0, 0, 100, 255]].concat());
        let opacity = rgba8(1, 1, vec![9, 9, 9, 40]);
        let image = texture_with_opacity(&texture, None, &opacity, None).unwrap();
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        let data = image.data.unwrap();
        assert_eq!(&data[..8], [200, 0, 0, 40, 100, 0, 0, 40]);
        assert_eq!(&data[16..], [150, 0, 0, 40]);
    }

    #[test]
    fn constant_alpha_replaces_alpha() {
        let texture = Texture { format: TextureFormat::Rgba8, palette: None, mips: vec![mip(1, 1, vec![10, 20, 30, 77])], masked: false, clamp: [false; 2], lod_set: 0 };
        assert_eq!(texture_with_constant_alpha(&texture, None, 128).unwrap().data.unwrap(), [30, 20, 10, 128]);
    }

    #[test]
    fn alpha_on_black() {
        let texture = Texture { format: TextureFormat::Rgba8, palette: None, mips: vec![mip(1, 1, vec![10, 20, 30, 77])], masked: false, clamp: [false; 2], lod_set: 0 };
        assert_eq!(alpha_on_black_image(&texture, None).unwrap().data.unwrap(), [0, 0, 0, 77]);
    }

    #[test]
    fn glow_splits_by_mask() {
        assert_eq!(glow_texel([200, 100, 50, 255], [10, 20, 30, 0], 0), ([200, 100, 50, 0], [0, 0, 0, 0]));
        assert_eq!(glow_texel([200, 100, 50, 255], [10, 20, 30, 0], 255), ([0, 0, 0, 255], [10, 20, 30, 255]));
        assert_eq!(glow_texel([200, 0, 0, 255], [100, 0, 0, 0], 128), ([100, 0, 0, 128], [50, 0, 0, 128]));
        let (base, glow) = texture_glow((1, 1, &[[200, 0, 0, 255]]), (2, 1, &[[0, 0, 0, 0], [255, 0, 0, 0]]), (1, 1, &[[0, 0, 0, 255]]));
        assert_eq!(base.data.unwrap()[..8], [0, 0, 0, 255, 0, 0, 0, 255]);
        assert_eq!(glow.data.unwrap()[..8], [0, 0, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn p8_uses_palette() {
        let mut palette = vec![[0; 4]; 256];
        palette[1] = [10, 20, 30, 255];
        let texture = Texture {
            format: TextureFormat::P8,
            palette: Some(1),
            mips: vec![mip(2, 1, vec![1, 0]), mip(1, 1, vec![1])],
            masked: false,
            clamp: [false; 2],
            lod_set: 0,
        };
        let image = texture_to_image(&texture, Some(&palette)).unwrap();
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert_eq!(image.data.unwrap(), [10, 20, 30, 255, 0, 0, 0, 0, 10, 20, 30, 255]);
        assert!(texture_to_image(&texture, None).is_none());
    }

    #[test]
    fn dxt_mips_stop_at_empty() {
        let texture = Texture {
            format: TextureFormat::Dxt1,
            palette: None,
            mips: vec![mip(8, 8, vec![0; 32]), mip(4, 4, vec![0; 8]), mip(2, 2, vec![])],
            masked: false,
            clamp: [false; 2],
            lod_set: 0,
        };
        let image = texture_to_image(&texture, None).unwrap();
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert_eq!(image.data.unwrap().len(), 40);
    }

    #[test]
    fn dxt_decoded_with_mips() {
        let block = vec![0x1f, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, 0xff];
        let texture = Texture {
            format: TextureFormat::Dxt1,
            palette: None,
            mips: vec![mip(8, 8, block.repeat(4)), mip(4, 4, block.clone()), mip(2, 2, block.clone()), mip(1, 1, block)],
            masked: false,
            clamp: [false; 2],
            lod_set: 0,
        };
        let image = texture_image(&texture, None, Compression::Off).unwrap();
        assert_eq!(image.texture_descriptor.format, GpuFormat::Rgba8UnormSrgb);
        assert_eq!(image.texture_descriptor.mip_level_count, 4);
        let data = image.data.unwrap();
        assert_eq!(data.len(), (64 + 16 + 4 + 1) * 4);
        assert!(data.iter().all(|&b| b == 0));
    }

    #[test]
    fn combiners_bake_at_the_larger_size() {
        let ops = p2_assets::CombineOps { color: p2_assets::ColorOperation::Add, alpha: p2_assets::AlphaOperation::Material1, invert_mask: false, modulate: 1 };
        let small = [[10, 0, 0, 255]];
        let large = [[0, 20, 0, 0]; 4];
        let image = texture_combined(&ops, (1, 1, &small), (2, 2, &large), None);
        assert_eq!((image.width(), image.height()), (2, 2));
        assert_eq!(image.data.as_deref().map(|d| &d[..4]), Some(&[10, 20, 0, 255][..]));
    }

    #[test]
    fn dxt_needs_whole_blocks() {
        let texture = Texture { format: TextureFormat::Dxt1, palette: None, mips: vec![mip(2, 2, vec![0; 8])], masked: false, clamp: [false; 2], lod_set: 0 };
        assert!(texture_to_image(&texture, None).is_none());
    }

    #[test]
    fn ui_image_follows_clamp_modes() {
        let texture = Texture {
            format: TextureFormat::Rgba8,
            palette: None,
            mips: vec![mip(1, 1, vec![0; 4])],
            masked: false,
            clamp: [true, false],
            lod_set: 0,
        };
        let ImageSampler::Descriptor(sampler) = texture_to_ui_image(&texture, None, false).unwrap().sampler else {
            panic!("no sampler descriptor")
        };
        assert_eq!((sampler.address_mode_u, sampler.address_mode_v), (ImageAddressMode::ClampToEdge, ImageAddressMode::Repeat));
    }
}
