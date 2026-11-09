//! Vectors use Unreal axes (X forward, Y right, Z up); rotators 65536ths of a turn (pitch, yaw, roll).

mod rng;
pub use rng::Rng;

/// Retail converts some radians to rotation units with 65535 units per turn, not 65536.
pub const RETAIL_UNITS_PER_TURN: f32 = 65535.0;
pub const QUARTER_TURN: i32 = 16384;
/// Row spacing of a hexagonal grid (√3 / 2 as retail rounds it).
pub const HEX_ROW_SPACING: f32 = 0.866_025;

#[inline]
pub fn radians(units: i32) -> f32 {
    units as f32 * std::f32::consts::TAU / 65536.0
}

#[inline]
pub fn units_rounded(radians: f32) -> i32 {
    (radians * 32768.0 / std::f32::consts::PI).round() as i32
}

#[inline]
pub fn units_truncated(radians: f32) -> i32 {
    (radians * 32768.0 / std::f32::consts::PI) as i32
}

#[inline]
pub fn mul_byte_round(a: u8, b: u8) -> u8 {
    ((u16::from(a) * u16::from(b) + 127) / 255) as u8
}

#[inline]
pub fn mul_byte_floor(a: u8, b: u8) -> u8 {
    (u16::from(a) * u16::from(b) / 255) as u8
}

#[inline]
pub fn rotate_by(axes: &[Vector; 3], v: Vector) -> Vector {
    axes[0] * v.x + axes[1] * v.y + axes[2] * v.z
}

#[inline]
pub fn unrotate_by(axes: &[Vector; 3], v: Vector) -> Vector {
    Vector::new(axes[0].dot(v), axes[1].dot(v), axes[2].dot(v))
}

/// Quaternions are `[x, y, z, w]`.
#[inline]
pub fn rotator_to_quat(rotation: Rotator) -> [f32; 4] {
    matrix_to_quat(rotation.axes())
}

#[inline]
pub fn matrix_to_quat([x, y, z]: [Vector; 3]) -> [f32; 4] {
    let m = [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]];
    let trace = m[0][0] + m[1][1] + m[2][2];
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s, s / 4.0]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [s / 4.0, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s, (m[2][1] - m[1][2]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [(m[0][1] + m[1][0]) / s, s / 4.0, (m[1][2] + m[2][1]) / s, (m[0][2] - m[2][0]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [(m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, s / 4.0, (m[1][0] - m[0][1]) / s]
    }
}

#[inline]
pub fn quat_to_rotator(q: [f32; 4]) -> Rotator {
    let [x, y, z] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|v| quat_rotate(q, v.into()));
    [units_rounded(x[2].atan2(x[0].hypot(x[1]))), units_rounded(x[1].atan2(x[0])), units_rounded((-y[2]).atan2(z[2]))].into()
}

#[inline]
fn quat_rotate([qx, qy, qz, w]: [f32; 4], v: Vector) -> Vector {
    let u = Vector::new(qx, qy, qz);
    let t = u.cross(v) * 2.0;
    let ut = u.cross(t);
    v + t * w + ut
}

#[inline]
pub fn color_byte(unit: f32) -> u8 {
    (unit.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// An angle of a rotator in radians, taken within one turn as the engine's sine table reads it (`& 0xffff`): huge
/// accumulated angles keep their precision.
#[inline]
pub fn turn(units: i32) -> f32 {
    radians(units & 0xffff)
}

#[inline]
pub fn axes_at(radians: [f32; 3]) -> [Vector; 3] {
    let [(sp, cp), (sy, cy), (sr, cr)] = radians.map(f32::sin_cos);
    [
        Vector::new(cp * cy, cp * sy, sp),
        Vector::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp),
        Vector::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp),
    ]
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    #[inline]
    pub fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    #[inline]
    pub fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    #[inline]
    pub fn size(self) -> f32 {
        self.size_squared().sqrt()
    }

    #[inline]
    pub fn size_squared(self) -> f32 {
        self.dot(self)
    }

    #[inline]
    pub fn safe_normal(self) -> Self {
        let squared = self.size_squared();
        if squared < 1e-8 { Self::ZERO } else { self * (1.0 / squared.sqrt()) }
    }

    #[inline]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        self + (other - self) * t
    }

    #[inline]
    pub fn map(self, f: impl Fn(f32) -> f32) -> Self {
        Self::new(f(self.x), f(self.y), f(self.z))
    }

    #[inline]
    pub fn mirror_by_normal(self, normal: Self) -> Self {
        self - normal * (2.0 * self.dot(normal))
    }

    /// Pitch and yaw along the vector (roll 0), `atan2 * 65535 / 2π` in double, truncated and not wrapped,
    /// so a yaw runs from -32767 to 32767.
    #[inline]
    pub fn rotation(self) -> Rotator {
        let Self { x, y, z } = self;
        let units = |radians: f64| (radians * f64::from(RETAIL_UNITS_PER_TURN) / std::f64::consts::TAU) as i32;
        let horizontal = f64::from(x * x + y * y).sqrt();
        Rotator::new(units(f64::from(z).atan2(horizontal)), units(f64::from(y).atan2(f64::from(x))), 0)
    }
}

impl From<[f32; 3]> for Vector {
    #[inline]
    fn from([x, y, z]: [f32; 3]) -> Self {
        Self { x, y, z }
    }
}

impl From<Vector> for [f32; 3] {
    #[inline]
    fn from(v: Vector) -> Self {
        [v.x, v.y, v.z]
    }
}

impl std::ops::Index<usize> for Vector {
    type Output = f32;
    #[inline]
    fn index(&self, i: usize) -> &f32 {
        match i {
            0 => &self.x,
            1 => &self.y,
            2 => &self.z,
            _ => panic!("Vector index {i} out of range"),
        }
    }
}

impl std::ops::IndexMut<usize> for Vector {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut f32 {
        match i {
            0 => &mut self.x,
            1 => &mut self.y,
            2 => &mut self.z,
            _ => panic!("Vector index {i} out of range"),
        }
    }
}

impl std::ops::Add for Vector {
    type Output = Self;
    #[inline]
    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl std::ops::Sub for Vector {
    type Output = Self;
    #[inline]
    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl std::ops::Neg for Vector {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

impl std::ops::Mul<f32> for Vector {
    type Output = Self;
    #[inline]
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}

impl std::ops::Mul for Vector {
    type Output = Self;
    #[inline]
    fn mul(self, other: Self) -> Self {
        Self::new(self.x * other.x, self.y * other.y, self.z * other.z)
    }
}

impl std::ops::Div<f32> for Vector {
    type Output = Self;
    #[inline]
    fn div(self, s: f32) -> Self {
        Self::new(self.x / s, self.y / s, self.z / s)
    }
}

impl std::ops::AddAssign for Vector {
    #[inline]
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl std::ops::SubAssign for Vector {
    #[inline]
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

impl std::ops::MulAssign<f32> for Vector {
    #[inline]
    fn mul_assign(&mut self, s: f32) {
        *self = *self * s;
    }
}

impl std::ops::MulAssign for Vector {
    #[inline]
    fn mul_assign(&mut self, other: Self) {
        *self = *self * other;
    }
}

impl std::ops::DivAssign<f32> for Vector {
    #[inline]
    fn div_assign(&mut self, s: f32) {
        *self = *self / s;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rotator {
    pub pitch: i32,
    pub yaw: i32,
    pub roll: i32,
}

impl Rotator {
    pub const ZERO: Self = Self::new(0, 0, 0);

    #[inline]
    pub const fn new(pitch: i32, yaw: i32, roll: i32) -> Self {
        Self { pitch, yaw, roll }
    }

    #[inline]
    pub fn axes(self) -> [Vector; 3] {
        axes_at([self.pitch, self.yaw, self.roll].map(turn))
    }

    #[inline]
    pub fn rotate(self, v: Vector) -> Vector {
        rotate_by(&self.axes(), v)
    }

    #[inline]
    pub fn unrotate(self, v: Vector) -> Vector {
        unrotate_by(&self.axes(), v)
    }

    #[inline]
    pub fn scale(self, s: f32) -> Self {
        self.map(|c| (c as f32 * s) as i32)
    }

    #[inline]
    pub fn vector(self) -> Vector {
        let (pitch, yaw) = (turn(self.pitch), turn(self.yaw));
        Vector::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin())
    }

    #[inline]
    pub fn map(self, f: impl Fn(i32) -> i32) -> Self {
        Self::new(f(self.pitch), f(self.yaw), f(self.roll))
    }
}

impl std::ops::Index<usize> for Rotator {
    type Output = i32;
    #[inline]
    fn index(&self, i: usize) -> &i32 {
        match i {
            0 => &self.pitch,
            1 => &self.yaw,
            2 => &self.roll,
            _ => panic!("Rotator index {i} out of range"),
        }
    }
}

impl std::ops::IndexMut<usize> for Rotator {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut i32 {
        match i {
            0 => &mut self.pitch,
            1 => &mut self.yaw,
            2 => &mut self.roll,
            _ => panic!("Rotator index {i} out of range"),
        }
    }
}

impl std::ops::Add for Rotator {
    type Output = Self;
    #[inline]
    fn add(self, other: Self) -> Self {
        Self::new(self.pitch.wrapping_add(other.pitch), self.yaw.wrapping_add(other.yaw), self.roll.wrapping_add(other.roll))
    }
}

impl std::ops::Sub for Rotator {
    type Output = Self;
    #[inline]
    fn sub(self, other: Self) -> Self {
        Self::new(self.pitch.wrapping_sub(other.pitch), self.yaw.wrapping_sub(other.yaw), self.roll.wrapping_sub(other.roll))
    }
}

impl From<[i32; 3]> for Rotator {
    #[inline]
    fn from([pitch, yaw, roll]: [i32; 3]) -> Self {
        Self { pitch, yaw, roll }
    }
}

impl From<Rotator> for [i32; 3] {
    #[inline]
    fn from(r: Rotator) -> Self {
        [r.pitch, r.yaw, r.roll]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: Vector, b: Vector) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    #[test]
    fn axes_follow_the_engine_conventions() {
        assert!(near(Rotator::new(0, 16384, 0).rotate(Vector::new(1.0, 0.0, 0.0)), Vector::new(0.0, 1.0, 0.0)));
        assert!(near(Rotator::new(16384, 0, 0).vector(), Vector::new(0.0, 0.0, 1.0)));
        assert!(near(Rotator::new(0, 0, 16384).axes()[1], Vector::new(0.0, 0.0, -1.0)));
    }

    #[test]
    fn rotation_truncates_and_does_not_wrap() {
        assert_eq!(Vector::new(1.0, 0.0, 0.0).rotation(), Rotator::new(0, 0, 0));
        assert_eq!(Vector::new(0.0, 1.0, 0.0).rotation(), Rotator::new(0, 16383, 0));
        assert_eq!(Vector::new(0.0, -1.0, 0.0).rotation(), Rotator::new(0, -16383, 0));
        assert_eq!(Vector::new(-1.0, 0.0, 0.0).rotation(), Rotator::new(0, 32767, 0));
        assert_eq!(Vector::new(0.0, 0.0, 1.0).rotation(), Rotator::new(16383, 0, 0));
        assert_eq!(Vector::new(0.0, 0.0, -1.0).rotation(), Rotator::new(-16383, 0, 0));
    }

    #[test]
    fn rotator_quaternion_round_trip() {
        for r in [[0, 0, 0], [0, 16384, 0], [4000, -9000, 2000], [-12000, 30000, -15000], [8192, 0, 32000]] {
            let q = rotator_to_quat(r.into());
            for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, -0.5, 0.8]] {
                let (a, b) = (quat_rotate(q, v.into()), Rotator::from(r).rotate(v.into()));
                assert!((0..3).all(|i| (a[i] - b[i]).abs() < 1e-4), "{r:?} {v:?}: {a:?} vs {b:?}");
            }
            let back = quat_to_rotator(q);
            assert!((0..3).all(|i| (back[i] - r[i]).rem_euclid(65536).min((r[i] - back[i]).rem_euclid(65536)) <= 2), "{r:?} -> {back:?}");
        }
    }

    #[test]
    fn safe_normal_is_zero_for_tiny_vectors() {
        assert_eq!(Vector::new(5e-5, 0.0, 0.0).safe_normal(), Vector::ZERO);
        assert!(near(Vector::new(0.0, 3.0, 4.0).safe_normal(), Vector::new(0.0, 0.6, 0.8)));
    }

    #[test]
    fn operators_are_per_component() {
        let (a, b) = (Vector::new(1.0, -2.0, 3.5), Vector::new(0.25, 4.0, -1.0));
        assert_eq!(a + b, Vector::new(1.25, 2.0, 2.5));
        assert_eq!(a - b, Vector::new(0.75, -6.0, 4.5));
        assert_eq!(a * b, Vector::new(0.25, -8.0, -3.5));
        assert_eq!(a * 2.0, Vector::new(2.0, -4.0, 7.0));
        assert_eq!(a.dot(b), 0.25 - 8.0 - 3.5);
        assert_eq!(Vector::new(1.0, 0.0, 0.0).cross(Vector::new(0.0, 1.0, 0.0)), Vector::new(0.0, 0.0, 1.0));
        assert_eq!((a.lerp(b, 0.0), a.lerp(b, 1.0)), (a, b));
    }
}
