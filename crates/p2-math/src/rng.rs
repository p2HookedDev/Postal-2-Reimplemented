//! The engine's random numbers: C runtime `rand()` / 32767, so a value can be 1.0.

/// A seedable copy of the MSVC C runtime `rand` generator (one global state in the engine; one per emitter and
/// one per VM here, so the exact sequence of the game is not reproduced).
#[derive(Debug, Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed)
    }

    pub fn rand(&mut self) -> i32 {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        ((self.0 >> 16) & 0x7fff) as i32
    }

    pub fn frand(&mut self) -> f32 {
        self.rand() as f32 / 32767.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msvc_sequence() {
        let mut rng = Rng::new(1);
        assert_eq!([rng.rand(), rng.rand(), rng.rand()], [41, 18467, 6334]);
    }
}
