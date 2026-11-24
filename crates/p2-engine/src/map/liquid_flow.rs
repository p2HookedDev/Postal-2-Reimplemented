const SEEDS: u32 = 1023;

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
