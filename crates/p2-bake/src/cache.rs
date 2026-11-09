use std::hash::Hasher;
use std::io;
use std::path::Path;

use rustc_hash::FxHasher;

use crate::{Atlas, BakeInput, SCALE};

const MAGIC: &[u8; 4] = b"P2BK";
// Raise with any change to the bake's result, so old files stop matching.
const VERSION: u32 = 1;

pub fn cache_key(input: &BakeInput) -> u64 {
    let mut hash = FxHasher::default();
    let mut float = |value: f32| hash.write_u32(value.to_bits());
    float(VERSION as f32);
    float(SCALE as f32);
    float(input.level_brightness);
    float(input.model.nodes.len() as f32);
    for map in &input.model.light_maps {
        map.world_to_light_map.iter().chain(&map.base).for_each(|&value| float(value));
    }
    for light in input.lights {
        <[f32; 3]>::from(light.location).into_iter().for_each(&mut float);
        [light.brightness, light.radius, f32::from(light.hue), f32::from(light.saturation), f32::from(light.effect.0), f32::from(light.cone)].into_iter().for_each(&mut float);
    }
    for mesh in input.meshes {
        let (min, max) = mesh.bounds();
        <[f32; 3]>::from(min).into_iter().chain(<[f32; 3]>::from(max)).for_each(&mut float);
    }
    input.zone_ambients.iter().flatten().for_each(|&value| float(value));
    float(input.terrains.len() as f32);
    hash.finish()
}

pub fn save(path: &Path, atlases: &[Option<Atlas>]) -> io::Result<()> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend((atlases.len() as u32).to_le_bytes());
    for atlas in atlases {
        let Some(atlas) = atlas else {
            bytes.extend(0u32.to_le_bytes());
            continue;
        };
        let packed = lz4_flex::compress_prepend_size(atlas.texels.as_flattened());
        bytes.extend(atlas.width.to_le_bytes());
        bytes.extend(atlas.height.to_le_bytes());
        bytes.extend((packed.len() as u32).to_le_bytes());
        bytes.extend(packed);
    }
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(partial, path)
}

/// `None` for a missing or damaged file: the map is baked again.
pub fn load(path: &Path) -> Option<Vec<Option<Atlas>>> {
    let bytes = std::fs::read(path).ok()?;
    let mut rest = bytes.strip_prefix(MAGIC)?;
    let mut word = || {
        let (head, tail) = rest.split_first_chunk::<4>()?;
        rest = tail;
        Some(u32::from_le_bytes(*head))
    };
    let count = word()?;
    let mut sizes = Vec::new();
    let mut read = rest;
    for _ in 0..count {
        let (width, tail) = read.split_first_chunk::<4>()?;
        let width = u32::from_le_bytes(*width);
        read = tail;
        if width == 0 {
            sizes.push(None);
            continue;
        }
        let (height, tail) = read.split_first_chunk::<4>()?;
        let (length, tail) = tail.split_first_chunk::<4>()?;
        let length = u32::from_le_bytes(*length) as usize;
        let packed = tail.get(..length)?;
        read = tail.get(length..)?;
        let flat = lz4_flex::decompress_size_prepended(packed).ok()?;
        let height = u32::from_le_bytes(*height);
        let (texels, []) = flat.as_chunks::<4>() else { return None };
        if texels.len() != width as usize * height as usize {
            return None;
        }
        sizes.push(Some(Atlas { width, height, texels: texels.to_vec() }));
    }
    Some(sizes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlases_survive_the_file_and_damage_is_a_miss() {
        let folder = std::env::temp_dir().join(format!("p2-bake-{}", std::process::id()));
        let path = folder.join("map.bake");
        let atlases = vec![None, Some(Atlas { width: 2, height: 1, texels: vec![[1, 2, 3, 4], [5, 6, 7, 8]] })];
        save(&path, &atlases).unwrap();
        let loaded = load(&path).unwrap();
        assert!(loaded[0].is_none());
        let atlas = loaded[1].as_ref().unwrap();
        assert_eq!((atlas.width, atlas.height), (2, 1));
        assert_eq!(atlas.texels, vec![[1, 2, 3, 4], [5, 6, 7, 8]]);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        assert!(load(&path).is_none());
        assert!(load(&folder.join("missing.bake")).is_none());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
