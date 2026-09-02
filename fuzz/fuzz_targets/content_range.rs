//! Fuzz `Content-Range` handling.
//!
//! The total object length is taken from this header, and everything downstream - the slice plan,
//! the response's `Content-Length` - is derived from it. A wrong length here is not a crash, it
//! is a silently corrupt cached object, so the parser must be conservative: anything it cannot
//! read confidently must yield no length rather than a guess.

#![no_main]

use libfuzzer_sys::fuzz_target;

// Re-exported through cachic so the fuzz crate does not need to pin matching versions of hyper
// and bytes independently.
use cachic::upstream::client::UpstreamResponse;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let Some(response) = UpstreamResponse::for_test_with_content_range(input) else {
        return;
    };

    if let Some(total) = response.content_range_total() {
        // A length we accept must be one we could actually have parsed from the text.
        assert!(
            input.contains(&total.to_string()),
            "parsed {total} from {input:?}, which does not contain it"
        );
    }
});
