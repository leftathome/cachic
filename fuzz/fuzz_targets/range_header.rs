//! Fuzz the `Range` header parser.
//!
//! This reads a header supplied by whatever client happens to connect, so it is the most directly
//! attacker-controlled parser in the codebase. It must never panic, and a parsed range must never
//! escape the object it was resolved against - a range that ends past the end would index outside
//! a slice.

#![no_main]

use cachic::proxy::range::{self, RangeSpec};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let Ok(spec) = range::parse_range(input) else {
        return;
    };

    // Resolving against several object lengths, including the awkward ones.
    for total in [0u64, 1, 2, 1023, 1024, 1025, u64::MAX] {
        if let Ok(resolved) = range::resolve(spec, total) {
            assert!(resolved.start <= resolved.end, "inverted range from {input:?}");
            assert!(
                resolved.end < total,
                "range from {input:?} ends at {} past an object of {total}",
                resolved.end
            );

            // And the slice plan for it must stay within the object.
            for slice_size in [1u32, 1024, 1 << 20] {
                let plan = range::plan(resolved, slice_size);
                assert!(plan.first <= plan.last);
                let last_possible = (total - 1) / slice_size as u64;
                assert!(
                    plan.last as u64 <= last_possible,
                    "plan for {input:?} names slice {} beyond {last_possible}",
                    plan.last
                );
            }
        }
    }

    // A suffix range of zero is unsatisfiable rather than a whole-object request.
    if matches!(spec, RangeSpec::Suffix(0)) {
        assert!(range::resolve(spec, 100).is_err());
    }
});
