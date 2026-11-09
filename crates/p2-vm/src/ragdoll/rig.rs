use p2_assets::pose::Frame;
use p2_assets::{KaConeType, KaJointKind, KaShape, KaTwistType, KarmaAsset};
use p2_physics::{JointLimit, JointParams, PartParams, Shape};
use p2_math::Vector;
use crate::bones::PosedMesh;
use p2_math::matrix_to_quat;
use super::{Built, CONE_SCALE, CUT_SCALE, MAX_CONE, MIN_TWIST, TrackedJoint};

pub(super) fn cut_of(own: Option<&[f32]>, parents: &[usize]) -> Vec<bool> {
    let Some(own) = own else { return Vec::new() };
    let mut effective = own.to_vec();
    for bone in 0..effective.len() {
        if let Some(parent) = parents.get(bone).copied().filter(|&p| p < bone) {
            effective[bone] *= effective[parent];
        }
    }
    effective.into_iter().map(|s| s.abs() < CUT_SCALE).collect()
}

pub(super) fn build(asset: &KarmaAsset, mesh: &PosedMesh, frames: &[Frame], velocity: Vector, cut: &[bool]) -> Option<Built> {
    let to_unreal = if asset.scale > 0.0 { 1.0 / asset.scale } else { 1.0 };
    let unreal = |v: Vector| v.map(|c| c * to_unreal);
    let mut parts = Vec::new();
    let mut part_of_bone = vec![None; mesh.names.len()];
    let mut part_names: Vec<&str> = Vec::new();
    let bodied = |name: &str| {
        mesh.names.iter().position(|n| n.eq_ignore_ascii_case(name)).is_some_and(|bone| !cut.get(bone).copied().unwrap_or(false))
            && asset
                .parts
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(name))
                .and_then(|p| asset.models.iter().find(|m| m.id == p.model))
                .is_some_and(|m| m.dynamic)
    };
    for (bone, name) in mesh.names.iter().enumerate() {
        if cut.get(bone).copied().unwrap_or(false) {
            continue;
        }
        let Some(part) = asset.parts.iter().find(|p| p.id.eq_ignore_ascii_case(name)) else { continue };
        let Some(model) = asset.models.iter().find(|m| m.id == part.model).filter(|m| m.dynamic) else { continue };
        let geometry = model.geometry.as_ref().filter(|_| model.collides).and_then(|g| asset.geometries.iter().find(|x| x.id == *g));
        let shapes = geometry.map_or_else(Vec::new, |g| {
            g.primitives
                .iter()
                .filter_map(|p| {
                    let origin = unreal([p.tm[12], p.tm[13], p.tm[14]].into());
                    match p.shape {
                        KaShape::Sphyl { radius, height } => {
                            let half = [p.tm[8], p.tm[9], p.tm[10]].map(|c| c * height * to_unreal / 2.0);
                            let (a, b) = ([0, 1, 2].map(|i| origin[i] - half[i]), [0, 1, 2].map(|i| origin[i] + half[i]));
                            Some(Shape::Capsule(a.into(), b.into(), radius * to_unreal))
                        }
                        KaShape::Sphere { radius } => Some(Shape::Sphere(origin, radius * to_unreal)),
                        KaShape::Other(_) => None,
                    }
                })
                .collect()
        });
        // Colliding with nothing, such a body falls through the level and takes the drawn top half with it.
        let jointed = || {
            asset.joints.iter().any(|joint| {
                let [a, b] = &joint.parts;
                (a.eq_ignore_ascii_case(&part.id) && bodied(b)) || (b.eq_ignore_ascii_case(&part.id) && bodied(a))
            })
        };
        if shapes.is_empty() && cut.contains(&true) && !jointed() {
            continue;
        }
        let frame = &frames[bone];
        // The file's inertia is a placeholder.
        let file_inertia = [model.inertia[0], model.inertia[3], model.inertia[5]].map(|i| i * asset.mass_scale * to_unreal * to_unreal).into();
        let inertia = p2_physics::shapes_inertia(&shapes, model.mass * asset.mass_scale).unwrap_or(file_inertia);
        part_of_bone[bone] = Some(parts.len());
        part_names.push(&part.id);
        parts.push(PartParams {
            shapes,
            location: frame.origin.into(),
            rotation: matrix_to_quat(orthonormal(frame.axes.map(Vector::from))),
            mass: model.mass * asset.mass_scale,
            center_of_mass: unreal(model.mass_offset.into()),
            inertia,
            velocity,
        });
    }
    if parts.is_empty() || (parts.len() < 2 && cut.contains(&true)) {
        return None;
    }
    let part_index = |name: &str| part_names.iter().position(|p| p.eq_ignore_ascii_case(name));
    let bone_of = |part: usize| part_of_bone.iter().position(|&p| p == Some(part));
    let unit = |bone: usize| {
        let axes = orthonormal(frames[bone].axes.map(Vector::from)).map(<[f32; 3]>::from);
        Frame { axes, origin: frames[bone].origin }
    };
    let mut joints = Vec::new();
    let mut tracked = Vec::new();
    for joint in &asset.joints {
        let (Some(child), Some(parent)) = (part_index(&joint.parts[0]), part_index(&joint.parts[1])) else { continue };
        let (Some(child_bone), Some(parent_bone)) = (bone_of(child), bone_of(parent)) else { continue };
        let limit = match joint.kind {
            KaJointKind::Hinge { low, high, limited, .. } => JointLimit::Hinge(limited.then_some([low, high])),
            // Cone half angle X about the third axis, Y about the orthogonal axis (unverified); a slot limits X only.
            KaJointKind::Skeletal { cone_type, cone_half_angles, twist_type, twist_half_angle, .. } => {
                let [x, y] = cone_half_angles.map(|a| (a * CONE_SCALE).min(MAX_CONE));
                JointLimit::Cone {
                    swing: match cone_type {
                        KaConeType::Free => None,
                        KaConeType::Slot => Some([0.0, x]),
                        KaConeType::Fixed => Some([0.0, 0.0]),
                        KaConeType::Cone => Some([y, x]),
                    },
                    twist: match twist_type {
                        KaTwistType::Free => None,
                        KaTwistType::Limited => Some(twist_half_angle.max(MIN_TWIST)),
                        KaTwistType::Fixed => Some(MIN_TWIST),
                    },
                }
            }
            KaJointKind::Other(_) => JointLimit::Cone { swing: None, twist: None },
        };
        // Retail takes the parent's end from the asset, up to ~8 units off the bones; ours has no jolt at the start.
        let child_anchor = unreal(joint.positions[0].into());
        let world_anchor = unit(child_bone).transform_point(child_anchor.into());
        let parent_anchor = unit(parent_bone).inverse().map_or(child_anchor, |inv| inv.transform_point(world_anchor).into());
        tracked.push(TrackedJoint { child: child_bone, parent: parent_bone, at_child: child_anchor, at_parent: parent_anchor });
        joints.push(JointParams {
            child,
            parent,
            child_anchor,
            child_axes: [joint.primary_axes[0].into(), joint.orthogonal_axes[0].into()],
            parent_anchor,
            parent_axes: [joint.primary_axes[1].into(), joint.orthogonal_axes[1].into()],
            limit,
        });
    }
    Some(Built { parts, joint_params: joints, part_of_bone, joints: tracked })
}

pub(super) fn orthonormal([x, y, _]: [Vector; 3]) -> [Vector; 3] {
    let unit = |a: Vector| a / a.size().max(1e-8);
    let x = unit(x);
    let y = unit(y - x * y.dot(x));
    [x, y, x.cross(y)]
}

/// A bone at 0 collapses its whole chain onto its joint. Parents come before their children.
pub(super) fn scale_frames(frames: &mut [Frame], own: &[f32], parents: &[usize]) {
    let original = frames.to_vec();
    let mut changed = vec![false; frames.len()];
    for bone in 0..frames.len() {
        let parent = parents.get(bone).copied().filter(|&p| p < bone);
        if let Some(p) = parent.filter(|&p| changed[p])
            && let Some(local) = original[p].inverse().map(|inverse| inverse.then(&original[bone]))
        {
            frames[bone] = frames[p].then(&local);
            changed[bone] = true;
        }
        let s = own.get(bone).copied().unwrap_or(1.0);
        if s != 1.0 {
            frames[bone].axes = frames[bone].axes.map(|a| a.map(|c| c * s));
            changed[bone] = true;
        }
    }
}

pub(super) fn frame_scale(frame: &Frame) -> f32 {
    let [x, y, z] = frame.axes[0];
    (x * x + y * y + z * z).sqrt().max(1e-6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orthonormal_keeps_x() {
        let [x, y, z] = orthonormal([[2.0, 0.0, 0.0].into(), [1.0, 3.0, 0.0].into(), [0.0; 3].into()]);
        assert_eq!((x, y, z), (Vector::new(1.0, 0.0, 0.0), Vector::new(0.0, 1.0, 0.0), Vector::new(0.0, 0.0, 1.0)));
    }

    #[test]
    fn a_scaled_bone_shrinks_its_chain_onto_its_joint() {
        let at = |x: f32| Frame { origin: [x, 0.0, 0.0], ..Frame::IDENTITY };
        let parents = [0, 0, 1, 0];
        let original = [at(0.0), at(10.0), at(20.0), at(5.0)];
        let mut frames = original;
        scale_frames(&mut frames, &[1.0, 0.5, 1.0, 1.0], &parents);
        assert_eq!(frames[2].origin, [15.0, 0.0, 0.0]);
        assert_eq!((frames[1].axes[0], frames[2].axes[0]), ([0.5, 0.0, 0.0], [0.5, 0.0, 0.0]));
        assert_eq!(frames[3], original[3]);
        let mut frames = original;
        scale_frames(&mut frames, &[1.0, 0.0, 1.0, 1.0], &parents);
        assert_eq!(frames[2].origin, [10.0, 0.0, 0.0]);
        assert_eq!(frames[2].axes, [[0.0; 3]; 3]);
    }
}
