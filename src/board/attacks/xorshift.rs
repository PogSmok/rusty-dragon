//! Fixed-seed PRNG for tests

#[derive(Debug)]
pub(super) struct Xorshift64(u64);

impl Xorshift64 {
    /// Standard Fibonacci hashing constant.
    pub(super) const DEFAULT_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

    /// `seed` must be non-zero, or every output is zero.
    pub(super) const fn new(seed: u64) -> Self {
        debug_assert!(seed != 0, "xorshift seed must be non-zero");
        Self(seed)
    }

    pub(super) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Returns a sparse random number (i.e. one with few bits set)
    pub(super) fn sparse(&mut self) -> u64 {
        self.next() & self.next() & self.next()
    }
}
