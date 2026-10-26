//! A tiny deterministic generator for the robustness tests (no extra dependencies):
//! random bytes and mutations of valid inputs.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.next() as u8).collect()
    }
    /// Flip, insert, delete, duplicate or truncate parts of `seed`.
    pub fn mutate(&mut self, seed: &[u8]) -> Vec<u8> {
        let mut v = seed.to_vec();
        for _ in 0..1 + self.below(8) {
            let at = self.below(v.len() + 1);
            match self.below(7) {
                0 if at < v.len() => v[at] ^= 1 << self.below(8),
                1 => v.insert(at, self.next() as u8),
                2 if at < v.len() => {
                    v.remove(at);
                }
                3 => {
                    let end = (at + self.below(16)).min(v.len());
                    let part = v[at..end].to_vec();
                    v.splice(at..at, part);
                }
                4 => v.truncate(at),
                5 => {
                    const JUNK: [&[u8]; 9] = [b"\"\\", b"\xff", b"{", b"]", b"\n", b"\0", b"1e999", b"-", b"NaN"];
                    let junk = JUNK[self.below(JUNK.len())];
                    v.splice(at..at, junk.iter().copied());
                }
                _ => {
                    let n = self.below(4) + 1;
                    v.splice(at..at, std::iter::repeat_n(b'9', n * 8));
                }
            }
        }
        v
    }
}
