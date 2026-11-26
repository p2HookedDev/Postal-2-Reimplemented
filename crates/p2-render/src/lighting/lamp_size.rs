/// Default source size as a share of a lamp's radius. Matches `lamp_shadow.wesl`.
pub const LAMP_SOURCE_SHARE: f32 = 0.08;
/// A corona's quad is a halo around the fixture: the fixture's bright part is this share of its side
/// (unverified).
const CORONA_SHARE: f32 = 0.05;
const LARGEST_SHARE: f32 = 0.25;

pub fn lamp_size(radius: f32, corona: Option<f32>, fixture: Option<f32>) -> f32 {
    let size = match (fixture, corona) {
        (Some(fixture), _) if fixture > 0.0 => fixture,
        (_, Some(corona)) if corona > 0.0 => corona * CORONA_SHARE,
        _ => radius * LAMP_SOURCE_SHARE,
    };
    size.clamp(0.0, radius.max(0.0) * LARGEST_SHARE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_comes_before_the_corona_and_the_radius() {
        assert_eq!(lamp_size(1000.0, Some(640.0), Some(24.0)), 24.0);
        assert_eq!(lamp_size(1000.0, Some(640.0), None), 32.0);
        assert_eq!(lamp_size(1000.0, None, None), 80.0);
        assert_eq!(lamp_size(1000.0, Some(0.0), Some(0.0)), 80.0);
    }

    #[test]
    fn a_size_stays_within_a_share_of_the_radius() {
        assert_eq!(lamp_size(100.0, None, Some(500.0)), 25.0);
        assert_eq!(lamp_size(-5.0, None, None), 0.0);
    }
}
