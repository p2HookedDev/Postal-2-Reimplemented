//! C runtime `rand()` / 32767, so a value can be 1.0.

/// One generator per emitter and per VM, not retail's one global state, so retail's exact sequence is not reproduced.
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
