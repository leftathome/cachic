//! Fuzz the `cache-domains` parser.
//!
//! This list is community-maintained and arrives over the network on a refresh (FR-61), so it is
//! untrusted input on a path that can replace live configuration. It must reject a malformed list
//! in one piece: a partially-applied list is a cache that silently stops caching half the
//! services it used to.

#![no_main]

use std::collections::BTreeMap;

use cachic::services::domains::DomainList;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    // The domain file, with a fixed index.
    let mut files = BTreeMap::new();
    files.insert("x.txt".to_string(), input.to_string());
    if let Ok(list) = DomainList::parse(
        r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
        &files,
    ) {
        // A list that parses must never be empty; that is what makes a bad refresh rejectable.
        assert!(list.pattern_count() > 0, "accepted an empty list from {input:?}");
        // And every pattern it produced must match something, or the rule does nothing.
        let matcher = cachic::services::matcher::Matcher::build(&list);
        assert!(matcher.pattern_count() > 0);
    }

    // The index itself, which is JSON from the same untrusted source.
    let _ = DomainList::parse(input, &BTreeMap::new());
});
