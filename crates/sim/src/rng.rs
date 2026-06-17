//! rng.rs — PCG32: small, fast, statistically solid, fully deterministic.
//! The world owns exactly one of these; all game randomness flows through it.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcg32 {
    pub state: u64,
    pub inc: u64,
}

impl Pcg32 {
    pub fn new(seed: u64) -> Self {
        let mut r = Pcg32 { state: 0, inc: (seed << 1) | 1 };
        r.next_u32();
        r.state = r.state.wrapping_add(seed ^ 0x853c49e6748fea9b);
        r.next_u32();
        r
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6364136223846793005)
            .wrapping_add(self.inc | 1);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in [0, n). n must be > 0.
    pub fn below(&mut self, n: u32) -> u32 {
        // simple modulo; bias is irrelevant for gameplay purposes and keeps it branch-light
        self.next_u32() % n.max(1)
    }

    /// Uniform in [lo, hi] inclusive.
    pub fn range_i32(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        lo + self.below((hi - lo + 1) as u32) as i32
    }

    /// True with probability num/den.
    pub fn chance(&mut self, num: u32, den: u32) -> bool {
        self.below(den.max(1)) < num
    }
}
