use bevy::math::{Mat4, Vec3, Vec4};
use bevy::transform::components::Transform;
use p2_math::{Rotator, Vector};

/// Unreal (X forward, Y right, Z up, left-handed) to Bevy (Y up, right-handed): swap Y and Z.
pub fn to_bevy(v: impl Into<[f32; 3]>) -> [f32; 3] {
    let [x, y, z] = v.into();
    [x, z, y]
}

fn unreal_vec3(v: Vector) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

pub fn unreal_transform(location: Vector, rotation: Rotator, scale: Vector, pre_pivot: Vector) -> Transform {
    let [x, y, z] = rotation.axes().map(unreal_vec3);
    let (x, y, z) = (x * scale.x, y * scale.y, z * scale.z);
    let Vector { x: px, y: py, z: pz } = pre_pivot;
    let translation = unreal_vec3(location) - (x * px + y * py + z * pz);
    let unreal = Mat4::from_cols(x.extend(0.0), y.extend(0.0), z.extend(0.0), translation.extend(1.0));
    unreal_to_bevy(unreal)
}

pub fn rotator_matrix(rotation: Rotator) -> Mat4 {
    let [x, y, z] = rotation.axes().map(|axis| unreal_vec3(axis).extend(0.0));
    Mat4::from_cols(x, y, z, Vec4::W)
}

/// world = `rotation` · R(`rot_origin`) · (scale ⊙ (v − `origin`)) + location + pre-pivot,
/// with scale = `DrawScale3D` · `DrawScale` · the mesh's `Scale`.
pub fn unreal_mesh_matrix(
    location: Vector,
    rotation: Mat4,
    scale: Vector,
    pre_pivot: Vector,
    mesh_scale: Vector,
    origin: Vector,
    rot_origin: Rotator,
) -> Mat4 {
    let scale = unreal_vec3(scale) * unreal_vec3(mesh_scale);
    Mat4::from_translation(unreal_vec3(location) + unreal_vec3(pre_pivot))
        * rotation
        * rotator_matrix(rot_origin)
        * Mat4::from_scale(scale)
        * Mat4::from_translation(-unreal_vec3(origin))
}

pub(crate) const AXIS_SWAP: Mat4 = Mat4::from_cols(Vec4::X, Vec4::Z, Vec4::Y, Vec4::W);

pub fn unreal_to_bevy(matrix: Mat4) -> Transform {
    Transform::from_matrix(AXIS_SWAP * matrix * AXIS_SWAP)
}

pub fn unreal_mesh_transform(
    location: Vector,
    rotation: Rotator,
    scale: Vector,
    pre_pivot: Vector,
    mesh_scale: Vector,
    origin: Vector,
    rot_origin: Rotator,
) -> Transform {
    unreal_to_bevy(unreal_mesh_matrix(location, rotator_matrix(rotation), scale, pre_pivot, mesh_scale, origin, rot_origin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Quat;

    #[test]
    fn unreal_placement() {
        let identity = unreal_transform([1.0, 2.0, 3.0].into(), [0; 3].into(), [1.0; 3].into(), [0.0; 3].into());
        assert!(identity.translation.abs_diff_eq(Vec3::new(1.0, 3.0, 2.0), 1e-5));
        assert!(identity.rotation.abs_diff_eq(Quat::IDENTITY, 1e-5));
        let yawed = unreal_transform([0.0; 3].into(), [0, 16384, 0].into(), [2.0; 3].into(), [0.0; 3].into());
        assert!((yawed.rotation * Vec3::X).abs_diff_eq(Vec3::Z, 1e-5));
        assert!(yawed.scale.abs_diff_eq(Vec3::splat(2.0), 1e-5));
        let pitched = unreal_transform([0.0; 3].into(), [16384, 0, 0].into(), [1.0; 3].into(), [0.0; 3].into());
        assert!((pitched.rotation * Vec3::X).abs_diff_eq(Vec3::Y, 1e-5));
        let pivoted = unreal_transform([5.0, 0.0, 0.0].into(), [0, 16384, 0].into(), [3.0; 3].into(), [10.0, 0.0, 0.0].into());
        assert!(pivoted.transform_point(Vec3::new(10.0, 0.0, 0.0)).abs_diff_eq(Vec3::new(5.0, 0.0, 0.0), 1e-4));
    }
}
