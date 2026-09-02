//! Property tests for the parsers and the slice arithmetic (TASK-21).
//!
//! These run on stable in ordinary CI, unlike the `cargo-fuzz` targets, which need nightly and
//! run on a schedule. The division of labour: fuzzing hunts for inputs that panic, these assert
//! the properties that must hold for *all* inputs, panic or not.
//!
//! The distinction matters. A parser that never panics but silently returns the wrong number is
//! exactly as dangerous as one that crashes, and only the properties below would catch it.

use cachic::{
    config::units,
    proxy::range::{self, ByteRange, RangeSpec},
    services::{domains::DomainList, key, matcher::Matcher},
};
use proptest::prelude::*;

proptest! {
    /// No input, however malformed, may panic a parser that reads network data.
    #[test]
    fn range_parsing_never_panics(input in ".*") {
        let _ = range::parse_range(&input);
    }

    #[test]
    fn size_parsing_never_panics(input in ".*") {
        let _ = units::parse_size(&input);
    }

    #[test]
    fn duration_parsing_never_panics(input in ".*") {
        let _ = units::parse_duration(&input);
    }

    #[test]
    fn host_normalisation_never_panics(input in ".*") {
        let _ = Matcher::normalise_host(&input);
    }

    /// A parsed size must round-trip through its own formatting.
    #[test]
    fn size_formatting_round_trips(bytes in 0u64..(1u64 << 50)) {
        let text = units::format_size(bytes);
        let reparsed = units::parse_size(&text).expect("format_size produced something unparsable");
        prop_assert_eq!(reparsed, bytes);
    }

    /// A resolved range always lies inside the object.
    #[test]
    fn resolved_ranges_stay_inside_the_object(
        total in 1u64..1_000_000,
        start in 0u64..1_000_000,
        len in 1u64..1_000_000,
    ) {
        let spec = RangeSpec::FromTo(start, start.saturating_add(len).saturating_sub(1));
        if let Ok(resolved) = range::resolve(spec, total) {
            prop_assert!(resolved.start <= resolved.end);
            prop_assert!(resolved.end < total, "range ends past the object");
            prop_assert!(!resolved.is_empty());
        }
    }

    /// A suffix range never asks for more than the object holds.
    #[test]
    fn suffix_ranges_are_clamped(total in 1u64..1_000_000, n in 1u64..2_000_000) {
        if let Ok(resolved) = range::resolve(RangeSpec::Suffix(n), total) {
            prop_assert_eq!(resolved.end, total - 1);
            prop_assert!(resolved.len() <= total);
        }
    }

    /// The invariant the whole orchestrator rests on: the slice windows covering a request
    /// concatenate back to exactly the bytes requested, with no gaps and no overlaps.
    #[test]
    fn slice_windows_tile_the_request_exactly(
        slice_size in prop::sample::select(vec![1024u32, 4096, 65536, 1 << 20]),
        total in 1u64..10_000_000,
        start in 0u64..10_000_000,
        len in 1u64..10_000_000,
    ) {
        prop_assume!(start < total);
        let end = start.saturating_add(len - 1).min(total - 1);
        let wanted = ByteRange { start, end };
        let plan = range::plan(wanted, slice_size);

        let mut covered = 0u64;
        let mut previous_end: Option<u64> = None;
        for index in plan.indices() {
            let (from, to) = range::payload_window(index, slice_size, wanted);
            prop_assert!(to > from, "slice {} contributes nothing", index);
            prop_assert!(
                to <= slice_size as usize,
                "window {}..{} exceeds the slice size {}",
                from, to, slice_size
            );
            // Absolute positions must be contiguous across slices.
            let absolute_start = index as u64 * slice_size as u64 + from as u64;
            if let Some(previous) = previous_end {
                prop_assert_eq!(absolute_start, previous, "gap or overlap between slices");
            }
            previous_end = Some(index as u64 * slice_size as u64 + to as u64);
            covered += (to - from) as u64;
        }
        prop_assert_eq!(covered, wanted.len(), "windows do not cover the request");
    }

    /// Every slice a plan names actually exists within the object.
    #[test]
    fn planned_slices_exist_within_the_object(
        slice_size in prop::sample::select(vec![1024u32, 65536]),
        total in 1u64..1_000_000,
        start in 0u64..1_000_000,
    ) {
        prop_assume!(start < total);
        let wanted = ByteRange { start, end: total - 1 };
        let plan = range::plan(wanted, slice_size);
        let last_slice = (total - 1) / slice_size as u64;
        prop_assert!(
            plan.last as u64 <= last_slice,
            "plan names slice {} but the object ends at slice {}",
            plan.last, last_slice
        );
    }

    /// A cache key is a pure function of its inputs, and different paths never collide.
    #[test]
    fn cache_keys_are_deterministic_and_distinct(
        service in "[a-z]{1,10}",
        path_a in "/[a-zA-Z0-9/._-]{0,40}",
        path_b in "/[a-zA-Z0-9/._-]{0,40}",
    ) {
        let rule = key::CompiledRule::default();
        let a1 = key::normalise(&service, "h", &path_a, &rule);
        let a2 = key::normalise(&service, "h", &path_a, &rule);
        prop_assert_eq!(a1.object_id(), a2.object_id(), "hashing is not deterministic");

        if path_a != path_b {
            let b = key::normalise(&service, "h", &path_b, &rule);
            prop_assert_ne!(a1.object_id(), b.object_id(), "distinct paths collided");
        }
    }

    /// The domain-list parser must reject or accept, never panic, on arbitrary bytes.
    #[test]
    fn domain_list_parsing_never_panics(body in ".*") {
        let mut files = std::collections::BTreeMap::new();
        files.insert("x.txt".to_string(), body);
        let _ = DomainList::parse(
            r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
            &files,
        );
    }

    /// And on an arbitrary index too, which arrives over the network on a refresh.
    #[test]
    fn domain_index_parsing_never_panics(index in ".*") {
        let _ = DomainList::parse(&index, &std::collections::BTreeMap::new());
    }
}
