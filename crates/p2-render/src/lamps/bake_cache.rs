//! The load's lamp and probe bakes kept on disk, one file for each set of inputs, so a map loaded again by the
//! same build skips the bake. A missing, damaged or foreign file is a miss.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use bevy::math::Vec3;
use serde::{Deserialize, Serialize};

use super::LampJobResult;
use super::atlas::WorldTriangles;
use super::grid::{ShadowSurface, Sides};
use crate::actor_light::LightEnvironment;
use crate::level_collision::StaticCollision;
use crate::probes::ProbeBake;

const MAGIC: &[u8; 4] = b"P2LB";
/// Raise with any change to what is cached.
const VERSION: u32 = 1;
const KEPT_FILES: usize = 8;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Baked {
    pub(super) lamps: LampJobResult,
    pub(super) probes: Option<(i32, Option<ProbeBake>)>,
}

pub(super) struct BakeInputs<'a> {
    pub(super) environment: &'a LightEnvironment,
    pub(super) collision: &'a StaticCollision,
    pub(super) world: &'a WorldTriangles,
    pub(super) surfaces: &'a [ShadowSurface],
    pub(super) origin: Vec3,
    pub(super) side: u32,
    pub(super) probe_level: u8,
}

pub(super) fn folder() -> Option<PathBuf> {
    Some(p2_host::game_dir::config_folder()?.join("bake-cache"))
}

/// The running program's size and modified time: another build never reads this one's files.
pub(super) fn exe_stamp() -> Option<[u64; 2]> {
    let file = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    let modified = file.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some([file.len(), modified.as_nanos() as u64])
}

fn hash_floats(hash: &mut DefaultHasher, values: &[f32]) {
    values.len().hash(hash);
    hash.write(bytemuck::cast_slice(values));
}

/// Lights are hashed by their debug text, which names every field; it can change only with the build.
pub(super) fn key(inputs: &BakeInputs, stamp: [u64; 2]) -> u64 {
    let BakeInputs { environment, collision, world, surfaces, origin, side, probe_level } = inputs;
    let mut hash = DefaultHasher::new();
    (VERSION, stamp, side, probe_level, world.terrain_from).hash(&mut hash);
    let mut lights: Vec<_> = environment.lights.iter().collect();
    lights.sort_unstable_by_key(|&(number, _)| *number);
    format!("{:?}", (lights, &environment.zone_ambients, environment.level_brightness, &environment.sun, &environment.suns)).hash(&mut hash);
    let model = &collision.model;
    (model.nodes.len(), model.zones.len(), model.leaves.len(), collision.lamp_masked.len(), surfaces.len()).hash(&mut hash);
    let origin = origin.to_array();
    for values in [model.points.as_flattened(), model.vectors.as_flattened(), world.triangles.as_flattened().as_flattened(), world.albedos.as_flattened(), &origin[..]] {
        hash_floats(&mut hash, values);
    }
    for node in &model.nodes {
        hash_floats(&mut hash, &node.plane);
    }
    for caster in &collision.lamp_masked {
        hash_floats(&mut hash, &[caster.cutoff]);
        hash_floats(&mut hash, caster.triangles.as_flattened().as_flattened());
    }
    for surface in surfaces.iter() {
        hash_floats(&mut hash, [surface.origin, surface.u, surface.v, surface.normal].as_flattened());
        (surface.size, surface.sides == Sides::Two).hash(&mut hash);
    }
    hash.finish()
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(bytes);
    hash.finish()
}

fn file(folder: &Path, key: u64) -> PathBuf {
    folder.join(format!("{key:016x}.bake"))
}

pub(super) fn load(folder: &Path, key: u64) -> Option<Baked> {
    let bytes = std::fs::read(file(folder, key)).ok()?;
    let (stored, rest) = bytes.strip_prefix(MAGIC)?.split_first_chunk::<8>()?;
    let (sum, packed) = rest.split_first_chunk::<8>()?;
    if u64::from_le_bytes(*stored) != key || u64::from_le_bytes(*sum) != checksum(packed) {
        return None;
    }
    postcard::from_bytes(&lz4_flex::decompress_size_prepended(packed).ok()?).ok()
}

/// Written through a temp file and a rename, so a reader never sees half a file; then only the newest files stay.
pub(super) fn save(folder: &Path, key: u64, baked: &Baked) -> io::Result<()> {
    let packed = lz4_flex::compress_prepend_size(&postcard::to_stdvec(baked).map_err(io::Error::other)?);
    let mut bytes = MAGIC.to_vec();
    bytes.extend(key.to_le_bytes());
    bytes.extend(checksum(&packed).to_le_bytes());
    bytes.extend(packed);
    std::fs::create_dir_all(folder)?;
    let path = file(folder, key);
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(partial, path)?;
    let mut files: Vec<_> = std::fs::read_dir(folder)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            Some((entry.metadata().ok()?.modified().ok()?, entry.path()))
        })
        .collect();
    files.sort_unstable_by_key(|file| std::cmp::Reverse(file.0));
    for (_, old) in files.into_iter().skip(KEPT_FILES) {
        std::fs::remove_file(old)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bevy::asset::Handle;
    use p2_render_cpu::MeshLight;
    use rustc_hash::FxHashMap;

    use super::super::atlas::lamp_bake;
    use super::super::visibility::{PatchJob, VisibilityPlan};
    use super::*;
    use crate::level_collision::{LevelAlbedos, MaskedCaster};

    const TRIANGLES: [[[f32; 3]; 3]; 2] = [[[0.0; 3], [64.0, 0.0, 0.0], [0.0, 64.0, 0.0]], [[0.0, 0.0, 32.0], [64.0, 0.0, 32.0], [0.0, 64.0, 32.0]]];

    fn baked() -> Baked {
        let mut bake = lamp_bake(&TRIANGLES, &[], &[([10.0, 10.0, 16.0], 128.0), ([500.0, 0.0, 0.0], 64.0)]);
        bake.records = vec![[1.0, 2.0, 3.0, 4.0]; 3];
        let job = PatchJob { origin: [1.0; 3], groups: 2, u: [0.5; 3], list: 7, v: [0.25; 3], size: 16, normal: [0.0, 0.0, 1.0], atlas: 65537 };
        let lamps = LampJobResult {
            lights: Vec::new(),
            zones_reach: vec![(vec![1, 3], [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), (Vec::new(), [0.5; 6])],
            bake: Some(bake),
            book: vec![[0.25, -1.0, 3.5, f32::MAX]; 5],
            listed: "cells of 128 units".to_owned(),
            visibility: Some(VisibilityPlan { size: [64, 32], jobs: vec![job; 3] }),
            patches: vec![(0, [0, 0]), (2, [8, 16])],
        };
        Baked { lamps, probes: Some((128, Some(ProbeBake { spacing: 256.0, pixels: (0..=255).collect() }))) }
    }

    fn temp_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("p2-bake-cache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        folder
    }

    #[test]
    fn a_saved_bake_loads_byte_for_byte() {
        let folder = temp_folder("round-trip");
        let original = baked();
        save(&folder, 42, &original).unwrap();
        let mut loaded = load(&folder, 42).unwrap();
        assert_eq!(postcard::to_stdvec(&loaded).unwrap(), postcard::to_stdvec(&original).unwrap());
        let bake = loaded.lamps.bake.as_mut().unwrap();
        bake.restore(&TRIANGLES, &[]);
        assert_eq!(bake.vertices, original.lamps.bake.as_ref().unwrap().vertices);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_damaged_or_foreign_file_is_a_miss() {
        let folder = temp_folder("damage");
        save(&folder, 7, &baked()).unwrap();
        let path = file(&folder, 7);
        let bytes = std::fs::read(&path).unwrap();
        let mut flipped = bytes.clone();
        flipped[bytes.len() / 2] ^= 0x10;
        for damaged in [flipped, bytes[..bytes.len() - 3].to_vec(), bytes[..10].to_vec(), Vec::new()] {
            std::fs::write(&path, damaged).unwrap();
            assert!(load(&folder, 7).is_none());
        }
        std::fs::write(file(&folder, 8), &bytes).unwrap();
        assert!(load(&folder, 8).is_none());
        assert!(load(&folder, 9).is_none());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn the_folder_keeps_only_the_newest_files() {
        let folder = temp_folder("bounded");
        for key in 0..KEPT_FILES as u64 + 3 {
            save(&folder, key, &baked()).unwrap();
        }
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), KEPT_FILES);
        std::fs::remove_dir_all(folder).unwrap();
    }

    struct Setup {
        environment: LightEnvironment,
        collision: StaticCollision,
        world: WorldTriangles,
        surfaces: Vec<ShadowSurface>,
        origin: Vec3,
        side: u32,
        probe_level: u8,
        stamp: [u64; 2],
    }

    impl Setup {
        fn new() -> Self {
            let light = MeshLight { brightness: 64.0, radius: 32.0, ..MeshLight::default() };
            let environment = LightEnvironment { lights: FxHashMap::from_iter([(3, light), (9, light)]), zone_ambients: vec![[0.1; 3]; 2], level_brightness: 1.0, suns: Vec::new(), zone_suns: Vec::new(), sun: None, terrain: Vec::new() };
            let collision = StaticCollision {
                model: p2_assets::Model::default(),
                hulls: p2_collision::BspHulls::default(),
                terrains: Vec::new(),
                meshes: Vec::new(),
                mesh_set: p2_collision::MeshSet::default(),
                lamp_casters: Vec::new(),
                lamp_masked: vec![MaskedCaster { image: Handle::default(), cutoff: 0.5, triangles: vec![[[0.0; 5], [1.0; 5], [2.0; 5]]] }],
                albedos: LevelAlbedos::default(),
            };
            let world = WorldTriangles { triangles: TRIANGLES.to_vec(), albedos: vec![[0.3; 3]; 2], terrain_from: 2 };
            let surface = ShadowSurface { origin: [0.0; 3], u: [1.0, 0.0, 0.0], v: [0.0, 1.0, 0.0], normal: [0.0, 0.0, 1.0], size: [16, 8], sides: Sides::One };
            Self { environment, collision, world, surfaces: vec![surface], origin: Vec3::ZERO, side: 4096, probe_level: 1, stamp: [1000, 2000] }
        }

        fn key(&self) -> u64 {
            let inputs = BakeInputs {
                environment: &self.environment,
                collision: &self.collision,
                world: &self.world,
                surfaces: &self.surfaces,
                origin: self.origin,
                side: self.side,
                probe_level: self.probe_level,
            };
            key(&inputs, self.stamp)
        }
    }

    #[test]
    fn every_input_changes_the_key() {
        let base = Setup::new().key();
        assert_eq!(Setup::new().key(), base);
        let changes: [fn(&mut Setup); 17] = [
            |setup| setup.environment.lights.get_mut(&3).unwrap().radius = 33.0,
            |setup| setup.environment.lights.get_mut(&9).unwrap().location.x = 1.0,
            |setup| setup.environment.level_brightness = 0.5,
            |setup| setup.environment.zone_ambients[1][2] = 0.2,
            |setup| setup.environment.suns.push(MeshLight::default()),
            |setup| setup.collision.lamp_masked[0].cutoff = 0.25,
            |setup| setup.collision.lamp_masked[0].triangles[0][1][3] = 9.0,
            |setup| setup.collision.model.points.push([1.0; 3]),
            |setup| setup.world.triangles[1][2][0] = 5.0,
            |setup| setup.world.albedos[0][0] = 0.9,
            |setup| setup.world.terrain_from = 1,
            |setup| setup.surfaces[0].size[0] = 17,
            |setup| setup.surfaces[0].sides = Sides::Two,
            |setup| setup.origin.y = 1.0,
            |setup| setup.side = 8192,
            |setup| setup.probe_level = 2,
            |setup| setup.stamp[1] += 1,
        ];
        for (index, change) in changes.into_iter().enumerate() {
            let mut setup = Setup::new();
            change(&mut setup);
            assert_ne!(setup.key(), base, "change {index}");
        }
    }
}
