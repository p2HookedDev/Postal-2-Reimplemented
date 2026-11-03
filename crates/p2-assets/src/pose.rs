//! Pose maths for crates that must not depend on Bevy, in Unreal coordinates.
//! The root bone's quaternion is used as stored, every other bone's conjugated.

use p2_math::{Rotator, Vector};

use crate::mesh_animation::slerp;

pub type LocalBone = ([f32; 4], [f32; 3]);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub axes: [[f32; 3]; 3],
    pub origin: [f32; 3],
}

impl Frame {
    pub const IDENTITY: Self = Self { axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], origin: [0.0; 3] };

    pub fn transform_vector(&self, v: [f32; 3]) -> [f32; 3] {
        [0, 1, 2].map(|i| self.axes[0][i] * v[0] + self.axes[1][i] * v[1] + self.axes[2][i] * v[2])
    }

    pub fn transform_point(&self, p: [f32; 3]) -> [f32; 3] {
        let v = self.transform_vector(p);
        [0, 1, 2].map(|i| v[i] + self.origin[i])
    }

    /// `self` after `inner` (`self · inner`).
    pub fn then(&self, inner: &Self) -> Self {
        Self { axes: inner.axes.map(|axis| self.transform_vector(axis)), origin: self.transform_point(inner.origin) }
    }

    pub fn inverse(&self) -> Option<Self> {
        let [a, b, c] = self.axes.map(Vector::from);
        let determinant = a.dot(b.cross(c));
        if determinant.abs() < 1e-12 {
            return None;
        }
        let rows = [b.cross(c), c.cross(a), a.cross(b)].map(|r| r.map(|x| x / determinant));
        let axes = [0, 1, 2].map(|column| [rows[0][column], rows[1][column], rows[2][column]]);
        let origin = Vector::from(self.origin);
        let origin = rows.map(|r| -r.dot(origin));
        Some(Self { axes, origin })
    }

    pub fn from_quat(q: [f32; 4], scale: f32, position: [f32; 3]) -> Self {
        let [x, y, z, w] = q;
        let axes = [
            [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y)],
            [2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)],
            [2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)],
        ];
        Self { axes: axes.map(|a| a.map(|c| c * scale)), origin: position }
    }

    /// Rotation matrix of a rotator (pitch, yaw, roll in 65536ths of a turn). The angles are not wrapped
    /// as [`Rotator::axes`] wraps them, which moves the last bits of any negative or huge angle.
    pub fn from_rotator(rotation: [i32; 3]) -> Self {
        let axes = p2_math::axes_at(rotation.map(|u| p2_math::radians(u as f32)));
        Self { axes: axes.map(<[f32; 3]>::from), origin: [0.0; 3] }
    }

    fn scale(s: [f32; 3]) -> Self {
        Self { axes: [[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]], origin: [0.0; 3] }
    }

    fn translation(t: [f32; 3]) -> Self {
        Self { origin: t, ..Self::IDENTITY }
    }
}

pub fn bone_frames(parents: &[usize], local: &[LocalBone], scale: impl Fn(usize) -> f32) -> Vec<Frame> {
    let mut frames: Vec<Frame> = Vec::with_capacity(local.len());
    for (bone, &(q, position)) in local.iter().enumerate() {
        let q = normalize(q);
        let q = if bone == 0 { q } else { [-q[0], -q[1], -q[2], q[3]] };
        let frame = Frame::from_quat(q, scale(bone), position);
        let parent = parents.get(bone).copied().filter(|&p| p < bone).and_then(|p| frames.get(p));
        frames.push(parent.map_or(frame, |parent| parent.then(&frame)));
    }
    frames
}

pub fn subtree(parents: &[usize], start: Option<usize>) -> Vec<bool> {
    let start = start.filter(|&s| s < parents.len()).unwrap_or(0);
    let mut inside = vec![false; parents.len()];
    for bone in 0..parents.len() {
        inside[bone] = bone == start || (parents[bone] < bone && inside[parents[bone]]);
    }
    inside
}

/// A channel's blend weight: `BlendAlpha`, ramped from 0 while `AnimFrame` is below the channel's
/// `BlendInTime` (a frame still tweening in, below 0, weighs 0).
pub fn channel_weight(alpha: f32, frame: f32, blend_in_time: f32) -> f32 {
    if blend_in_time > 0.0 && frame < blend_in_time { alpha * frame.max(0.0) / blend_in_time } else { alpha }
}

/// A channel above 0 over the base channel: local-space blending, `bGlobalPose` ignored as retail.
/// Orientations blend the short way.
pub fn blend_local(base: &mut [LocalBone], over: &[LocalBone], alpha: f32, bones: &[bool]) {
    let alpha = alpha.clamp(0.0, 1.0);
    for ((base, over), _) in base.iter_mut().zip(over).zip(bones).filter(|(_, inside)| **inside) {
        blend_bone(base, over, alpha);
    }
}

fn blend_bone(base: &mut LocalBone, over: &LocalBone, alpha: f32) {
    let (a, mut b) = (normalize(base.0), normalize(over.0));
    if dot(a, b) < 0.0 {
        b = b.map(|c| -c);
    }
    let rotation = slerp(a, b, alpha);
    *base = (rotation, [0, 1, 2].map(|i| base.1[i] + (over.1[i] - base.1[i]) * alpha));
}

/// Moves `pose` from the kept pose `from` towards itself. A kept pose of another mesh is ignored.
pub fn tween_pose(from: &[LocalBone], pose: &mut [LocalBone], weight: f32) {
    if from.len() != pose.len() {
        return;
    }
    let weight = weight.clamp(0.0, 1.0);
    for (from, bone) in from.iter().zip(pose) {
        let mut mixed = *from;
        blend_bone(&mut mixed, bone, weight);
        *bone = mixed;
    }
}

/// world = R(`rotation`) · R(`rot_origin`) · (scale ⊙ (v − `origin`)) + `location` + `pre_pivot`,
/// with `scale` = `DrawScale3D` × `DrawScale` and `mesh_scale` the mesh's `Scale`.
pub fn mesh_to_world(
    location: Vector,
    rotation: Rotator,
    scale: Vector,
    pre_pivot: Vector,
    mesh_scale: Vector,
    origin: Vector,
    rot_origin: Rotator,
) -> Frame {
    Frame::translation((location + pre_pivot).into())
        .then(&Frame::from_rotator(rotation.into()))
        .then(&Frame::from_rotator(rot_origin.into()))
        .then(&Frame::scale((scale * mesh_scale).into()))
        .then(&Frame::translation((-origin).into()))
}

/// The rotator's matrix multiplies the stored local frame, so the root turns on its parent's side
/// and every other bone, conjugated, on its own side the other way.
pub fn turn_local(orientation: [f32; 4], turn: [i32; 3]) -> [f32; 4] {
    // Yaw about Z, then pitch about −Y, then roll about −X; half angles for the quaternions.
    let [pitch, yaw, roll] = turn.map(|u| u as f32 * std::f32::consts::PI / 65536.0);
    let about = |axis: usize, half: f32| {
        let mut q = [0.0, 0.0, 0.0, half.cos()];
        q[axis] = half.sin();
        q
    };
    multiply(multiply(multiply(about(2, yaw), about(1, -pitch)), about(0, -roll)), normalize(orientation))
}

fn multiply(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let ([ax, ay, az, aw], [bx, by, bz, bw]) = (a, b);
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

pub(crate) fn dot(a: [f32; 4], b: [f32; 4]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalize(q: [f32; 4]) -> [f32; 4] {
    let length = dot(q, q).sqrt();
    if length < 1e-8 { [0.0, 0.0, 0.0, 1.0] } else { q.map(|c| c / length) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-4)
    }

    #[test]
    fn child_follows_turned_root() {
        let half = std::f32::consts::FRAC_PI_4;
        let turn = [0.0, 0.0, half.sin(), half.cos()];
        let frames = bone_frames(&[0, 0], &[(turn, [0.0; 3]), ([0.0, 0.0, 0.0, 1.0], [10.0, 0.0, 0.0])], |_| 1.0);
        assert!(close(frames[1].origin, [0.0, 10.0, 0.0]), "{:?}", frames[1]);
        assert!(close(frames[1].axes[0], [0.0, 1.0, 0.0]));
    }

    #[test]
    fn turn_of_identity_is_the_rotator_matrix() {
        let turn = [3000, -12000, 7000];
        let turned = Frame::from_quat(turn_local([0.0, 0.0, 0.0, 1.0], turn), 1.0, [0.0; 3]);
        for (axis, expected) in turned.axes.iter().zip(Frame::from_rotator(turn).axes) {
            assert!(close(*axis, expected), "{turned:?}");
        }
    }

    #[test]
    fn turned_bones_move_as_the_engine_turns_them() {
        let identity = [0.0, 0.0, 0.0, 1.0];
        let quarter = [0, 16384, 0];
        let frames = bone_frames(&[0, 0], &[(turn_local(identity, quarter), [0.0; 3]), (identity, [10.0, 0.0, 0.0])], |_| 1.0);
        assert!(close(frames[1].origin, [0.0, 10.0, 0.0]), "{:?}", frames[1]);
        let frames = bone_frames(&[0, 0], &[(identity, [0.0; 3]), (turn_local(identity, quarter), [10.0, 0.0, 0.0])], |_| 1.0);
        assert!(close(frames[1].origin, [10.0, 0.0, 0.0]), "{:?}", frames[1]);
        assert!(close(frames[1].axes[0], [0.0, -1.0, 0.0]), "{:?}", frames[1]);
    }

    #[test]
    fn mesh_to_world_turns_and_moves() {
        let frame = mesh_to_world(Vector::new(100.0, 0.0, 0.0), Rotator::new(0, 16384, 0), Vector::new(1.0, 1.0, 1.0), Vector::new(0.0, 0.0, 5.0), Vector::new(2.0, 2.0, 2.0), Vector::ZERO, Rotator::ZERO);
        assert!(close(frame.transform_point([1.0, 0.0, 0.0]), [100.0, 2.0, 5.0]), "{:?}", frame.transform_point([1.0, 0.0, 0.0]));
    }

    #[test]
    fn inverse_undoes_the_frame() {
        let frame = mesh_to_world(Vector::new(100.0, -20.0, 7.0), Rotator::new(3000, 16384, -2000), Vector::new(1.5, 1.5, 1.5), Vector::new(0.0, 0.0, 5.0), Vector::new(2.0, 2.0, 2.0), Vector::new(1.0, 2.0, 3.0), Rotator::new(0, 100, 0));
        let back = frame.inverse().unwrap().then(&frame);
        for (axis, unit) in back.axes.iter().zip(Frame::IDENTITY.axes) {
            assert!(close(*axis, unit), "{back:?}");
        }
        assert!(close(back.origin, [0.0; 3]), "{back:?}");
    }

    #[test]
    fn a_tween_goes_from_the_kept_pose_to_the_target() {
        let from = [([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0])];
        let target = ([0.0, 0.0, 0.0, 1.0], [10.0, 0.0, 0.0]);
        for (weight, x) in [(0.0, 0.0), (0.25, 2.5), (1.0, 10.0)] {
            let mut pose = [target];
            tween_pose(&from, &mut pose, weight);
            assert!((pose[0].1[0] - x).abs() < 1e-4, "weight {weight}: {}", pose[0].1[0]);
        }
    }

    #[test]
    fn a_tween_from_another_mesh_changes_nothing() {
        let target = ([0.0, 0.0, 0.0, 1.0], [10.0, 0.0, 0.0]);
        let mut pose = [target];
        tween_pose(&[target, target], &mut pose, 0.0);
        assert_eq!(pose[0], target);
    }

    #[test]
    fn blending_half_way() {
        assert_eq!(channel_weight(0.8, -0.1, 0.5), 0.0);
        assert_eq!(channel_weight(0.8, 0.25, 0.5), 0.4);
        assert_eq!(channel_weight(0.8, 0.5, 0.5), 0.8);
        assert_eq!(channel_weight(0.8, -0.1, 0.0), 0.8);

        let mut base = vec![([0.0, 0.0, 0.0, 1.0], [0.0; 3])];
        let half = std::f32::consts::FRAC_PI_4;
        blend_local(&mut base, &[([0.0, 0.0, half.sin(), half.cos()], [10.0, 0.0, 0.0])], 0.5, &[true]);
        let quarter = std::f32::consts::FRAC_PI_8;
        assert!((base[0].0[2] - quarter.sin()).abs() < 1e-4 && (base[0].1[0] - 5.0).abs() < 1e-4);
    }
}
