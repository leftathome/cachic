//! The differential tester.
//!
//! The correctness argument for a caching proxy is simple to state and expensive to get wrong:
//! for any object and any range, the bytes a client receives through the cache must equal the
//! bytes the origin would have sent. Everything else - hit ratios, throughput, coalescing - is
//! worthless if this does not hold.
//!
//! Two properties make it usable rather than merely correct:
//!
//! - **A failure prints a seed that reproduces it exactly.** A flaky test with no reproducer is
//!   worse than no test, because it trains people to re-run until green.
//! - **A failure is shrunk to a minimal case.** "Range 4,127,891-4,392,104 of obj-3 differs" is a
//!   bug report nobody can act on; "byte 0 of range 1048576-1048577 differs" is.

use std::fmt;

/// Deterministic RNG. Small and explicit so a seed means the same thing across runs and machines.
#[derive(Debug, Clone)]
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

    /// Uniform in `[0, n)`. Returns 0 for `n == 0` rather than dividing by zero.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

/// One generated case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub object: String,
    pub size: u64,
    pub start: u64,
    /// Inclusive.
    pub end: u64,
}

impl Case {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Always false: a case covers at least one byte by construction, since `end` is inclusive
    /// and never less than `start`. Present because `len` without `is_empty` is a lint.
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn range_header(&self) -> String {
        format!("bytes={}-{}", self.start, self.end)
    }
}

impl fmt::Display for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({} bytes), range {}-{}",
            self.object, self.size, self.start, self.end
        )
    }
}

/// Generates cases, biased towards the boundaries that break slice arithmetic.
#[derive(Debug, Clone)]
pub struct Generator {
    rng: Rng,
    objects: usize,
    size: u64,
    slice_size: u64,
}

impl Generator {
    pub fn new(seed: u64, objects: usize, size: u64, slice_size: u64) -> Self {
        Self {
            rng: Rng::new(seed),
            objects: objects.max(1),
            size,
            slice_size,
        }
    }

    /// Next case.
    ///
    /// One case in three is a deliberate boundary: the first or last byte, a slice edge, or a
    /// range spanning exactly one slice. Uniform random ranges almost never hit those, and they
    /// are where off-by-one errors live.
    pub fn next_case(&mut self) -> Case {
        let object = format!("obj-{}", self.rng.below(self.objects as u64));
        let size = self.size;
        let (start, end) = match self.rng.below(3) {
            0 => self.boundary_range(size),
            _ => {
                let start = self.rng.below(size);
                let len = 1 + self.rng.below(size - start);
                (start, start + len - 1)
            }
        };
        Case {
            object,
            size,
            start,
            end: end.min(size - 1),
        }
    }

    fn boundary_range(&mut self, size: u64) -> (u64, u64) {
        let s = self.slice_size;
        let last = size - 1;
        match self.rng.below(6) {
            0 => (0, 0),
            1 => (last, last),
            2 => (0, last),
            // Exactly one slice.
            3 => {
                let index = self.rng.below(size.div_ceil(s));
                let start = index * s;
                (start.min(last), (start + s - 1).min(last))
            }
            // Straddling a slice boundary by one byte either side.
            4 => {
                let index = 1 + self.rng.below(size.div_ceil(s).max(2) - 1);
                let edge = (index * s).min(last);
                (edge.saturating_sub(1), edge.min(last))
            }
            // The short final slice.
            _ => ((last / s) * s, last),
        }
    }
}

/// A mismatch, already shrunk.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub seed: u64,
    pub case: Case,
    /// Offset of the first differing byte, relative to the start of the range.
    pub offset: u64,
    pub expected: u8,
    pub actual: u8,
    pub actual_len: usize,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "differential mismatch (seed {:#x}): {}\n  \
             first differing byte at offset {} of the range (absolute {}): \
             expected {:#04x}, got {:#04x}\n  \
             response was {} bytes, expected {}",
            self.seed,
            self.case,
            self.offset,
            self.case.start + self.offset,
            self.expected,
            self.actual,
            self.actual_len,
            self.case.len()
        )
    }
}

/// Compare a response against what the origin would have produced.
///
/// Shrinks to the first differing byte rather than reporting that two multi-megabyte buffers are
/// unequal.
pub fn compare(seed: u64, case: &Case, actual: &[u8]) -> Result<(), Mismatch> {
    let expected = crate::content::range(
        crate::content::seed_for(&case.object),
        case.start,
        case.len() as usize,
    );

    if let Some(offset) = expected
        .iter()
        .zip(actual.iter())
        .position(|(a, b)| a != b)
        .map(|p| p as u64)
    {
        return Err(Mismatch {
            seed,
            case: case.clone(),
            offset,
            expected: expected[offset as usize],
            actual: actual[offset as usize],
            actual_len: actual.len(),
        });
    }

    if actual.len() != expected.len() {
        // A short or long body with no differing byte in the overlap: report the boundary, since
        // that is where the defect is.
        let offset = actual.len().min(expected.len()) as u64;
        return Err(Mismatch {
            seed,
            case: case.clone(),
            offset,
            expected: expected.get(offset as usize).copied().unwrap_or(0),
            actual: actual.get(offset as usize).copied().unwrap_or(0),
            actual_len: actual.len(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content;

    #[test]
    fn the_same_seed_produces_the_same_cases() {
        // Without this a printed seed is not a reproducer.
        let a: Vec<_> = (0..20)
            .map(|_| Generator::new(42, 4, 100_000, 4096).next_case())
            .collect();
        let mut gen_b = Generator::new(42, 4, 100_000, 4096);
        let b: Vec<_> = (0..20).map(|_| gen_b.next_case()).collect();
        let mut gen_a = Generator::new(42, 4, 100_000, 4096);
        let a2: Vec<_> = (0..20).map(|_| gen_a.next_case()).collect();
        assert_eq!(a2, b);
        assert_eq!(a.len(), 20);
    }

    #[test]
    fn different_seeds_produce_different_cases() {
        let mut a = Generator::new(1, 4, 100_000, 4096);
        let mut b = Generator::new(2, 4, 100_000, 4096);
        let ca: Vec<_> = (0..10).map(|_| a.next_case()).collect();
        let cb: Vec<_> = (0..10).map(|_| b.next_case()).collect();
        assert_ne!(ca, cb);
    }

    #[test]
    fn every_case_is_within_the_object() {
        let mut g = Generator::new(7, 3, 10_000, 1024);
        for _ in 0..500 {
            let c = g.next_case();
            assert!(c.start <= c.end, "{c}");
            assert!(c.end < c.size, "{c} runs past the end");
            assert!(!c.is_empty());
        }
    }

    #[test]
    fn boundary_cases_are_actually_generated() {
        // Uniform random ranges essentially never hit a slice edge, which is where the
        // off-by-one errors live.
        let mut g = Generator::new(11, 2, 10_000, 1024);
        let cases: Vec<_> = (0..600).map(|_| g.next_case()).collect();
        assert!(
            cases.iter().any(|c| c.start == 0 && c.end == 0),
            "no single-first-byte case"
        );
        assert!(
            cases.iter().any(|c| c.end == c.size - 1),
            "no case touching the final byte"
        );
        assert!(
            cases.iter().any(|c| c.start % 1024 == 0),
            "no slice-aligned case"
        );
    }

    #[test]
    fn a_matching_response_passes() {
        let case = Case {
            object: "obj-0".into(),
            size: 10_000,
            start: 100,
            end: 199,
        };
        let bytes = content::range(content::seed_for("obj-0"), 100, 100);
        compare(1, &case, &bytes).unwrap();
    }

    #[test]
    fn a_corrupt_byte_is_shrunk_to_its_offset() {
        let case = Case {
            object: "obj-0".into(),
            size: 10_000,
            start: 100,
            end: 199,
        };
        let mut bytes = content::range(content::seed_for("obj-0"), 100, 100);
        bytes[37] ^= 0xff;
        let m = compare(0xabc, &case, &bytes).unwrap_err();
        assert_eq!(m.offset, 37);
        let text = m.to_string();
        assert!(text.contains("0xabc"), "seed missing from report: {text}");
        assert!(text.contains("offset 37"), "{text}");
        assert!(text.contains("absolute 137"), "{text}");
    }

    #[test]
    fn a_short_response_is_reported_at_the_boundary() {
        let case = Case {
            object: "obj-0".into(),
            size: 10_000,
            start: 0,
            end: 99,
        };
        let bytes = content::range(content::seed_for("obj-0"), 0, 50);
        let m = compare(1, &case, &bytes).unwrap_err();
        assert_eq!(m.offset, 50);
        assert_eq!(m.actual_len, 50);
        assert!(m.to_string().contains("50 bytes, expected 100"));
    }

    #[test]
    fn content_from_the_wrong_object_is_caught() {
        // The failure a cache-key bug produces: right length, wrong bytes.
        let case = Case {
            object: "obj-0".into(),
            size: 10_000,
            start: 0,
            end: 99,
        };
        let wrong = content::range(content::seed_for("obj-1"), 0, 100);
        assert!(compare(1, &case, &wrong).is_err());
    }
}
