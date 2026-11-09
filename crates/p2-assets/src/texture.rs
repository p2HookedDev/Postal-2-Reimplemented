use p2_package::{Export, Package, Reader};
use p2_script::{Objects, Resolve, Source};

use crate::Error;
use crate::read::{count, lazy_bytes, unsigned};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    P8,
    Rgba7,
    Rgb16,
    Dxt1,
    Rgb8,
    Rgba8,
    NoData,
    Dxt3,
    Dxt5,
    L8,
    G16,
    Rrrgggbbb,
}

impl TextureFormat {
    /// 0 for the formats nothing decodes.
    pub fn mip_bytes(self, width: u32, height: u32) -> usize {
        let (width, height) = (width as usize, height as usize);
        let blocks = width.div_ceil(4) * height.div_ceil(4);
        match self {
            Self::P8 | Self::L8 => width * height,
            Self::G16 => width * height * 2,
            Self::Rgba8 => width * height * 4,
            Self::Dxt1 => blocks * 8,
            Self::Dxt3 | Self::Dxt5 => blocks * 16,
            _ => 0,
        }
    }

    pub(crate) fn from_byte(value: u8) -> Result<Self, Error> {
        use TextureFormat::*;
        const ALL: [TextureFormat; 12] = [P8, Rgba7, Rgb16, Dxt1, Rgb8, Rgba8, NoData, Dxt3, Dxt5, L8, G16, Rrrgggbbb];
        ALL.get(usize::from(value)).copied().ok_or(Error::UnknownTextureFormat(value))
    }
}

#[derive(Debug, Clone)]
pub struct Mip {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub format: TextureFormat,
    pub palette: Option<i32>,
    pub mips: Vec<Mip>,
    pub masked: bool,
    pub clamp: [bool; 2],
    /// Which `TextureDetail*` setting picks the mips shown: 1 world, 2 skin, 3 lightmap, 4 terrain,
    /// 5 weapon (client settings order; the enum names are `(unverified)`), 0 none.
    pub lod_set: u8,
}

pub const LOD_SET_LIGHT_MAP: u8 = 3;

pub fn read_texture<'a, R: Resolve + ?Sized>(objects: &mut Objects<'a, R>, src: Source<'a>, export: usize) -> Result<Texture, Error> {
    let object = objects.read(src, export)?;
    let format = TextureFormat::from_byte(object.byte("Format"))?;
    // A class default would index the class's package; texture classes are not known to set one (unverified).
    let palette = object.object_in("Palette", src.package);
    let masked = object.bool("bMasked");
    let clamp = ["UClampMode", "VClampMode"].map(|name| object.byte(name) == 1);
    let lod_set = object.byte("LODSet");
    let mut r = src.package.export_reader(src.data, &src.package.exports[export])?;
    r.seek(object.native_offset)?;
    let count = count(&mut r)?;
    let mips = (0..count).map(|_| read_mip(&mut r)).collect::<Result<_, _>>()?;
    Ok(Texture { format, palette, mips, masked, clamp, lod_set })
}

pub fn texture_heights(texture: &Texture) -> Option<(u32, u32, Vec<u16>)> {
    let mip = texture.mips.first().filter(|mip| !mip.data.is_empty())?;
    let heights = match texture.format {
        TextureFormat::G16 => mip.data.as_chunks::<2>().0.iter().map(|&pair| u16::from_le_bytes(pair)).collect(),
        TextureFormat::P8 => mip.data.iter().map(|&index| u16::from(index) << 8).collect(),
        _ => return None,
    };
    Some((mip.width, mip.height, heights))
}

/// Mip 0 of a weight texture as one alpha byte per texel: RGBA8 alpha, P8 palette red (retail copies
/// red into alpha for terrain alpha maps), L8 value. `None` for other formats.
pub fn texture_alpha(texture: &Texture, palette: Option<&[[u8; 4]]>) -> Option<(u32, u32, Vec<u8>)> {
    let mip = texture.mips.first().filter(|mip| !mip.data.is_empty())?;
    let alpha = match texture.format {
        // Stored B, G, R, A.
        TextureFormat::Rgba8 => mip.data.as_chunks::<4>().0.iter().map(|texel| texel[3]).collect(),
        TextureFormat::P8 => {
            let palette = palette?;
            mip.data.iter().map(|&index| palette.get(usize::from(index)).map_or(0, |color| color[0])).collect()
        }
        TextureFormat::L8 => mip.data.clone(),
        _ => return None,
    };
    Some((mip.width, mip.height, alpha))
}

pub fn texture_bgra(texture: &Texture) -> Option<(u32, u32, Vec<[u8; 4]>)> {
    let mip = texture.mips.first().filter(|mip| !mip.data.is_empty() && texture.format == TextureFormat::Rgba8)?;
    Some((mip.width, mip.height, mip.data.as_chunks::<4>().0.to_vec()))
}

pub fn texture_rgba(texture: &Texture, palette: Option<&[[u8; 4]]>) -> Option<(u32, u32, Vec<[u8; 4]>)> {
    mip_rgba(texture, 0, palette)
}

pub fn mip_rgba(texture: &Texture, level: usize, palette: Option<&[[u8; 4]]>) -> Option<(u32, u32, Vec<[u8; 4]>)> {
    let mip = texture.mips.get(level).filter(|mip| !mip.data.is_empty())?;
    let palette = palette.map(|palette| {
        let mut table: [[u8; 4]; 256] = std::array::from_fn(|i| palette.get(i).copied().unwrap_or_default());
        if texture.masked {
            table[0] = [0; 4];
        }
        table
    });
    rgba_pixels(texture.format, mip.width, mip.height, &mip.data, palette.as_ref())
}

pub(crate) fn rgba_pixels(
    format: TextureFormat,
    width: u32,
    height: u32,
    data: &[u8],
    palette: Option<&[[u8; 4]; 256]>,
) -> Option<(u32, u32, Vec<[u8; 4]>)> {
    let texels = (width as usize).checked_mul(height as usize)?;
    let pixels = match format {
        TextureFormat::P8 => {
            let palette = palette?;
            data.iter().take(texels).map(|&i| palette[usize::from(i)]).collect()
        }
        TextureFormat::Rgba8 => data.as_chunks::<4>().0.iter().take(texels).map(|&[b, g, r, a]| [r, g, b, a]).collect(),
        TextureFormat::L8 => data.iter().take(texels).map(|&v| [v, v, v, 255]).collect(),
        TextureFormat::G16 => data.as_chunks::<2>().0.iter().take(texels).map(|&[_, v]| [v, v, v, 255]).collect(),
        TextureFormat::Dxt1 | TextureFormat::Dxt3 | TextureFormat::Dxt5 => decode_dxt(format, width, height, data)?,
        _ => return None,
    };
    (pixels.len() == texels).then_some((width, height, pixels))
}

fn decode_dxt(format: TextureFormat, width: u32, height: u32, data: &[u8]) -> Option<Vec<[u8; 4]>> {
    let (w, h) = (width as usize, height as usize);
    let block_size = format.mip_bytes(4, 4);
    let blocks_x = w.div_ceil(4);
    if blocks_x.checked_mul(h.div_ceil(4))?.checked_mul(block_size)? > data.len() {
        return None;
    }
    let mut out = vec![[0u8; 4]; w * h];
    for by in 0..h.div_ceil(4) {
        for bx in 0..blocks_x {
            let start = (by * blocks_x + bx) * block_size;
            let block = data.get(start..start + block_size)?;
            let (alpha, color) = block.split_at(block_size - 8);
            let texels = color_block(color, format == TextureFormat::Dxt1);
            let alphas: Option<[u8; 16]> = match format {
                TextureFormat::Dxt3 => Some(std::array::from_fn(|i| ((alpha[i / 2] >> (4 * (i % 2))) & 0xf) * 17)),
                TextureFormat::Dxt5 => Some(dxt5_alphas(alpha)),
                _ => None,
            };
            for i in 0..16 {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x >= w || y >= h {
                    continue;
                }
                let mut texel = texels[i];
                if let Some(alphas) = alphas {
                    texel[3] = alphas[i];
                }
                out[y * w + x] = texel;
            }
        }
    }
    Some(out)
}

/// The 16 texels of an 8-byte colour block (two RGB565 end points, 2-bit indices). `one_bit_alpha` (DXT1): with
/// `c0 <= c1` index 3 is transparent black.
fn color_block(block: &[u8], one_bit_alpha: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let rgb = |c: u16| {
        let (r, g, b) = (u32::from(c >> 11), u32::from((c >> 5) & 0x3f), u32::from(c & 0x1f));
        [r * 255 / 31, g * 255 / 63, b * 255 / 31]
    };
    let (a, b) = (rgb(c0), rgb(c1));
    let mix = |wa: u32, wb: u32, d: u32| [0, 1, 2].map(|i| ((a[i] * wa + b[i] * wb) / d) as u8);
    let opaque = |c: [u32; 3]| [c[0] as u8, c[1] as u8, c[2] as u8, 255];
    let palette = if c0 > c1 || !one_bit_alpha {
        let (m1, m2) = (mix(2, 1, 3), mix(1, 2, 3));
        [opaque(a), opaque(b), [m1[0], m1[1], m1[2], 255], [m2[0], m2[1], m2[2], 255]]
    } else {
        let m = mix(1, 1, 2);
        [opaque(a), opaque(b), [m[0], m[1], m[2], 255], [0; 4]]
    };
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    std::array::from_fn(|i| palette[((indices >> (2 * i)) & 3) as usize])
}

fn dxt5_alphas(block: &[u8]) -> [u8; 16] {
    let (a0, a1) = (u32::from(block[0]), u32::from(block[1]));
    let palette: [u8; 8] = std::array::from_fn(|index| {
        let index = index as u32;
        let value = match index {
            0 => a0,
            1 => a1,
            _ if a0 > a1 => (a0 * (8 - index) + a1 * (index - 1)) / 7,
            6 => 0,
            7 => 255,
            _ => (a0 * (6 - index) + a1 * (index - 1)) / 5,
        };
        value as u8
    });
    let bits = block[2..8].iter().rev().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    std::array::from_fn(|i| palette[((bits >> (3 * i)) & 7) as usize])
}

pub fn read_palette(pkg: &Package, file: &[u8], export: &Export) -> Result<Vec<[u8; 4]>, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let count = count(&mut r)?;
    Ok((0..count).map(|_| r.array()).collect::<Result<_, _>>()?)
}

fn read_mip(r: &mut Reader) -> Result<Mip, Error> {
    let data = lazy_bytes(r)?.to_vec();
    let width = unsigned(r, "mip size")? as u32;
    let height = unsigned(r, "mip size")? as u32;
    r.bytes(2)?;
    Ok(Mip { width, height, data })
}


#[cfg(test)]
mod dxt_tests {
    use super::*;

    #[test]
    fn mip_bytes_rounds_blocks_up() {
        assert_eq!(TextureFormat::Dxt1.mip_bytes(5, 4), 16);
        assert_eq!(TextureFormat::Dxt5.mip_bytes(1, 1), 16);
        assert_eq!(TextureFormat::G16.mip_bytes(3, 2), 12);
        assert_eq!(TextureFormat::NoData.mip_bytes(4, 4), 0);
    }

    fn texture(format: TextureFormat, data: Vec<u8>) -> Texture {
        Texture { format, palette: None, mips: vec![Mip { width: 4, height: 4, data }], masked: false, clamp: [false; 2], lod_set: 0 }
    }

    const COLOR: [u8; 8] = [0x00, 0xf8, 0x1f, 0x00, 0b1100_0100, 0, 0, 0];

    #[test]
    fn dxt1_end_points_and_transparent_index() {
        let (_, _, texels) = texture_rgba(&texture(TextureFormat::Dxt1, COLOR.to_vec()), None).unwrap();
        assert_eq!(texels[0], [255, 0, 0, 255]);
        assert_eq!(texels[1], [0, 0, 255, 255]);
        assert_eq!(texels[3], [85, 0, 170, 255]);
        let swapped = [0x1f, 0x00, 0x00, 0xf8, 0b1100_0000, 0, 0, 0];
        let (_, _, texels) = texture_rgba(&texture(TextureFormat::Dxt1, swapped.to_vec()), None).unwrap();
        assert_eq!(texels[3], [0; 4]);
    }

    #[test]
    fn dxt3_and_dxt5_alpha() {
        let mut dxt3 = vec![0x0f, 0xf0, 0, 0, 0, 0, 0, 0];
        dxt3.extend(COLOR);
        let (_, _, texels) = texture_rgba(&texture(TextureFormat::Dxt3, dxt3), None).unwrap();
        assert_eq!([texels[0][3], texels[1][3], texels[3][3]], [255, 0, 255]);
        let mut dxt5 = vec![255, 0, 0b0000_1000, 0, 0, 0, 0, 0];
        dxt5.extend(COLOR);
        let (_, _, texels) = texture_rgba(&texture(TextureFormat::Dxt5, dxt5), None).unwrap();
        assert_eq!([texels[0][3], texels[1][3]], [255, 0]);
    }

    #[test]
    fn dxt_dimensions_larger_than_data_are_rejected() {
        let mut t = texture(TextureFormat::Dxt1, COLOR.to_vec());
        for size in [65536, u32::MAX] {
            (t.mips[0].width, t.mips[0].height) = (size, size);
            assert!(texture_rgba(&t, None).is_none());
        }
    }

    #[test]
    fn masked_palette_index_zero_is_clear() {
        let mut t = texture(TextureFormat::P8, vec![0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1]);
        t.masked = true;
        let palette = [[10, 20, 30, 255], [40, 50, 60, 255]];
        let (_, _, texels) = texture_rgba(&t, Some(&palette)).unwrap();
        assert_eq!((texels[0], texels[1]), ([0; 4], [40, 50, 60, 255]));
    }
}