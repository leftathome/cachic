//! Host to service matching.
//!
//! On every request, so it has to be cheap. Exact hosts go in a hash map; wildcards are matched by
//! walking the host's parent domains, which is at most a handful of lookups because hostnames are
//! short. That beats scanning a pattern list, and it beats a regex set, without either's
//! complexity.
//!
//! Longest match wins, so `*.cdn.example.com` takes precedence over `*.example.com`. `cache-domains`
//! does contain overlapping suffixes across services, and picking the shorter one would attribute
//! traffic to the wrong service and normalise its key with the wrong rules.

use std::collections::HashMap;

use super::domains::{DomainList, Pattern};

/// Compiled host matcher.
#[derive(Debug, Clone, Default)]
pub struct Matcher {
    /// host -> service index
    exact: HashMap<String, usize>,
    /// parent domain -> service index, for `*.parent` patterns
    suffix: HashMap<String, usize>,
    names: Vec<String>,
}

impl Matcher {
    pub fn build(list: &DomainList) -> Self {
        let mut exact = HashMap::new();
        let mut suffix = HashMap::new();
        let mut names = Vec::with_capacity(list.services.len());

        for (index, service) in list.services.iter().enumerate() {
            names.push(service.name.clone());
            for pattern in &service.patterns {
                match pattern {
                    Pattern::Exact(host) => {
                        exact.insert(host.clone(), index);
                    }
                    Pattern::Suffix(parent) => {
                        suffix.insert(parent.clone(), index);
                    }
                }
            }
        }

        Self {
            exact,
            suffix,
            names,
        }
    }

    /// Normalise a `Host` header for matching: strip the port, lowercase, drop a trailing dot.
    ///
    /// IPv6 literals arrive bracketed (`[::1]:80`), so the port is found from the last colon only
    /// when it is outside the brackets.
    pub fn normalise_host(host: &str) -> String {
        let host = host.trim();
        let without_port = if let Some(rest) = host.strip_prefix('[') {
            // [::1]:80 -> ::1
            match rest.split_once(']') {
                Some((inner, _)) => inner,
                None => rest,
            }
        } else {
            match host.rsplit_once(':') {
                Some((before, after)) if after.chars().all(|c| c.is_ascii_digit()) => before,
                _ => host,
            }
        };
        without_port.trim_end_matches('.').to_ascii_lowercase()
    }

    /// The service a host belongs to, if any.
    pub fn service_for(&self, host: &str) -> Option<&str> {
        let host = Self::normalise_host(host);
        if host.is_empty() {
            return None;
        }
        if let Some(&index) = self.exact.get(&host) {
            return self.names.get(index).map(String::as_str);
        }
        // Walk parents, longest first: a.b.c.example.com tries b.c.example.com, then c.example.com,
        // and so on. A wildcard does not match its own parent, matching cache-domains' convention
        // of listing both when both are wanted.
        let mut rest = host.as_str();
        while let Some((_, parent)) = rest.split_once('.') {
            if parent.is_empty() {
                break;
            }
            if let Some(&index) = self.suffix.get(parent) {
                return self.names.get(index).map(String::as_str);
            }
            rest = parent;
        }
        None
    }

    pub fn service_count(&self) -> usize {
        self.names.len()
    }

    pub fn pattern_count(&self) -> usize {
        self.exact.len() + self.suffix.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::domains;
    use std::collections::BTreeMap;

    fn matcher_from(files: &[(&str, &str)], services: &[&str]) -> Matcher {
        let index = format!(
            r#"{{"cache_domains":[{}]}}"#,
            services
                .iter()
                .zip(files)
                .map(|(name, (file, _))| format!(
                    r#"{{"name":"{name}","domain_files":["{file}"]}}"#
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let map: BTreeMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Matcher::build(&domains::DomainList::parse(&index, &map).unwrap())
    }

    #[test]
    fn matches_exact_hosts() {
        let m = matcher_from(&[("a.txt", "cdn.example.com\n")], &["svc"]);
        assert_eq!(m.service_for("cdn.example.com"), Some("svc"));
        assert_eq!(m.service_for("other.example.com"), None);
    }

    #[test]
    fn matches_wildcards_at_any_depth() {
        let m = matcher_from(&[("a.txt", "*.cdn.example.com\n")], &["svc"]);
        assert_eq!(m.service_for("a.cdn.example.com"), Some("svc"));
        assert_eq!(m.service_for("a.b.c.cdn.example.com"), Some("svc"));
    }

    #[test]
    fn a_wildcard_does_not_match_its_own_parent() {
        // cache-domains lists *.cdn.blizzard.com and cdn.blizzard.com separately. Treating the
        // wildcard as covering the parent would silently diverge from monolithic.
        let m = matcher_from(&[("a.txt", "*.cdn.example.com\n")], &["svc"]);
        assert_eq!(m.service_for("cdn.example.com"), None);
    }

    #[test]
    fn longest_suffix_wins() {
        // Attributing traffic to the shorter match would normalise the key with another
        // service's rules.
        let m = matcher_from(
            &[
                ("a.txt", "*.example.com\n"),
                ("b.txt", "*.cdn.example.com\n"),
            ],
            &["broad", "specific"],
        );
        assert_eq!(m.service_for("x.cdn.example.com"), Some("specific"));
        assert_eq!(m.service_for("x.other.example.com"), Some("broad"));
    }

    #[test]
    fn exact_beats_wildcard() {
        let m = matcher_from(
            &[
                ("a.txt", "*.example.com\n"),
                ("b.txt", "special.example.com\n"),
            ],
            &["wild", "exact"],
        );
        assert_eq!(m.service_for("special.example.com"), Some("exact"));
    }

    #[test]
    fn strips_ports_and_normalises_case_and_trailing_dots() {
        let m = matcher_from(&[("a.txt", "cdn.example.com\n")], &["svc"]);
        for host in [
            "cdn.example.com:80",
            "CDN.Example.com",
            "cdn.example.com.",
            "  cdn.example.com:8080  ",
        ] {
            assert_eq!(m.service_for(host), Some("svc"), "host {host:?}");
        }
    }

    #[test]
    fn handles_ipv6_literals_without_mistaking_colons_for_ports() {
        let m = matcher_from(&[("a.txt", "cdn.example.com\n")], &["svc"]);
        assert_eq!(m.service_for("[::1]:80"), None);
        assert_eq!(m.service_for("[2001:db8::1]"), None);
    }

    #[test]
    fn rejects_empty_and_degenerate_hosts() {
        let m = matcher_from(&[("a.txt", "cdn.example.com\n")], &["svc"]);
        for host in ["", "   ", ".", ":80"] {
            assert_eq!(m.service_for(host), None, "host {host:?}");
        }
    }

    #[test]
    fn matches_real_cache_domains_hosts() {
        // Against the bundled snapshot rather than invented data: these are hostnames real
        // clients actually request.
        let m = Matcher::build(&domains::bundled().unwrap());
        assert_eq!(m.service_for("lancache.steamcontent.com"), Some("steam"));
        assert_eq!(m.service_for("cdn.blizzard.com"), Some("blizzard"));
        assert_eq!(
            m.service_for("level3.blizzard.com.cdn.blizzard.com"),
            Some("blizzard")
        );
        assert_eq!(m.service_for("cdn1.epicgames.com"), Some("epicgames"));
        assert_eq!(m.service_for("assets1.xboxlive.com"), Some("xboxlive"));
        assert_eq!(m.service_for("l3cdn.riotgames.com"), Some("riot"));
        // Not a CDN we cache.
        assert_eq!(m.service_for("www.google.com"), None);
        assert_eq!(m.service_for("example.org"), None);
    }

    #[test]
    fn lookup_is_fast_enough_to_run_on_every_request() {
        // The matcher is on the hot path, so a regression here costs throughput on every
        // request. Enforced only in release, for the same reason as the perf gate: a debug
        // build measures the wrong binary.
        let m = Matcher::build(&domains::bundled().unwrap());
        let hosts = [
            "lancache.steamcontent.com",
            "level3.blizzard.com.cdn.blizzard.com",
            "cdn1.epicgames.com",
            "www.google.com",
            "a.b.c.d.e.f.example.com",
        ];
        let iterations = 200_000;
        let start = std::time::Instant::now();
        let mut hits = 0usize;
        for i in 0..iterations {
            if m.service_for(hosts[i % hosts.len()]).is_some() {
                hits += 1;
            }
        }
        let elapsed = start.elapsed();
        assert!(hits > 0);
        let per_second = iterations as f64 / elapsed.as_secs_f64();
        eprintln!(
            "matcher: {per_second:.0} lookups/s ({:.0} ns each)",
            1e9 / per_second
        );

        if !cfg!(debug_assertions) {
            // Generous: real throughput is millions per second. This catches an accidental
            // linear scan, not a few percent of drift.
            assert!(
                per_second > 200_000.0,
                "matcher managed only {per_second:.0} lookups/s;                  something has turned this into a scan"
            );
        }
    }

    #[test]
    fn every_bundled_pattern_matches_itself() {
        // A pattern that cannot match the host it was written for is a rule that does nothing.
        let list = domains::bundled().unwrap();
        let m = Matcher::build(&list);
        for service in &list.services {
            for pattern in &service.patterns {
                let host = match pattern {
                    domains::Pattern::Exact(h) => h.clone(),
                    domains::Pattern::Suffix(parent) => format!("probe.{parent}"),
                };
                assert!(
                    m.service_for(&host).is_some(),
                    "pattern {pattern:?} in service {} matches nothing",
                    service.name
                );
            }
        }
    }
}
