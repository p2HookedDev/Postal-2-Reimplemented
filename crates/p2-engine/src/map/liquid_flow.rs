/// Seeds stay small: the shader reads one as a float.
const SEEDS: u32 = 1023;

/// The seed of a modern puddle's edge, never 0 (0 marks a sprite that is no puddle).
pub(super) fn puddle_seed(emitter: u32) -> u32 {
    emitter % SEEDS + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_puddle_seed_is_never_zero() {
        assert_eq!(puddle_seed(7), 8);
        assert_eq!(puddle_seed(1023), 1);
    }
}
