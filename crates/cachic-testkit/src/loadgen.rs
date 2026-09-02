//! Load generator.
//!
//! Drives N concurrent clients at a target request mix and reports throughput and time-to-first-
//! byte percentiles. Used by the performance gate and the benchmark harness.
//!
//! Percentiles come from the full sample set rather than a running estimate: the sample counts
//! here are small enough to sort, and an approximate p99 is exactly the number you cannot trust
//! when chasing a latency regression.

use std::time::Duration;

/// What a run measured.
#[derive(Debug, Clone)]
pub struct Report {
    pub requests: usize,
    pub bytes: u64,
    pub elapsed: Duration,
    /// Sorted, so percentiles are exact.
    pub ttfb: Vec<Duration>,
}

impl Report {
    pub fn gbps(&self) -> f64 {
        if self.elapsed.is_zero() {
            return 0.0;
        }
        (self.bytes as f64 * 8.0) / self.elapsed.as_secs_f64() / 1e9
    }

    pub fn mib_per_second(&self) -> f64 {
        if self.elapsed.is_zero() {
            return 0.0;
        }
        (self.bytes as f64 / (1024.0 * 1024.0)) / self.elapsed.as_secs_f64()
    }

    /// Exact percentile from the sorted samples, by nearest rank. `p` is in `[0, 1]`.
    ///
    /// Nearest rank rather than interpolating between indices: for 100 samples of 1..=100 ms,
    /// p50 is 50 ms and p99 is 99 ms. Interpolation gives 51 and 100, and a "p99" that returns
    /// the maximum is really p100 and hides the tail it exists to expose.
    pub fn ttfb_percentile(&self, p: f64) -> Duration {
        if self.ttfb.is_empty() {
            return Duration::ZERO;
        }
        let n = self.ttfb.len();
        let p = p.clamp(0.0, 1.0);
        let rank = (p * n as f64).ceil() as usize;
        let index = rank.saturating_sub(1).min(n - 1);
        self.ttfb[index]
    }

    pub fn summary(&self) -> String {
        format!(
            "{} requests, {:.2} Gbps ({:.0} MiB/s), TTFB p50 {:.2} ms / p99 {:.2} ms",
            self.requests,
            self.gbps(),
            self.mib_per_second(),
            self.ttfb_percentile(0.50).as_secs_f64() * 1000.0,
            self.ttfb_percentile(0.99).as_secs_f64() * 1000.0,
        )
    }
}

/// Accumulates samples from concurrent workers.
#[derive(Debug, Default)]
pub struct Collector {
    bytes: u64,
    requests: usize,
    ttfb: Vec<Duration>,
}

impl Collector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, bytes: u64, ttfb: Duration) {
        self.bytes += bytes;
        self.requests += 1;
        self.ttfb.push(ttfb);
    }

    pub fn finish(mut self, elapsed: Duration) -> Report {
        self.ttfb.sort();
        Report {
            requests: self.requests,
            bytes: self.bytes,
            elapsed,
            ttfb: self.ttfb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(samples: &[u64], bytes: u64, secs: f64) -> Report {
        let mut c = Collector::new();
        for ms in samples {
            c.record(
                bytes / samples.len().max(1) as u64,
                Duration::from_millis(*ms),
            );
        }
        c.finish(Duration::from_secs_f64(secs))
    }

    #[test]
    fn computes_throughput() {
        let r = report(&[1, 2, 3, 4], 1_000_000_000, 1.0);
        assert!((r.gbps() - 8.0).abs() < 0.01, "{}", r.gbps());
    }

    #[test]
    fn percentiles_are_exact_not_estimated() {
        // 100 samples, 1..=100 ms. An approximate p99 is the number you cannot trust when
        // chasing a latency regression.
        let samples: Vec<u64> = (1..=100).collect();
        let r = report(&samples, 100, 1.0);
        assert_eq!(r.ttfb_percentile(0.50).as_millis(), 50);
        // Nearest-rank: the 99th of 100 sorted samples, so 99 ms rather than the maximum. A p99
        // that returns the maximum is really p100 and hides the tail it exists to expose.
        assert_eq!(r.ttfb_percentile(0.99).as_millis(), 99);
        assert_eq!(
            r.ttfb_percentile(0.0).as_millis(),
            1,
            "p0 is the fastest sample"
        );
        assert_eq!(
            r.ttfb_percentile(1.0).as_millis(),
            100,
            "p100 is the slowest sample"
        );
    }

    #[test]
    fn handles_an_empty_run_without_dividing_by_zero() {
        let r = Collector::new().finish(Duration::ZERO);
        assert_eq!(r.gbps(), 0.0);
        assert_eq!(r.ttfb_percentile(0.99), Duration::ZERO);
        assert_eq!(r.requests, 0);
    }

    #[test]
    fn samples_are_sorted_so_percentiles_do_not_depend_on_arrival_order() {
        let a = report(&[100, 1, 50, 2], 4, 1.0);
        let b = report(&[1, 2, 50, 100], 4, 1.0);
        assert_eq!(a.ttfb_percentile(0.5), b.ttfb_percentile(0.5));
    }

    #[test]
    fn the_summary_names_the_numbers_an_operator_reads() {
        let r = report(&[5, 10], 2_000_000, 1.0);
        let s = r.summary();
        assert!(s.contains("Gbps"), "{s}");
        assert!(s.contains("TTFB p50"), "{s}");
        assert!(s.contains("p99"), "{s}");
    }
}
