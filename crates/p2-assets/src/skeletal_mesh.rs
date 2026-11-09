use p2_math::{Rotator, Vector};
use p2_package::{Export, Package, Reader};

use crate::Error;
use crate::read::{BOX_SIZE, array, invalid, skip_array, skip_lazy_array, skip_primitive, vec3, vec4};

#[derive(Debug, Clone)]
pub struct SkeletalMesh {
    pub materials: Vec<i32>,
    pub scale: Vector,
    pub origin: Vector,
    pub rot_origin: Rotator,
    pub positions: Vec<[f32; 3]>,
    /// Bone influences of all positions, in position order; at least one each, weights sum to 1. One position's run:
    /// [`SkeletalMesh::influences_of`].
    pub influences: Vec<Influence>,
    pub influence_starts: Vec<u32>,
    pub wedges: Vec<Wedge>,
    pub indices: Vec<u16>,
    pub sections: Vec<SkelMeshSection>,
    pub bones: Vec<Bone>,
    pub default_animation: i32,
    pub attach_aliases: Vec<AttachAlias>,
}

/// `AttachToBone` with the tag `alias` attaches to `bone`, offset by `coords` in the bone's frame.
#[derive(Debug, Clone)]
pub struct AttachAlias {
    pub alias: usize,
    pub bone: usize,
    pub coords: [[f32; 3]; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wedge {
    pub point: u32,
    pub uv: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Influence {
    pub bone: u16,
    pub weight: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct SkelMeshSection {
    pub material: u16,
    pub first_face: u16,
    pub num_faces: u16,
}

#[derive(Debug, Clone)]
pub struct Bone {
    pub name: usize,
    pub parent: usize,
    pub num_children: i32,
    /// Bind-pose orientation quaternion X, Y, Z, W, relative to the parent. As stored: the root bone rotates by
    /// `q` (`v' = q v q*`), every other bone by its conjugate.
    pub orientation: [f32; 4],
    pub position: [f32; 3],
}

struct LodModel {
    stream: Vec<u32>,
    stream_offset: usize,
    num_wedges: i32,
    positions: Vec<[f32; 3]>,
    indices: Vec<u16>,
    sections: Vec<SkelMeshSection>,
}

impl SkeletalMesh {
    pub fn influences_of(&self, point: usize) -> &[Influence] {
        match self.influence_starts.get(point..) {
            Some(&[start, end, ..]) => &self.influences[start as usize..end as usize],
            _ => &[],
        }
    }

    pub fn point_influences(&self) -> impl Iterator<Item = &[Influence]> {
        self.influence_starts.windows(2).map(|run| &self.influences[run[0] as usize..run[1] as usize])
    }
}

impl LodModel {
    fn skinning(&self) -> Result<(Vec<Wedge>, Vec<Influence>, Vec<u32>), Error> {
        let (wedges, influences, starts) = decode_stream(&self.stream).ok_or_else(|| invalid("skinning stream", self.stream_offset))?;
        if usize::try_from(self.num_wedges).ok() != Some(wedges.len()) {
            return Err(invalid("skinning stream wedge count", self.stream_offset));
        }
        if starts.len() != self.positions.len() + 1 {
            return Err(invalid("skinning stream point count", self.stream_offset));
        }
        Ok((wedges, influences, starts))
    }
}

/// Bounding sphere (centre, radius) of a skeletal or static mesh, without reading its geometry:
/// native data starts with the bounding box (25 bytes), then the sphere.
pub fn read_primitive_sphere(pkg: &Package, file: &[u8], export: &Export) -> Result<[f32; 4], Error> {
    let mut r = pkg.native_reader(file, export)?;
    r.bytes(BOX_SIZE)?;
    vec4(&mut r)
}

pub fn read_skeletal_mesh(pkg: &Package, file: &[u8], export: &Export) -> Result<SkeletalMesh, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;

    skip_primitive(r)?;

    let version_offset = r.pos();
    if r.i32()? < 2 {
        // Older versions store extra arrays; every mesh in the install is version 2.
        return Err(invalid("skeletal mesh internal version below 2", version_offset));
    }
    r.i32()?; // ModelVerts (unverified)
    skip_array(r, 4)?;
    let materials = array(r, |r| Ok(r.compact()?))?;
    let scale = vec3(r)?.into();
    let origin = vec3(r)?.into();
    let rot_origin = Rotator::new(r.i32()?, r.i32()?, r.i32()?);
    skip_array(r, 2)?;
    skip_array(r, 8)?;  // Faces (empty in the install)
    skip_array(r, 2)?;
    skip_array(r, 10)?;  // Wedges (empty in the install)
    skip_array(r, 8)?;
    r.bytes(24)?; // MeshScaleMax, LOD tuning (unverified)

    skip_array(r, 12)?;
    let bones = array(r, |r| read_bone(pkg, r))?;
    let default_animation = r.compact()?;
    r.i32()?;
    array(r, |r| {
        skip_array(r, 2)?;
        Ok(r.i32()?)
    })?; // WeightIndices (unverified)
    skip_array(r, 4)?; // BoneInfluences: two WORDs each (unverified)
    let aliases = array(r, |r| Ok(pkg.read_name(r)?))?;
    let alias_bones = array(r, |r| Ok(pkg.read_name(r)?))?;
    let alias_coords = array(r, |r| Ok([vec3(r)?, vec3(r)?, vec3(r)?, vec3(r)?]))?;
    let attach_aliases = aliases
        .into_iter()
        .zip(alias_bones)
        .zip(alias_coords)
        .map(|((alias, bone), coords)| AttachAlias { alias, bone, coords })
        .collect();
    let lods_offset = r.pos();
    let mut lods = array(r, read_lod_model)?;
    r.compact()?; // object reference (unverified)
    for _ in 0..6 {
        skip_lazy_array(r)?;
    }
    if !r.at_end() {
        return Err(invalid("skeletal mesh end", r.pos()));
    }

    if lods.is_empty() {
        return Err(invalid("skeletal mesh without LOD models", lods_offset));
    }
    let lod = lods.swap_remove(0);
    let (wedges, influences, influence_starts) = lod.skinning()?;
    Ok(SkeletalMesh {
        materials,
        scale,
        origin,
        rot_origin,
        positions: lod.positions,
        influences,
        influence_starts,
        wedges,
        indices: lod.indices,
        sections: lod.sections,
        bones,
        default_animation,
        attach_aliases,
    })
}

fn read_bone(pkg: &Package, r: &mut Reader) -> Result<Bone, Error> {
    let name = pkg.read_name(r)?;
    r.u32()?;
    let orientation = vec4(r)?;
    let position = vec3(r)?;
    r.bytes(16)?;
    let num_children = r.i32()?;
    let parent_offset = r.pos();
    let parent = usize::try_from(r.i32()?).map_err(|_| invalid("bone parent index", parent_offset))?;
    Ok(Bone { name, parent, num_children, orientation, position })
}

fn read_lod_model(r: &mut Reader) -> Result<LodModel, Error> {
    let stream_offset = r.pos();
    let stream = array(r, |r| Ok(r.u32()?))?;
    let positions = array(r, |r| {
        let position = vec3(r)?;
        r.u32()?;
        Ok(position)
    })?;
    let num_wedges = r.i32()?;
    let sections = array(r, read_section)?;
    skip_array(r, 18)?;
    let indices = array(r, |r| Ok(r.u16()?))?;
    r.i32()?;
    skip_array(r, 2)?;
    r.i32()?;
    r.bytes(12)?;
    skip_array(r, 32)?;
    for _ in 0..4 {
        skip_lazy_array(r)?;
    }
    r.bytes(24)?; // DisplayFactor, LODHysteresis, NumSharedPoints, MaxInfluences, 2 INTs (unverified)

    Ok(LodModel { stream, stream_offset, num_wedges, positions, indices, sections })
}

/// New point: header (bits 28..30 count - 1, bit 31 shared, bits 0..27 first influence), extras, U, V.
/// Shared point: `0xF0000000 | shared_index * 6`, U, V. Ends with `0xFFFFFFFF`; malformed: `None`.
fn decode_stream(stream: &[u32]) -> Option<(Vec<Wedge>, Vec<Influence>, Vec<u32>)> {
    let mut wedges = Vec::new();
    let mut influences = Vec::new();
    let mut starts = vec![0u32];
    let mut shared = Vec::new();
    let mut words = stream.iter().copied();
    loop {
        let header = words.next()?;
        if header == 0xFFFF_FFFF {
            break;
        }
        let point = if header >> 28 == 0xF {
            *shared.get(usize::try_from((header & 0x0FFF_FFFF) / 6).ok()?)?
        } else {
            let point = u32::try_from(starts.len() - 1).ok()?;
            let start = influences.len();
            influences.push(decode_influence(header)?);
            for _ in 0..(header >> 28) & 7 {
                influences.push(decode_influence(words.next()?)?);
            }
            normalize(&mut influences[start..]);
            starts.push(u32::try_from(influences.len()).ok()?);
            if header & 0x8000_0000 != 0 {
                shared.push(point);
            }
            point
        };
        let uv = [f32::from_bits(words.next()?), f32::from_bits(words.next()?)];
        wedges.push(Wedge { point, uv });
    }
    words.next().is_none().then_some((wedges, influences, starts))
}

/// Influence in bits 0..27: `weight << 12 | bone * 6`, weight = stored float × 65535 truncated to 16 bits.
fn decode_influence(word: u32) -> Option<Influence> {
    let bone_offset = word & 0xFFF;
    if !bone_offset.is_multiple_of(6) {
        return None;
    }
    let bone = u16::try_from(bone_offset / 6).ok()?;
    Some(Influence { bone, weight: ((word >> 12) & 0xFFFF) as f32 / 65535.0 })
}

/// Scales weights to sum 1. The engine ignores the weight of a single influence (full weight), which this matches.
fn normalize(influences: &mut [Influence]) {
    let sum: f32 = influences.iter().map(|i| i.weight).sum();
    let count = influences.len() as f32;
    for influence in influences {
        influence.weight = if sum > 0.0 { influence.weight / sum } else { 1.0 / count };
    }
}

/// On disk: 9 `WORD`s (one in-memory `WORD` is not stored).
fn read_section(r: &mut Reader) -> Result<SkelMeshSection, Error> {
    let material = r.u16()?;
    r.bytes(12)?; // MinStreamIndex, MinWedgeIndex, MaxWedgeIndex, NumStreamIndices, BoneIndex, MaxInfluences (unverified)
    Ok(SkelMeshSection { material, first_face: r.u16()?, num_faces: r.u16()? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_new_and_shared_wedges() {
        let (u, v) = (0.25f32.to_bits(), 0.5f32.to_bits());
        let stream = [
            0x9000_0000 | (0xC000 << 12) | 12, // point 0: shared, two influences, bone 2 weight 0.75
            (0x4000 << 12) | 6,                // its second influence: bone 1, weight 0.25
            u,
            v,
            0x0FFF_F000, // point 1: no influences in the source (bone 0, full weight)
            v,
            u,
            0xF000_0000, // shared point 0 again
            u,
            u,
            0xFFFF_FFFF,
        ];
        let (wedges, influences, starts) = decode_stream(&stream).unwrap();
        let points: Vec<u32> = wedges.iter().map(|w| w.point).collect();
        assert_eq!(points, [0, 1, 0]);
        assert_eq!(wedges[1].uv, [0.5, 0.25]);
        assert_eq!(starts, [0, 2, 3]);
        assert_eq!(influences.iter().map(|i| i.bone).collect::<Vec<_>>(), [2, 1, 0]);
        assert!((influences[0].weight - 0.75).abs() < 1e-4);
        assert!((influences[0].weight + influences[1].weight - 1.0).abs() < 1e-6);
        assert_eq!(influences[2], Influence { bone: 0, weight: 1.0 });
        assert!(decode_stream(&stream[..stream.len() - 1]).is_none());
        assert!(decode_stream(&[0xF000_0006, u, v, 0xFFFF_FFFF]).is_none());
        assert!(decode_stream(&[0x0000_0005, u, v, 0xFFFF_FFFF]).is_none());
    }
}
