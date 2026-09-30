//! Deterministic sampling — the only randomness in the harness.
//!
//! #940's rule: no clock, no `rand`, no thread-count-dependent ordering.
//! splitmix64 is a 2-instruction-state generator with fixed arithmetic, so
//! the same seed yields the same stream on every machine and every run.

/// splitmix64. Not cryptographic; that is not the job — the job is that the
/// stream never changes and is cheap to re-derive mid-epoch on resume.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9e37_79b9_7f4a_7c15))
    }

    /// Next raw u64.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in [0, n) — Java's `nextInt` rejection method, so the bound
    /// is unbiased (a modulo would favour low buckets; with 77 labels and
    /// 10k draws per epoch that bias is visible, and it is avoidable).
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "below(0)");
        if n == 1 {
            return 0;
        }
        let n = n as u64;
        let limit = u64::MAX - (u64::MAX % n) - 1;
        loop {
            let x = self.next_u64();
            if x <= limit {
                return (x % n) as usize;
            }
        }
    }

    /// Uniform f32 in [0, 1) — 24 random mantissa bits, the same precision
    /// a f32 can hold without rounding bias.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Normal sample, mean 0, standard deviation `sd` — Box–Muller on two
    /// uniforms; deterministic because both uniforms come from this stream.
    pub fn normal(&mut self, mean: f32, sd: f32) -> f32 {
        let u1 = self.unit().max(1e-7);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos() * sd + mean
    }

    /// Fisher–Yates over a slice, in place, using [`Rng::below`].
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

/// A stable per-epoch stream seed: two independent inputs folded through
/// splitmix64 so epoch N's data order depends only on (base, N).
pub fn stream_seed(base: u64, epoch: usize) -> u64 {
    let mut rng = Rng::new(base ^ (epoch as u64).wrapping_mul(0x517c_c1b7_2722_0a95));
    rng.next_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_is_fixed() {
        let mut rng = Rng::new(1064);
        let drawn: Vec<u64> = (0..4).map(|_| rng.next_u64()).collect();
        assert_eq!(
            drawn,
            vec![
                0x22e6_a127_c5c9_6dc9,
                0xf209_501a_efa8_d2c5,
                0x4566_4114_e006_c9c6,
                0x4934_f48e_9190_3000,
            ],
            "if this moves, every trained checkpoint just became irreproducible"
        );
    }

    #[test]
    fn below_covers_and_stays_in_range() {
        let mut rng = Rng::new(7);
        let mut seen = [false; 77];
        for _ in 0..7700 {
            seen[rng.below(77)] = true;
        }
        assert!(seen.iter().all(|&s| s), "77 labels must all be reachable");
    }
}
