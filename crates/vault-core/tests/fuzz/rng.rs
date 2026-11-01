//! Deterministic RNG (SplitMix64): tiny, seedable, and identical across
//! platforms, so every fuzz case is reproducible from its printed seed.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`. Panics on `n == 0` — callers pick from non-empty
    /// pools, and a violation is a generator bug worth failing loudly.
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "rng.below(0)");
        (self.next_u64() % n as u64) as usize
    }

    pub fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 0
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    /// A random `usize` in `lo..=hi` (inclusive), for small sizes.
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_covering() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut r = Rng::new(7);
        assert_eq!(r.range(3, 3), 3);
        assert!((3..=5).contains(&r.range(3, 5)));
        assert_eq!(r.below(1), 0);
        assert!(r.pick(&[9]).eq(&9));
    }
}
