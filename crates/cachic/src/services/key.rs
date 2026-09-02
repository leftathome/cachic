//! Cache key normalisation.
//!
//! This is where functional parity with monolithic is won or lost (G1, FR-21). The same content is
//! served from many CDN hostnames with many query strings, and the whole value of a LAN cache is
//! that all of those collapse to one stored object.
//!
//! Defaults, matching monolithic:
//!
//! - **Drop the query string.** CDNs put authentication tokens and expiry timestamps there, so
//!   keeping it would make every request a miss.
//! - **Exclude the host.** This is the point of the exercise: `cdn1.epicgames.com/x` and
//!   `cdn2.epicgames.com/x` are the same bytes and must share one cached copy.
//!
//! Both are overridable per service, because a few services genuinely need them.

use regex::RegexSet;

use crate::{config::rules::ServiceRule, services::matcher::Matcher};

/// A service's key rule with its patterns compiled.
///
/// Compiled once when configuration is loaded rather than per request: a `RegexSet` build is
/// expensive and this runs on every request.
#[derive(Debug, Clone, Default)]
pub struct CompiledRule {
    pub keep_query: bool,
    pub include_host: bool,
    /// Fetch upstream over https regardless of the client's scheme.
    pub upstream_https: bool,
    include: Option<RegexSet>,
    exclude: Option<RegexSet>,
}

#[derive(Debug, thiserror::Error)]
#[error("service {service:?}: {field} pattern {pattern:?} is not a valid regex: {source}")]
pub struct CompileError {
    pub service: String,
    pub field: &'static str,
    pub pattern: String,
    #[source]
    pub source: regex::Error,
}

impl CompiledRule {
    pub fn compile(service: &str, rule: &ServiceRule) -> Result<Self, CompileError> {
        let build = |patterns: &Vec<String>,
                     field: &'static str|
         -> Result<Option<RegexSet>, CompileError> {
            if patterns.is_empty() {
                return Ok(None);
            }
            // Compile individually first so the error can name the offending pattern rather than
            // just saying the set failed.
            for pattern in patterns {
                regex::Regex::new(pattern).map_err(|source| CompileError {
                    service: service.to_owned(),
                    field,
                    pattern: pattern.clone(),
                    source,
                })?;
            }
            RegexSet::new(patterns)
                .map(Some)
                .map_err(|source| CompileError {
                    service: service.to_owned(),
                    field,
                    pattern: patterns.join(", "),
                    source,
                })
        };

        Ok(Self {
            keep_query: rule.keep_query,
            include_host: rule.include_host,
            upstream_https: rule.upstream_https,
            include: build(&rule.include_paths, "include_paths")?,
            exclude: build(&rule.exclude_paths, "exclude_paths")?,
        })
    }

    /// Whether a path is cacheable under this rule.
    ///
    /// Exclusions win over inclusions: an operator adding an exclusion is telling us not to cache
    /// something, and that should not be overridden by a broader include.
    pub fn is_cacheable(&self, path: &str) -> bool {
        if let Some(exclude) = &self.exclude {
            if exclude.is_match(path) {
                return false;
            }
        }
        match &self.include {
            Some(include) => include.is_match(path),
            None => true,
        }
    }
}

/// The normalised cache key for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// Human-readable, and what gets hashed. Kept for logging and for the admin API's purge by
    /// prefix, which needs something an operator can reason about.
    pub key: String,
    pub service: String,
}

impl CacheKey {
    /// `blake3(service || 0x00 || key)[..16]`.
    ///
    /// The service identifier is part of the hash so two services cannot collide on a path, and
    /// the separator means a service named `ab` with key `c` cannot collide with `a` and `bc`.
    pub fn object_id(&self) -> [u8; 16] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.service.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(self.key.as_bytes());
        let mut id = [0u8; 16];
        id.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        id
    }
}

/// Split a request target into path and query.
fn split_target(target: &str) -> (&str, Option<&str>) {
    match target.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (target, None),
    }
}

/// Normalise a request into a cache key.
pub fn normalise(service: &str, host: &str, target: &str, rule: &CompiledRule) -> CacheKey {
    let (path, query) = split_target(target);

    let mut key = String::with_capacity(target.len() + host.len() + 1);
    if rule.include_host {
        key.push_str(&Matcher::normalise_host(host));
    }
    // Paths are case-sensitive per RFC 3986, and Steam's content paths genuinely are. Do not
    // lowercase here; doing so would merge distinct objects.
    if !path.starts_with('/') {
        key.push('/');
    }
    key.push_str(path);
    if rule.keep_query {
        if let Some(query) = query {
            key.push('?');
            key.push_str(query);
        }
    }

    CacheKey {
        key,
        service: service.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> CompiledRule {
        CompiledRule::default()
    }

    #[test]
    fn drops_the_query_string_by_default() {
        // CDN tokens live in the query. Keeping them would make every request a miss, which is
        // the single most important default in this module.
        let a = normalise(
            "steam",
            "cdn1.example.com",
            "/depot/1.chunk?token=aaa",
            &rule(),
        );
        let b = normalise(
            "steam",
            "cdn1.example.com",
            "/depot/1.chunk?token=bbb",
            &rule(),
        );
        assert_eq!(a, b);
        assert_eq!(a.key, "/depot/1.chunk");
    }

    #[test]
    fn excludes_the_host_by_default() {
        // The reason lancache exists: the same bytes from many CDN hostnames are one object.
        let a = normalise("epicgames", "cdn1.epicgames.com", "/x/y.bin", &rule());
        let b = normalise("epicgames", "cdn2.epicgames.com", "/x/y.bin", &rule());
        assert_eq!(a.object_id(), b.object_id());
    }

    #[test]
    fn keeps_the_query_when_the_service_asks() {
        let mut r = rule();
        r.keep_query = true;
        let a = normalise("svc", "h", "/x?v=1", &r);
        let b = normalise("svc", "h", "/x?v=2", &r);
        assert_ne!(a, b);
        assert_eq!(a.key, "/x?v=1");
    }

    #[test]
    fn includes_the_host_when_the_service_asks() {
        let mut r = rule();
        r.include_host = true;
        let a = normalise("svc", "a.example.com", "/x", &r);
        let b = normalise("svc", "b.example.com", "/x", &r);
        assert_ne!(a.object_id(), b.object_id());
        assert_eq!(a.key, "a.example.com/x");
    }

    #[test]
    fn host_is_normalised_when_included() {
        let mut r = rule();
        r.include_host = true;
        let a = normalise("svc", "A.Example.com:80", "/x", &r);
        let b = normalise("svc", "a.example.com", "/x", &r);
        assert_eq!(a, b);
    }

    #[test]
    fn paths_stay_case_sensitive() {
        // RFC 3986, and Steam's depot paths rely on it. Lowercasing would merge distinct objects.
        let a = normalise("svc", "h", "/Depot/A.chunk", &rule());
        let b = normalise("svc", "h", "/depot/a.chunk", &rule());
        assert_ne!(a.object_id(), b.object_id());
    }

    #[test]
    fn different_services_never_collide_on_the_same_path() {
        let a = normalise("steam", "h", "/x", &rule());
        let b = normalise("blizzard", "h", "/x", &rule());
        assert_ne!(a.object_id(), b.object_id());
    }

    #[test]
    fn the_service_separator_prevents_boundary_collisions() {
        // Without a separator, service "ab" + key "c" would hash the same as "a" + "bc".
        let a = CacheKey {
            service: "ab".into(),
            key: "c".into(),
        };
        let b = CacheKey {
            service: "a".into(),
            key: "bc".into(),
        };
        assert_ne!(a.object_id(), b.object_id());
    }

    #[test]
    fn object_ids_are_stable() {
        // A change here silently invalidates every cache in the field, so it must be deliberate.
        let key = normalise(
            "steam",
            "lancache.steamcontent.com",
            "/depot/1.chunk",
            &rule(),
        );
        let id = key.object_id();
        assert_eq!(id, key.object_id(), "hashing is not deterministic");
        assert_eq!(id.len(), 16);
    }

    #[test]
    fn include_and_exclude_patterns_gate_cacheability() {
        let compiled = CompiledRule::compile(
            "svc",
            &ServiceRule {
                include_paths: vec![r"^/depot/".into()],
                exclude_paths: vec![r"\.manifest$".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(compiled.is_cacheable("/depot/1.chunk"));
        assert!(
            !compiled.is_cacheable("/other/1.chunk"),
            "include did not gate"
        );
        // Exclusion wins even though the include matches.
        assert!(
            !compiled.is_cacheable("/depot/1.manifest"),
            "exclude did not win"
        );
    }

    #[test]
    fn no_patterns_means_everything_is_cacheable() {
        assert!(rule().is_cacheable("/anything"));
    }

    #[test]
    fn an_invalid_regex_names_the_service_and_the_pattern() {
        let err = CompiledRule::compile(
            "steam",
            &ServiceRule {
                exclude_paths: vec!["([unclosed".into()],
                ..Default::default()
            },
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("steam"), "{text}");
        assert!(text.contains("([unclosed"), "{text}");
        assert!(text.contains("exclude_paths"), "{text}");
    }

    #[test]
    fn targets_without_a_leading_slash_are_normalised() {
        let a = normalise("svc", "h", "depot/x", &rule());
        assert_eq!(a.key, "/depot/x");
    }

    #[test]
    fn an_empty_query_is_not_the_same_as_no_query_when_kept() {
        let mut r = rule();
        r.keep_query = true;
        assert_eq!(normalise("s", "h", "/x?", &r).key, "/x?");
        assert_eq!(normalise("s", "h", "/x", &r).key, "/x");
    }
}
