use std::collections::hash_map::Entry;
use bevy::math::Vec3;
use p2_assets::{Influence, SkelMeshSection, SkeletalMesh, Wedge};
use rustc_hash::{FxBuildHasher, FxHashMap};

const ROUNDING: f32 = 0.5;
const CREASE_COS: f32 = 0.5;
const MOVE_CAP: f32 = 0.1;

pub fn smoothed_skeletal_mesh(mesh: &SkeletalMesh) -> Option<SkeletalMesh> {
    let point_count = mesh.positions.len();
    let well_formed = mesh.influence_starts.len() == point_count + 1
        && mesh.influence_starts.last().map(|&end| end as usize) == Some(mesh.influences.len())
        && mesh.wedges.iter().all(|w| (w.point as usize) < point_count)
        && mesh.indices.iter().all(|&w| usize::from(w) < mesh.wedges.len());
    if !well_formed {
        return None;
    }
    let quadrupled = |faces: u16| u16::try_from(usize::from(faces) * 4).ok();
    let sections = mesh
        .sections
        .iter()
        .map(|s| Some(SkelMeshSection { first_face: quadrupled(s.first_face)?, num_faces: quadrupled(s.num_faces)?, ..*s }))
        .collect::<Option<Vec<_>>>()?;
    let faces = mesh.indices.as_chunks::<3>().0;
    let position = |point: u32| Vec3::from(mesh.positions[point as usize]);
    let face_points: Vec<[u32; 3]> = faces.iter().map(|face| face.map(|w| mesh.wedges[usize::from(w)].point)).collect();
    let face_normals: Vec<Vec3> = face_points.iter().map(|&[a, b, c]| (position(b) - position(a)).cross(position(c) - position(a))).collect();
    let mut normals = vec![Vec3::ZERO; point_count];
    let mut edge_faces = FxHashMap::with_capacity_and_hasher(faces.len() * 2, FxBuildHasher);
    for (&[a, b, c], &normal) in face_points.iter().zip(&face_normals) {
        for (p, q) in [(a, b), (b, c), (c, a)] {
            normals[p as usize] += normal;
            let uses: &mut u8 = edge_faces.entry(low_first(p, q)).or_default();
            *uses = uses.saturating_add(1);
        }
    }
    let normals: Vec<Vec3> = normals.into_iter().map(Vec3::normalize_or_zero).collect();
    let mut sharp = vec![false; point_count];
    for (points, normal) in face_points.iter().zip(&face_normals) {
        let Some(normal) = normal.try_normalize() else { continue };
        for &p in points {
            sharp[p as usize] |= normals[p as usize].dot(normal) < CREASE_COS;
        }
    }
    let mut subdivision = Subdivision {
        mesh,
        normals,
        sharp,
        edge_faces,
        points: FxHashMap::with_capacity_and_hasher(faces.len() * 2, FxBuildHasher),
        wedges: FxHashMap::with_capacity_and_hasher(faces.len() * 2, FxBuildHasher),
        out: SkeletalMesh { sections, ..mesh.clone() },
    };
    let mut indices = Vec::with_capacity(faces.len() * 12);
    for &[a, b, c] in faces {
        let ab = subdivision.wedge(a, b)?;
        let bc = subdivision.wedge(b, c)?;
        let ca = subdivision.wedge(c, a)?;
        indices.extend_from_slice(&[a, ab, ca, ab, b, bc, ca, bc, c, ab, bc, ca]);
    }
    let mut out = subdivision.out;
    out.indices = indices;
    Some(out)
}

struct Subdivision<'a> {
    mesh: &'a SkeletalMesh,
    normals: Vec<Vec3>,
    sharp: Vec<bool>,
    edge_faces: FxHashMap<(u32, u32), u8>,
    points: FxHashMap<(u32, u32), u32>,
    wedges: FxHashMap<(u16, u16), u16>,
    out: SkeletalMesh,
}

impl Subdivision<'_> {
    fn wedge(&mut self, a: u16, b: u16) -> Option<u16> {
        let key = low_first(a, b);
        if let Some(&wedge) = self.wedges.get(&key) {
            return Some(wedge);
        }
        let [wa, wb] = [a, b].map(|w| self.mesh.wedges[usize::from(w)]);
        let wedge = u16::try_from(self.out.wedges.len()).ok()?;
        let point = self.point(wa.point, wb.point);
        self.out.wedges.push(Wedge { point, uv: [(wa.uv[0] + wb.uv[0]) * 0.5, (wa.uv[1] + wb.uv[1]) * 0.5] });
        self.wedges.insert(key, wedge);
        Some(wedge)
    }

    fn point(&mut self, a: u32, b: u32) -> u32 {
        let key = low_first(a, b);
        let entry = match self.points.entry(key) {
            Entry::Occupied(found) => return *found.get(),
            Entry::Vacant(entry) => entry,
        };
        let (ia, ib) = (a as usize, b as usize);
        let ends = [ia, ib].map(|p| Vec3::from(self.mesh.positions[p]));
        let smooth = self.edge_faces.get(&key) == Some(&2) && !self.sharp[ia] && !self.sharp[ib];
        let at = if smooth { rounded_midpoint(ends, [self.normals[ia], self.normals[ib]]) } else { (ends[0] + ends[1]) * 0.5 };
        let point = self.out.positions.len() as u32;
        self.out.positions.push(at.to_array());
        let start = self.out.influences.len();
        for influence in self.mesh.influences_of(ia).iter().chain(self.mesh.influences_of(ib)) {
            let half = influence.weight * 0.5;
            match self.out.influences[start..].iter_mut().find(|merged| merged.bone == influence.bone) {
                Some(merged) => merged.weight += half,
                None => self.out.influences.push(Influence { bone: influence.bone, weight: half }),
            }
        }
        let run = &mut self.out.influences[start..];
        let total: f32 = run.iter().map(|i| i.weight).sum();
        if total > 0.0 {
            run.iter_mut().for_each(|i| i.weight /= total);
        }
        self.out.influence_starts.push(self.out.influences.len() as u32);
        *entry.insert(point)
    }
}

fn low_first<T: Ord + Copy>(a: T, b: T) -> (T, T) {
    (a.min(b), a.max(b))
}

fn rounded_midpoint([a, b]: [Vec3; 2], [na, nb]: [Vec3; 2]) -> Vec3 {
    let middle = (a + b) * 0.5;
    if na.dot(nb) < CREASE_COS {
        return middle;
    }
    let edge = b - a;
    let toward = (nb * edge.dot(nb) - na * edge.dot(na)) * (0.25 * ROUNDING);
    middle + toward.clamp_length_max(MOVE_CAP * edge.length())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2_math::{Rotator, Vector};

    fn mesh(positions: Vec<[f32; 3]>, wedges: Vec<Wedge>, indices: Vec<u16>, sections: Vec<SkelMeshSection>) -> SkeletalMesh {
        let points = positions.len() as u32;
        SkeletalMesh {
            materials: Vec::new(),
            scale: Vector::new(1.0, 1.0, 1.0),
            origin: Vector::ZERO,
            rot_origin: Rotator::ZERO,
            positions,
            influences: (0..points).map(|_| Influence { bone: 0, weight: 1.0 }).collect(),
            influence_starts: (0..=points).collect(),
            wedges,
            indices,
            sections,
            bones: Vec::new(),
            default_animation: 0,
            attach_aliases: Vec::new(),
        }
    }

    fn quad() -> SkeletalMesh {
        let wedge = |point, u, v| Wedge { point, uv: [u, v] };
        let wedges = vec![wedge(0, 0.0, 0.0), wedge(1, 1.0, 0.0), wedge(2, 1.0, 1.0), wedge(0, 0.5, 0.5), wedge(2, 0.9, 0.9), wedge(3, 0.0, 1.0)];
        let sections = vec![SkelMeshSection { material: 0, first_face: 0, num_faces: 1 }, SkelMeshSection { material: 1, first_face: 1, num_faces: 1 }];
        mesh(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]], wedges, vec![0, 1, 2, 3, 4, 5], sections)
    }

    #[test]
    fn seam_shares_the_new_point_and_flat_stays_flat() {
        let source = quad();
        let out = smoothed_skeletal_mesh(&source).unwrap();
        assert_eq!(out.indices.len(), 8 * 3);
        assert_eq!(out.positions.len(), 4 + 5);
        assert_eq!(out.wedges.len(), 6 + 6);
        assert_eq!(out.positions[..4], source.positions[..]);
        assert!(out.positions.iter().all(|p| p[2] == 0.0));
        let diagonal: Vec<&Wedge> = out.wedges[6..].iter().filter(|w| out.positions[w.point as usize] == [0.5, 0.5, 0.0]).collect();
        assert_eq!(diagonal.len(), 2);
        assert_eq!(diagonal[0].point, diagonal[1].point);
        assert_ne!(diagonal[0].uv, diagonal[1].uv);
        assert_eq!(out.influence_starts.len(), out.positions.len() + 1);
    }

    #[test]
    fn sections_stay_contiguous_and_cover_every_face() {
        let out = smoothed_skeletal_mesh(&quad()).unwrap();
        let ranges: Vec<(u16, u16)> = out.sections.iter().map(|s| (s.first_face, s.num_faces)).collect();
        assert_eq!(ranges, [(0, 4), (4, 4)]);
        assert_eq!(out.sections.iter().map(|s| usize::from(s.num_faces)).sum::<usize>() * 3, out.indices.len());
    }

    #[test]
    fn new_point_influences_merge_by_bone() {
        let mut source = quad();
        source.influences = vec![
            Influence { bone: 0, weight: 1.0 },
            Influence { bone: 0, weight: 1.0 },
            Influence { bone: 0, weight: 0.5 },
            Influence { bone: 1, weight: 0.5 },
            Influence { bone: 2, weight: 1.0 },
        ];
        source.influence_starts = vec![0, 1, 2, 4, 5];
        let out = smoothed_skeletal_mesh(&source).unwrap();
        let middle = out.positions.iter().position(|&p| p == [0.5, 0.5, 0.0]).unwrap();
        let influences = out.influences_of(middle);
        assert_eq!(influences, [Influence { bone: 0, weight: 0.75 }, Influence { bone: 1, weight: 0.25 }]);
        for point in 0..out.positions.len() {
            let total: f32 = out.influences_of(point).iter().map(|i| i.weight).sum();
            assert!((total - 1.0).abs() < 1e-6, "point {point}: {total}");
        }
    }

    #[test]
    fn crease_stays_straight_and_rounding_is_capped() {
        let ends = [Vec3::ZERO, Vec3::X * 10.0];
        let crease = rounded_midpoint(ends, [Vec3::Z, Vec3::new(1.0, 0.0, 0.2).normalize()]);
        assert_eq!(crease, Vec3::X * 5.0);
        let normals = |degrees: f32| {
            let tilt = degrees.to_radians();
            [Vec3::new(-tilt.sin(), 0.0, tilt.cos()), Vec3::new(tilt.sin(), 0.0, tilt.cos())]
        };
        let gentle = rounded_midpoint(ends, normals(10.0));
        assert!(gentle.z > 0.0 && gentle.z < MOVE_CAP * 10.0, "{gentle}");
        let capped = rounded_midpoint(ends, normals(28.0));
        assert!((capped.z - MOVE_CAP * 10.0).abs() < 1e-4, "{capped}");
        assert!((capped.x - 5.0).abs() < 1e-5);
    }

    #[test]
    fn oversized_mesh_is_refused() {
        let mut source = quad();
        source.sections[1].first_face = 20_000;
        assert!(smoothed_skeletal_mesh(&source).is_none());
        let triangles = 12_000u32;
        let positions = (0..triangles * 3).map(|i| [i as f32, (i % 3) as f32, 0.0]).collect();
        let wedges = (0..triangles * 3).map(|point| Wedge { point, uv: [0.0; 2] }).collect();
        let indices = (0..triangles * 3).map(|i| i as u16).collect();
        let sections = vec![SkelMeshSection { material: 0, first_face: 0, num_faces: triangles as u16 }];
        assert!(smoothed_skeletal_mesh(&mesh(positions, wedges, indices, sections)).is_none());
    }
}
