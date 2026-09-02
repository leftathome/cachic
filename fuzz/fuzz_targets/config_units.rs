//! Fuzz the size and duration parsers.
//!
//! These read operator input rather than network input, so the risk is different: a parser that
//! silently returns the wrong number configures a cache the operator did not ask for. A
//! `CACHE_DISK_SIZE` that wraps to a small value would quietly cap the cache at nothing.

#![no_main]

use cachic::config::units;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    if let Ok(bytes) = units::parse_size(input) {
        // Any size we accept must survive being formatted and read back.
        let formatted = units::format_size(bytes);
        let reparsed = units::parse_size(&formatted)
            .unwrap_or_else(|e| panic!("format_size({bytes}) -> {formatted:?} is unparsable: {e}"));
        assert_eq!(reparsed, bytes, "round trip changed {bytes} via {formatted:?}");
    }

    if let Ok(duration) = units::parse_duration(input) {
        // Overflow must be an error, never a wrap. If a unit suffix was applied, the result has
        // to be an exact multiple of that unit - a wrapped multiplication almost never is.
        //
        // An earlier version of this target asserted `as_secs() < u64::MAX`, which fuzzing
        // rejected within a minute. It was not a property, it was a lazy stand-in for "is
        // representable", and a bare `18446744073709551615` is a perfectly valid number of
        // seconds. The parser was right; the assertion was not.
        let trimmed = input.trim().to_ascii_lowercase();
        let multiplier = if trimmed.ends_with('w') {
            604_800
        } else if trimmed.ends_with('d') {
            86_400
        } else if trimmed.ends_with('h') {
            3_600
        } else if trimmed.ends_with('m') {
            60
        } else {
            1
        };
        assert_eq!(
            duration.as_secs() % multiplier,
            0,
            "{input:?} parsed to {} seconds, which is not a multiple of its unit ({multiplier}); \
             the multiplication wrapped",
            duration.as_secs()
        );
    }
});
