use crate::math::Vector;
use super::{PropertyValue, Range, RangeVector};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BeamEnd {
    #[default]
    Velocity,
    Distance,
    Offset,
    Actor,
    TraceOffset,
    OffsetAsAbsolute,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamEndPoint {
    pub offset: RangeVector,
    pub weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamScale {
    pub scale: Vector,
    pub relative_length: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BeamProperties {
    pub end_by: BeamEnd,
    pub distance_range: Range,
    pub end_points: Vec<BeamEndPoint>,
    pub u_scale: f32,
    pub v_scale: f32,
    pub rotating_sheets: i32,
    pub low_noise: RangeVector,
    pub low_points: i32,
    pub high_noise: RangeVector,
    pub high_points: i32,
    pub low_scales: Vec<BeamScale>,
    pub low_scale_repeats: f32,
    pub use_low_scale: bool,
    pub high_scales: Vec<BeamScale>,
    pub high_scale_repeats: f32,
    pub use_high_scale: bool,
    pub noise_determines_end_point: bool,
}

impl Default for BeamProperties {
    fn default() -> Self {
        let zero = RangeVector::splat(0.0, 0.0);
        Self {
            end_by: BeamEnd::Velocity,
            distance_range: Range::new(0.0, 0.0),
            end_points: Vec::new(),
            u_scale: 1.0,
            v_scale: 1.0,
            rotating_sheets: 0,
            low_noise: zero,
            low_points: 3,
            high_noise: zero,
            high_points: 10,
            low_scales: Vec::new(),
            low_scale_repeats: 0.0,
            use_low_scale: false,
            high_scales: Vec::new(),
            high_scale_repeats: 0.0,
            use_high_scale: false,
            noise_determines_end_point: false,
        }
    }
}

impl BeamProperties {
    pub fn sheets(&self) -> usize {
        usize::try_from(self.rotating_sheets).unwrap_or(0).max(1)
    }
}

pub(super) fn beam_end_point(point: &PropertyValue) -> Option<BeamEndPoint> {
    let members = point.members();
    let offset = members.get(1).and_then(PropertyValue::range_vector)?;
    Some(BeamEndPoint { offset, weight: members.get(2).and_then(PropertyValue::float).unwrap_or(0.0) })
}

#[expect(clippy::unnecessary_wraps, reason = "the `read!` macro takes an `Option` from every reader")]
pub(super) fn beam_scales(keys: &PropertyValue) -> Option<Vec<BeamScale>> {
    let key = |key: &PropertyValue| {
        let members = key.members();
        let scale = members.first().and_then(PropertyValue::floats::<3>).map_or(Vector::ZERO, Vector::from);
        BeamScale { scale, relative_length: members.get(1).and_then(PropertyValue::float).unwrap_or(0.0) }
    };
    Some(keys.members().iter().map(key).collect())
}
