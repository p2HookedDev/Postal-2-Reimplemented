use bevy::asset::RenderAssetUsages;
use bevy::math::{Mat4, Quat, Vec3};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::transform::components::Transform;
use p2_assets::SkeletalMesh;
use crate::coords::{AXIS_SWAP, to_bevy, unreal_to_bevy};

/// Mesh-space matrices (Unreal space), parents before children. As in the engine, the root's quaternion is used as
/// stored and every other bone's conjugated.
pub fn bone_matrices(parents: &[usize], local: &[([f32; 4], [f32; 3])]) -> Vec<Mat4> {
    scaled_bone_matrices(parents, local, |_| 1.0)
}

pub fn scaled_bone_matrices(parents: &[usize], local: &[([f32; 4], [f32; 3])], scale: impl Fn(usize) -> f32) -> Vec<Mat4> {
    let mut world = Vec::with_capacity(local.len());
    scaled_bone_matrices_into(parents, local, scale, &mut world);
    world
}

pub fn scaled_bone_matrices_into(parents: &[usize], local: &[([f32; 4], [f32; 3])], scale: impl Fn(usize) -> f32, world: &mut Vec<Mat4>) {
    world.clear();
    for (bone, &([x, y, z, w], position)) in local.iter().enumerate() {
        let rotation = Quat::from_xyzw(x, y, z, w).normalize();
        let rotation = if bone == 0 { rotation } else { rotation.conjugate() };
        let matrix = Mat4::from_scale_rotation_translation(Vec3::splat(scale(bone)), rotation, Vec3::from(position));
        let parent = parents.get(bone).copied().filter(|&p| p < bone).and_then(|p| world.get(p));
        let placed = parent.map_or(matrix, |parent| *parent * matrix);
        world.push(placed);
    }
}

/// The inverse bind poses and the bind transforms, in bone order.
pub fn skin_bind(mesh: &SkeletalMesh) -> (Vec<Mat4>, Vec<Transform>) {
    let binds = skin_bind_matrices(mesh);
    (binds.iter().map(|bind| AXIS_SWAP * bind.inverse() * AXIS_SWAP).collect(), binds.into_iter().map(unreal_to_bevy).collect())
}

fn skin_bind_matrices(mesh: &SkeletalMesh) -> Vec<Mat4> {
    let parents: Vec<usize> = mesh.bones.iter().map(|b| b.parent).collect();
    let local: Vec<_> = mesh.bones.iter().map(|b| (b.orientation, b.position)).collect();
    bone_matrices(&parents, &local)
}

pub fn skin_points(mesh: &SkeletalMesh, inverse_bind: &[Mat4], pose: &[Mat4]) -> Vec<[f32; 3]> {
    let moves: Vec<Mat4> = inverse_bind.iter().zip(pose).map(|(inverse, pose)| *pose * *inverse).collect();
    mesh.positions
        .iter()
        .zip(mesh.point_influences())
        .map(|(&point, influences)| {
            let point = Vec3::from(point);
            let skinned: Vec3 = influences
                .iter()
                .filter_map(|i| moves.get(usize::from(i.bone)).map(|m| m.transform_point3(point) * i.weight))
                .sum();
            skinned.to_array()
        })
        .collect()
}
/// Index order is kept, like static meshes.
/// The meshes stay in the main world too, so animation can move them.
pub fn skeletal_mesh_to_meshes(mesh: &SkeletalMesh, gpu_skin: bool) -> Vec<(Mesh, u16)> {
    let position = |point: u32| mesh.positions.get(point as usize).copied().unwrap_or_default();
    let mut point_normals = vec![Vec3::ZERO; mesh.positions.len()];
    for face in mesh.indices.as_chunks::<3>().0 {
        if let [Some(a), Some(b), Some(c)] = face.map(|w| mesh.wedges.get(usize::from(w)).map(|w| w.point)) {
            let [pa, pb, pc] = [a, b, c].map(|p| Vec3::from(to_bevy(position(p))));
            let normal = (pb - pa).cross(pc - pa);
            for p in [a, b, c] {
                if let Some(n) = point_normals.get_mut(p as usize) {
                    *n += normal;
                }
            }
        }
    }
    let positions: Vec<[f32; 3]> = mesh.wedges.iter().map(|w| to_bevy(position(w.point))).collect();
    let normals: Vec<[f32; 3]> = mesh
        .wedges
        .iter()
        .map(|w| point_normals.get(w.point as usize).copied().unwrap_or(Vec3::Y).normalize_or(Vec3::Y).to_array())
        .collect();
    let uvs: Vec<[f32; 2]> = mesh.wedges.iter().map(|w| w.uv).collect();
    let (joints, weights): (Vec<[u16; 4]>, Vec<[f32; 4]>) = if gpu_skin {
        let mut sorted = Vec::new();
        let per_point: Vec<([u16; 4], [f32; 4])> = mesh
            .point_influences()
            .map(|influences| {
                sorted.clear();
                sorted.extend_from_slice(influences);
                sorted.sort_by(|a, b| b.weight.total_cmp(&a.weight));
                let (mut joints, mut weights) = ([0u16; 4], [0f32; 4]);
                for (i, influence) in sorted.iter().take(4).enumerate() {
                    (joints[i], weights[i]) = (influence.bone, influence.weight);
                }
                let total: f32 = weights.iter().sum();
                if total > 0.0 {
                    weights = weights.map(|w| w / total);
                } else {
                    weights[0] = 1.0;
                }
                (joints, weights)
            })
            .collect();
        let unweighted = ([0u16; 4], [1.0, 0.0, 0.0, 0.0]);
        mesh.wedges.iter().map(|w| per_point.get(w.point as usize).copied().unwrap_or(unweighted)).unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    mesh.sections
        .iter()
        .map(|section| {
            let start = usize::from(section.first_face) * 3;
            let end = (start + usize::from(section.num_faces) * 3).min(mesh.indices.len());
            let indices = mesh.indices.get(start..end).unwrap_or_default().to_vec();
            let mut out = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            out.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
            out.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals.clone());
            out.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone());
            // Only for meshes drawn with a `SkinnedMesh`: Bevy draws any mesh with joints as skinned.
            if gpu_skin {
                out.insert_attribute(Mesh::ATTRIBUTE_JOINT_INDEX, VertexAttributeValues::Uint16x4(joints.clone()));
                out.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weights.clone());
            }
            out.insert_indices(Indices::U16(indices));
            if gpu_skin {
                let _ = out.generate_skinned_mesh_bounds();
            }
            (out, section.material)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p2_math::{Rotator, Vector};

    #[test]
    fn bone_scale_reaches_children() {
        let local = [([0.0, 0.0, 0.0, 1.0], [0.0; 3]), ([0.0, 0.0, 0.0, 1.0], [10.0, 0.0, 0.0])];
        let scaled = scaled_bone_matrices(&[0, 0], &local, |bone| if bone == 0 { 2.0 } else { 1.0 });
        assert!(scaled[1].transform_point3(Vec3::ZERO).abs_diff_eq(Vec3::new(20.0, 0.0, 0.0), 1e-5));
        assert_eq!(bone_matrices(&[0, 0], &local)[1].transform_point3(Vec3::ZERO), Vec3::new(10.0, 0.0, 0.0));
    }

    #[test]
    fn gpu_joints_match_cpu_skinning() {
        use p2_assets::{Bone, Influence};
        let bone = |parent, orientation: [f32; 4], position| Bone { name: 0, parent, num_children: 0, orientation, position };
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let mesh = SkeletalMesh {
            materials: Vec::new(),
            scale: Vector::new(1.0, 1.0, 1.0),
            origin: Vector::ZERO,
            rot_origin: Rotator::ZERO,
            positions: vec![[10.0, 20.0, 30.0]],
            influences: vec![Influence { bone: 0, weight: 0.25 }, Influence { bone: 1, weight: 0.75 }],
            influence_starts: vec![0, 2],
            wedges: Vec::new(),
            indices: Vec::new(),
            sections: Vec::new(),
            bones: vec![bone(0, [0.0, 0.0, 0.0, 1.0], [1.0, 2.0, 3.0]), bone(0, [0.0, 0.0, half, half], [5.0, 0.0, 0.0])],
            default_animation: 0,
            attach_aliases: Vec::new(),
        };
        let parents = [0, 0];
        let posed = [([0.0, half, 0.0, half], [4.0, 0.0, -2.0]), ([half, 0.0, 0.0, half], [0.0, 7.0, 0.0])];
        let pose = bone_matrices(&parents, &posed);
        let bind = bone_matrices(&parents, &mesh.bones.iter().map(|b| (b.orientation, b.position)).collect::<Vec<_>>());
        let cpu = Vec3::from(to_bevy(skin_points(&mesh, &bind.iter().map(Mat4::inverse).collect::<Vec<_>>(), &pose)[0]));
        let (inverse, transforms) = skin_bind(&mesh);
        let vertex = Vec3::from(to_bevy(mesh.positions[0]));
        let gpu: Vec3 = mesh
            .influences_of(0)
            .iter()
            .map(|i| (unreal_to_bevy(pose[usize::from(i.bone)]).to_matrix() * inverse[usize::from(i.bone)]).transform_point3(vertex) * i.weight)
            .sum();
        assert!(cpu.distance(gpu) < 1e-3, "cpu {cpu} gpu {gpu}");
        for (at, inverse) in transforms.iter().zip(&inverse) {
            let moved = (at.to_matrix() * *inverse).transform_point3(vertex);
            assert!(moved.distance(vertex) < 1e-3, "{moved} vs {vertex}");
        }
    }
}
