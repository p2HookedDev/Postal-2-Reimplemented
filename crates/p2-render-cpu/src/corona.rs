use p2_math::Vector;

pub const MAX_CORONAS: usize = 64;

pub fn fade_delta(seconds: f32) -> f32 {
    3.0 * seconds
}

#[derive(Debug, Clone)]
pub struct Coronas<K> {
    slots: Vec<Option<(K, f32)>>,
}

impl<K> Default for Coronas<K> {
    fn default() -> Self {
        Self { slots: (0..MAX_CORONAS).map(|_| None).collect() }
    }
}

impl<K: Copy + PartialEq> Coronas<K> {
    /// Every corona dims by `delta` and is dropped below 0; a candidate `clear` says the eye sees
    /// brightens by 2 · `delta` up to 1, or takes the first free slot. Seen it lights over 1/3 s.
    pub fn step(&mut self, delta: f32, candidates: impl IntoIterator<Item = K>, mut clear: impl FnMut(K) -> bool) {
        for slot in &mut self.slots {
            if let Some((_, bright)) = slot {
                *bright -= delta;
                if *bright < 0.0 {
                    *slot = None;
                }
            }
        }
        let mut free = 0;
        for key in candidates {
            if !clear(key) {
                continue;
            }
            if let Some((_, bright)) = self.slots.iter_mut().flatten().find(|(k, _)| *k == key) {
                *bright = (*bright + 2.0 * delta).min(1.0);
                continue;
            }
            while free < MAX_CORONAS && self.slots[free].is_some() {
                free += 1;
            }
            if free < MAX_CORONAS {
                self.slots[free] = Some((key, (2.0 * delta).min(1.0)));
            }
        }
    }

    pub fn lit(&self) -> impl Iterator<Item = (K, f32)> + '_ {
        self.slots.iter().flatten().copied()
    }

    pub fn forget(&mut self, key: K) {
        for slot in &mut self.slots {
            if slot.is_some_and(|(k, _)| k == key) {
                *slot = None;
            }
        }
    }
}

/// `LightHue` as hue, blended towards white by `LightSaturation` / 255, times the brightness.
/// `LightBrightness`, `LightType` and fog play no part in retail.
pub fn corona_color(hue: u8, saturation: u8, bright: f32) -> [f32; 3] {
    crate::lighting::hue_color(hue, saturation).map(|c| c * bright)
}

pub fn size_factor(distance: f32, light_sizes: Option<(f32, f32)>) -> f32 {
    let (min, max) = match light_sizes {
        Some((min, max)) => {
            let min = (min / 10.0).max(0.01);
            (min, (max / 10.0).max(min))
        }
        None => (0.01, 1000.0),
    };
    let factor = distance / 1000.0;
    if min <= factor { factor.min(max) } else { min }
}

pub fn non_square(draw_scale3d: [f32; 3]) -> bool {
    draw_scale3d[0] != draw_scale3d[1] && draw_scale3d[2] > 0.0
}

pub fn non_square_scale(front: Vector, x: Vector, y: Vector) -> f32 {
    ((1.0 - front.dot(x).abs()) * (1.0 - front.dot(y).abs())).sqrt()
}

pub fn corona_quad(location: [f32; 3], right: [f32; 3], up: [f32; 3], draw_scale: f32, draw_scale3d: [f32; 3], factor: f32) -> [[f32; 3]; 4] {
    let r = right.map(|c| c * draw_scale3d[0] * draw_scale * factor * 512.0);
    let u = up.map(|c| c * draw_scale3d[1] * draw_scale * factor * 512.0);
    let corner = |sr: f32, su: f32| std::array::from_fn(|i| location[i] + sr * r[i] + su * u[i]);
    [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seen_corona_lights_in_a_third_of_a_second_and_fades_as_fast() {
        let mut coronas = Coronas::default();
        let delta = fade_delta(1.0 / 30.0);
        for _ in 0..8 {
            coronas.step(delta, [7], |_| true);
        }
        let bright = coronas.lit().next().map_or(0.0, |(_, b)| b);
        assert!((bright - 0.9).abs() < 1e-4, "{bright}");
        coronas.step(delta, [7], |_| true);
        coronas.step(delta, [7], |_| true);
        assert_eq!(coronas.lit().next(), Some((7, 1.0)));
        for _ in 0..10 {
            coronas.step(delta, [7], |_| false);
        }
        assert!(coronas.lit().next().is_none_or(|(_, b)| b < 1e-4));
        coronas.step(delta, [7], |_| false);
        assert_eq!(coronas.lit().next(), None);
    }

    #[test]
    fn slots_are_capped_and_reused() {
        let mut coronas = Coronas::default();
        coronas.step(0.1, 0..100, |_| true);
        assert_eq!(coronas.lit().count(), MAX_CORONAS);
        coronas.forget(3);
        coronas.step(0.1, [500], |_| true);
        assert_eq!(coronas.lit().nth(3), Some((500, 0.2)));
    }

    #[test]
    fn colour_is_hue_blended_to_white_by_saturation() {
        assert_eq!(corona_color(0, 0, 1.0), [1.0, 0.0, 0.0]);
        assert_eq!(corona_color(0, 255, 0.5), [0.5, 0.5, 0.5]);
        assert_eq!(corona_color(255, 0, 1.0), [1.0, 0.0, 0.0]);
        assert_eq!(corona_color(170, 0, 1.0), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn size_grows_with_distance_between_the_limits() {
        assert_eq!(size_factor(500.0, None), 0.5);
        assert_eq!(size_factor(1.0, None), 0.01);
        assert_eq!(size_factor(5000.0, Some((0.0, 20.0))), 2.0);
        assert_eq!(size_factor(100.0, Some((5.0, 20.0))), 0.5);
        assert_eq!(size_factor(5000.0, Some((5.0, 1.0))), 0.5);
    }

    #[test]
    fn quad_spans_draw_scale_times_512_times_the_factor() {
        let quad = corona_quad([0.0, 0.0, 100.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], 0.5, [1.0, 2.0, 1.0], 0.125);
        assert_eq!(quad[0], [0.0, -32.0, 36.0]);
        assert_eq!(quad[1], [0.0, 32.0, 36.0]);
        assert_eq!(quad[2], [0.0, 32.0, 164.0]);
        assert!(non_square([1.0, 2.0, 1.0]) && !non_square([1.0, 1.0, 1.0]) && !non_square([1.0, 2.0, 0.0]));
        assert_eq!(non_square_scale([1.0, 0.0, 0.0].into(), [0.0, 1.0, 0.0].into(), [0.0, 0.0, 1.0].into()), 1.0);
        assert_eq!(non_square_scale([1.0, 0.0, 0.0].into(), [1.0, 0.0, 0.0].into(), [0.0, 0.0, 1.0].into()), 0.0);
    }
}
