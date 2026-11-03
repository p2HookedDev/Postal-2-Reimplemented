use p2_math::Vector;
use p2_package::{Export, Package, Reader};

use crate::Error;
use crate::pose::{LocalBone, dot};
use crate::read::{array, invalid};

/// Rotation keys: the root bone's quaternion rotates as stored (`v' = q v q*`), every other bone's
/// as its conjugate.
#[derive(Debug, Clone)]
pub struct MeshAnimation {
    pub version: i32,
    pub bones: Vec<NamedBone>,
    pub sequences: Vec<AnimSequence>,
}

#[derive(Debug, Clone)]
pub struct NamedBone {
    pub name: usize,
    pub flags: u32,
    pub parent: i32,
}

#[derive(Debug, Clone)]
pub struct AnimSequence {
    pub name: usize,
    pub groups: Vec<usize>,
    pub start_frame: i32,
    pub num_frames: i32,
    pub rate: f32,
    pub notifies: Vec<AnimNotify>,
    pub root_speed: Vector,
    pub track_time: f32,
    pub start_bone: i32,
    pub flags: u32,
    pub bone_indices: Vec<i32>,
    /// Track `i` animates [`MeshAnimation::bones`]`[i]`. Some sequences store more tracks than bones; the engine
    /// never reads the extra ones.
    pub tracks: Vec<AnalogTrack>,
    pub root_track: AnalogTrack,
}

#[derive(Debug, Clone)]
pub struct AnimNotify {
    pub time: f32,
    pub function: usize,
    pub object: i32,
}

#[derive(Debug, Clone, Default)]
pub struct AnalogTrack {
    pub flags: u32,
    pub rotations: Vec<[f32; 4]>,
    pub positions: Vec<[f32; 3]>,
    pub times: Vec<f32>,
    /// `times` never fall and hold no NaN, as checked when read: `sample` may bisect them.
    pub times_sorted: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PoseAt<'a> {
    tracks: &'a [AnalogTrack],
    track_time: f32,
    time: f32,
}

impl PoseAt<'_> {
    pub fn get(&self, track: usize) -> Option<LocalBone> {
        self.tracks.get(track).map(|t| t.sample(self.track_time, self.time))
    }
}

impl MeshAnimation {
    /// `time` in frames wraps at `track_time`; the last key blends into the first, as retail.
    /// Empty when `sequence` is out of range.
    pub fn pose(&self, sequence: usize, time: f32) -> Vec<LocalBone> {
        let at = self.pose_at(sequence, time);
        at.tracks.iter().map(|track| track.sample(at.track_time, at.time)).collect()
    }

    pub fn pose_at(&self, sequence: usize, time: f32) -> PoseAt<'_> {
        let Some(seq) = self.sequences.get(sequence) else { return PoseAt { tracks: &[], track_time: 0.0, time: 0.0 } };
        let time = if seq.track_time > 0.0 { time.rem_euclid(seq.track_time) } else { 0.0 };
        PoseAt { tracks: &seq.tracks[..seq.tracks.len().min(self.bones.len())], track_time: seq.track_time, time }
    }
}

impl AnalogTrack {
    fn sample(&self, track_time: f32, time: f32) -> ([f32; 4], [f32; 3]) {
        let count = self.times.len();
        let key = if self.times_sorted && !time.is_nan() {
            self.times.get(1..).map_or(0, |later| later.partition_point(|&t| t <= time))
        } else {
            self.times.iter().skip(1).position(|&t| time < t).unwrap_or(count.saturating_sub(1))
        };
        let wraps = key + 1 >= count;
        let next = if wraps { 0 } else { key + 1 };
        let mut alpha = 0.0;
        if next != key {
            let start = self.times[key];
            let interval = if wraps { track_time - start } else { (self.times[next] - start).abs() };
            alpha = if interval > 1e-4 { (time - start) / interval } else { 1.0 };
        }
        let rotation = |i| key_at(&self.rotations, i).unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let position = |i| key_at(&self.positions, i).unwrap_or([0.0; 3]);
        if alpha == 0.0 {
            return (rotation(key), position(key));
        }
        let from = rotation(key);
        let mut to = rotation(next);
        // Wrapping to the first key: take the shorter way (consecutive keys are stored aligned).
        if wraps && dot(from, to) < 0.0 {
            to = to.map(|c| -c);
        }
        let (p, q) = (position(key), position(next));
        (slerp(from, to, alpha), [0, 1, 2].map(|i| p[i] + (q[i] - p[i]) * alpha))
    }
}

fn key_at<T: Copy>(keys: &[T], i: usize) -> Option<T> {
    if keys.len() < 2 { keys.first().copied() } else { keys.get(i).copied() }
}

/// As retail: `a` when nearly equal, linear for small angles, spherical from cos 0.55 down.
/// Normalized. It does not flip `b` to `a`'s hemisphere.
pub(crate) fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let cos = dot(a, b);
    if cos > 0.999_999_9 {
        return a;
    }
    let (wa, wb) = if cos <= 0.55 {
        let angle = cos.max(-1.0).acos();
        let sin = angle.sin();
        (((1.0 - t) * angle).sin() / sin, (t * angle).sin() / sin)
    } else {
        (1.0 - t, t)
    };
    let q = [0, 1, 2, 3].map(|i| a[i] * wa + b[i] * wb);
    let length_sq = dot(q, q);
    if length_sq < 1e-5 {
        return a;
    }
    q.map(|c| c / length_sq.sqrt())
}

pub fn read_mesh_animation(pkg: &Package, file: &[u8], export: &Export) -> Result<MeshAnimation, Error> {
    let mut r = pkg.native_reader(file, export)?;
    let r = &mut r;
    let version = r.i32()?;
    let bones = array(r, |r| Ok(NamedBone { name: pkg.read_name(r)?, flags: r.u32()?, parent: r.i32()? }))?;
    let moves_offset = r.pos();
    let moves = array(r, read_motion_chunk)?;
    let sequences = array(r, |r| read_sequence(pkg, r))?;
    if !r.at_end() {
        return Err(invalid("mesh animation end", r.pos()));
    }
    if moves.len() != sequences.len() {
        return Err(invalid("mesh animation motion chunk count", moves_offset));
    }
    let sequences = sequences
        .into_iter()
        .zip(moves)
        .map(|(seq, chunk)| AnimSequence {
            name: seq.name,
            groups: seq.groups,
            start_frame: seq.start_frame,
            num_frames: seq.num_frames,
            rate: seq.rate,
            notifies: seq.notifies,
            root_speed: chunk.root_speed,
            track_time: chunk.track_time,
            start_bone: chunk.start_bone,
            flags: chunk.flags,
            bone_indices: chunk.bone_indices,
            tracks: chunk.tracks,
            root_track: chunk.root_track,
        })
        .collect();
    Ok(MeshAnimation { version, bones, sequences })
}

struct MotionChunk {
    root_speed: Vector,
    track_time: f32,
    start_bone: i32,
    flags: u32,
    bone_indices: Vec<i32>,
    tracks: Vec<AnalogTrack>,
    root_track: AnalogTrack,
}

fn read_motion_chunk(r: &mut Reader) -> Result<MotionChunk, Error> {
    Ok(MotionChunk {
        root_speed: Vector::new(r.f32()?, r.f32()?, r.f32()?),
        track_time: r.f32()?,
        start_bone: r.i32()?,
        flags: r.u32()?,
        bone_indices: array(r, |r| Ok(r.i32()?))?,
        tracks: array(r, read_track)?,
        root_track: read_track(r)?,
    })
}

fn read_track(r: &mut Reader) -> Result<AnalogTrack, Error> {
    let flags = r.u32()?;
    let rotations = array(r, |r| Ok([r.f32()?, r.f32()?, r.f32()?, r.f32()?]))?;
    let positions = array(r, |r| Ok([r.f32()?, r.f32()?, r.f32()?]))?;
    let times: Vec<f32> = array(r, |r| Ok(r.f32()?))?;
    let times_sorted = times.windows(2).all(|pair| pair[0] <= pair[1]);
    Ok(AnalogTrack { flags, rotations, positions, times, times_sorted })
}

struct Sequence {
    name: usize,
    groups: Vec<usize>,
    start_frame: i32,
    num_frames: i32,
    rate: f32,
    notifies: Vec<AnimNotify>,
}

fn read_sequence(pkg: &Package, r: &mut Reader) -> Result<Sequence, Error> {
    // Version >= 115: 4 bytes, 0 in 3031 sequences, a float in 0..1 in the rest (meaning unknown).
    r.u32()?;
    let name = pkg.read_name(r)?;
    let groups = array(r, |r| Ok(pkg.read_name(r)?))?;
    let start_frame = r.i32()?;
    let num_frames = r.i32()?;
    let notifies = array(r, |r| Ok(AnimNotify { time: r.f32()?, function: pkg.read_name(r)?, object: r.compact()? }))?;
    let rate = r.f32()?;
    Ok(Sequence { name, groups, start_frame, num_frames, rate, notifies })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> AnalogTrack {
        let half = std::f32::consts::FRAC_1_SQRT_2;
        AnalogTrack {
            flags: 0,
            rotations: vec![[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, half, half]],
            positions: vec![[0.0; 3], [10.0, 0.0, 0.0]],
            times: vec![0.0, 2.0],
            times_sorted: true,
        }
    }

    #[test]
    fn bisection_matches_the_scan() {
        let mut sorted = track();
        sorted.times = vec![0.0, 1.0, 1.0, 3.0];
        sorted.rotations = vec![[0.0, 0.0, 0.0, 1.0]; 4];
        sorted.positions = vec![[0.0; 3], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0], [3.0, 0.0, 0.0]];
        let mut scanned = sorted.clone();
        scanned.times_sorted = false;
        for time in [0.0, 0.5, 1.0, 2.0, 3.0, 3.5] {
            assert_eq!(sorted.sample(4.0, time), scanned.sample(4.0, time), "{time}");
        }
    }

    #[test]
    fn samples_between_keys() {
        let (q, p) = track().sample(4.0, 1.0);
        assert!((p[0] - 5.0).abs() < 1e-5);
        let angle = 2.0 * q[3].acos();
        assert!((angle - std::f32::consts::FRAC_PI_4).abs() < 1e-3, "{angle}");
        assert!((dot(q, q) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn wraps_into_first_key() {
        let (_, p) = track().sample(4.0, 3.0);
        assert!((p[0] - 5.0).abs() < 1e-5);
        let (q, p) = track().sample(4.0, 0.0);
        assert_eq!((q, p), ([0.0, 0.0, 0.0, 1.0], [0.0; 3]));
    }

    #[test]
    fn constant_position_key() {
        let mut t = track();
        t.positions.truncate(1);
        assert_eq!(t.sample(4.0, 1.0).1, [0.0; 3]);
    }
}
