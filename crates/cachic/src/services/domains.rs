//! The `uklans/cache-domains` list.
//!
//! Format: `cache_domains.json` names each service and the files listing its hostnames; each of
//! those files holds one hostname per line, `#` for comments, and `*.` for a wildcard covering any
//! subdomain.
//!
//! Parsing is strict about structure and forgiving about whitespace, because this file is
//! community-maintained and arrives over the network on a refresh (FR-61). A malformed refresh
//! must be rejected in one piece rather than partially applied - a half-loaded domain list is a
//! cache that silently stops caching half the services it used to.
//!
//! This is a fuzz target in TASK-21.

use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum DomainsError {
    #[error("cache_domains.json is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("service {service:?} lists domain file {file:?}, which is missing")]
    MissingFile { service: String, file: String },
    #[error("{file}:{line}: {reason}")]
    Malformed {
        file: String,
        line: usize,
        reason: String,
    },
    #[error("the domain list is empty, which would disable caching entirely")]
    Empty,
}

/// One hostname pattern from a domain file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pattern {
    /// `cdn.blizzard.com` - this host and nothing else.
    Exact(String),
    /// `*.cdn.blizzard.com` - any subdomain. Stored without the leading `*.`.
    ///
    /// Note this does *not* match the bare parent: `cache-domains` lists
    /// `*.cdn.blizzard.com` and `cdn.blizzard.com` separately, so treating the wildcard as
    /// covering the parent would silently diverge from monolithic.
    Suffix(String),
}

impl Pattern {
    fn parse(raw: &str) -> Option<Self> {
        let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return None;
        }
        Some(match host.strip_prefix("*.") {
            Some(rest) if !rest.is_empty() => Pattern::Suffix(rest.to_owned()),
            _ => Pattern::Exact(host),
        })
    }
}

/// A service and the hostnames that belong to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub description: String,
    pub patterns: Vec<Pattern>,
}

/// The whole list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainList {
    pub services: Vec<Service>,
}

#[derive(serde::Deserialize)]
struct RawIndex {
    cache_domains: Vec<RawService>,
}

#[derive(serde::Deserialize)]
struct RawService {
    name: String,
    #[serde(default)]
    description: String,
    domain_files: Vec<String>,
}

impl DomainList {
    /// Parse an index plus the contents of its domain files, keyed by file name.
    ///
    /// Taking the files as a map rather than reading them keeps this pure and therefore fuzzable,
    /// and lets the bundled snapshot and a network refresh share one code path.
    pub fn parse(index_json: &str, files: &BTreeMap<String, String>) -> Result<Self, DomainsError> {
        let index: RawIndex = serde_json::from_str(index_json)?;
        let mut services = Vec::with_capacity(index.cache_domains.len());

        for raw in index.cache_domains {
            let mut patterns = Vec::new();
            for file in &raw.domain_files {
                let body = files.get(file).ok_or_else(|| DomainsError::MissingFile {
                    service: raw.name.clone(),
                    file: file.clone(),
                })?;
                for (index, line) in body.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if line.split_whitespace().count() > 1 {
                        return Err(DomainsError::Malformed {
                            file: file.clone(),
                            line: index + 1,
                            reason: format!("expected one hostname per line, found {line:?}"),
                        });
                    }
                    match Pattern::parse(line) {
                        Some(p) => patterns.push(p),
                        None => {
                            return Err(DomainsError::Malformed {
                                file: file.clone(),
                                line: index + 1,
                                reason: format!("{line:?} is not a hostname"),
                            })
                        }
                    }
                }
            }
            patterns.sort();
            patterns.dedup();
            services.push(Service {
                name: raw.name,
                description: raw.description,
                patterns,
            });
        }

        let list = Self { services };
        if list.pattern_count() == 0 {
            return Err(DomainsError::Empty);
        }
        Ok(list)
    }

    /// Load from a directory laid out like the upstream repository.
    pub fn load_dir(
        dir: &std::path::Path,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let index = std::fs::read_to_string(dir.join("cache_domains.json"))?;
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".txt") {
                files.insert(name, std::fs::read_to_string(entry.path())?);
            }
        }
        Ok(Self::parse(&index, &files)?)
    }

    pub fn pattern_count(&self) -> usize {
        self.services.iter().map(|s| s.patterns.len()).sum()
    }
}

/// The snapshot bundled at build time (FR-61), so a fresh install caches immediately and an
/// air-gapped install works at all.
pub fn bundled() -> Result<DomainList, DomainsError> {
    let index = include_str!("../../testdata/cache-domains/cache_domains.json");
    let mut files = BTreeMap::new();
    for (name, body) in BUNDLED_FILES {
        files.insert((*name).to_string(), (*body).to_string());
    }
    DomainList::parse(index, &files)
}

macro_rules! bundled_files {
    ($($name:literal),* $(,)?) => {
        const BUNDLED_FILES: &[(&str, &str)] = &[
            $(($name, include_str!(concat!("../../testdata/cache-domains/", $name))),)*
        ];
    };
}

bundled_files!(
    "arenanet.txt",
    "blizzard.txt",
    "bsg.txt",
    "cityofheroes.txt",
    "cod.txt",
    "daybreak.txt",
    "epicgames.txt",
    "frontier.txt",
    "neverwinter.txt",
    "nexusmods.txt",
    "nintendo.txt",
    "origin.txt",
    "pathofexile.txt",
    "renegadex.txt",
    "riot.txt",
    "rockstar.txt",
    "sony.txt",
    "square.txt",
    "steam.txt",
    "teso.txt",
    "test.txt",
    "uplay.txt",
    "warframe.txt",
    "wargaming.net.txt",
    "windowsupdates.txt",
    "xboxlive.txt",
);

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_bundled_snapshot_parses() {
        let list = bundled().unwrap();
        assert_eq!(list.services.len(), 26, "service count changed");
        assert!(
            list.pattern_count() > 100,
            "only {} patterns; the snapshot looks truncated",
            list.pattern_count()
        );
        let steam = list.services.iter().find(|s| s.name == "steam").unwrap();
        assert!(steam
            .patterns
            .contains(&Pattern::Exact("lancache.steamcontent.com".into())));
    }

    #[test]
    fn the_bundled_snapshot_contains_the_headline_services() {
        // G1 is parity for every service in cache-domains; these are the ones users notice.
        let list = bundled().unwrap();
        let names: Vec<_> = list.services.iter().map(|s| s.name.as_str()).collect();
        for expected in [
            "steam",
            "blizzard",
            "epicgames",
            "riot",
            // Named for the service, not its file: the hostnames live in windowsupdates.txt.
            "wsus",
            "xboxlive",
            "sony",
            "nintendo",
        ] {
            assert!(names.contains(&expected), "missing service {expected}");
        }
    }

    #[test]
    fn parses_wildcards_and_exact_hosts() {
        let list = DomainList::parse(
            r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
            &files(&[("x.txt", "*.cdn.example.com\ncdn.example.com\n")]),
        )
        .unwrap();
        let p = &list.services[0].patterns;
        assert!(p.contains(&Pattern::Suffix("cdn.example.com".into())));
        assert!(p.contains(&Pattern::Exact("cdn.example.com".into())));
    }

    #[test]
    fn ignores_comments_blank_lines_and_case() {
        let list = DomainList::parse(
            r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
            &files(&[("x.txt", "# a comment\n\n  CDN.Example.COM  \n\n")]),
        )
        .unwrap();
        assert_eq!(
            list.services[0].patterns,
            vec![Pattern::Exact("cdn.example.com".into())]
        );
    }

    #[test]
    fn deduplicates_repeated_hosts() {
        let list = DomainList::parse(
            r#"{"cache_domains":[{"name":"x","domain_files":["a.txt","b.txt"]}]}"#,
            &files(&[
                ("a.txt", "cdn.example.com\n"),
                ("b.txt", "cdn.example.com\n"),
            ]),
        )
        .unwrap();
        assert_eq!(list.services[0].patterns.len(), 1);
    }

    #[test]
    fn rejects_a_missing_domain_file_naming_the_service() {
        let err = DomainList::parse(
            r#"{"cache_domains":[{"name":"steam","domain_files":["gone.txt"]}]}"#,
            &files(&[]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("steam"), "{err}");
        assert!(err.to_string().contains("gone.txt"), "{err}");
    }

    #[test]
    fn rejects_a_line_with_multiple_tokens() {
        let err = DomainList::parse(
            r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
            &files(&[("x.txt", "cdn.example.com extra\n")]),
        )
        .unwrap_err();
        match err {
            DomainsError::Malformed { line, .. } => assert_eq!(line, 1),
            other => panic!("expected Malformed, got {other}"),
        }
    }

    #[test]
    fn rejects_an_empty_list_rather_than_disabling_caching() {
        // A refresh that yields nothing must not be applied; it would silently stop the cache
        // caching anything at all.
        let err = DomainList::parse(
            r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
            &files(&[("x.txt", "# nothing but comments\n")]),
        )
        .unwrap_err();
        assert!(matches!(err, DomainsError::Empty));
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(DomainList::parse("{not json", &files(&[])).is_err());
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        let mut seed = 0xfeed_face_dead_beefu64;
        for _ in 0..5_000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = (seed % 40) as usize;
            let body: String = (0..len)
                .map(|i| {
                    let b = ((seed >> (i % 8 * 8)) & 0xff) as u8;
                    match b % 6 {
                        0 => '.',
                        1 => '*',
                        2 => '#',
                        3 => '\n',
                        4 => ' ',
                        _ => (b'a' + b % 26) as char,
                    }
                })
                .collect();
            let _ = DomainList::parse(
                r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
                &files(&[("x.txt", &body)]),
            );
        }
    }
}
